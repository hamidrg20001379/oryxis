//! The last command a shell ran, and what it printed.
//!
//! Shell integration (OSC 133) marks where the command line begins (`B`),
//! where its output begins (`C`) and where it ended (`D`, or the next
//! prompt's `A`). The span between `C` and the end is the output, exactly:
//! no prompt heuristics, no guessing where the next prompt starts.
//!
//! The marks are turned into rows the moment the emulator reaches them,
//! and held with the terminal's own row anchors
//! (`alacritty_terminal::term::anchor`), never as line numbers: a line
//! number stops naming the row at the next scroll, and once the
//! scrollback is full it names whatever rotated into its place. An anchor
//! follows its row, and when that row is gone (past the end of the
//! history, a cleared history, a width change) the span reports it gone
//! instead of reading other text.

use alacritty_terminal::index::Line;
use alacritty_terminal::term::anchor::RowAnchor;
use alacritty_terminal::term::Term;

use crate::osc::ShellMark;

/// A held position: the row and the column the mark was on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Held {
    row: RowAnchor,
    col: u16,
}

/// One command's span.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Span {
    /// Where the command line began (`B`), when the shell marked it.
    command: Option<Held>,
    /// The command line as the shell reported it (`OSC 633 ; E`).
    command_text: Option<String>,
    /// Where the output began (`C`).
    output: Held,
    /// Where it ended (`D`, or the next prompt's `A`). `None` while the
    /// command is still running.
    end: Option<Held>,
}

/// The marks seen so far, kept as held rows.
#[derive(Debug, Default)]
pub(crate) struct CommandSpans {
    /// The command line being typed (`B` seen, no `C` yet).
    pending: Option<Held>,
    /// Its text, when the shell reported one.
    pending_text: Option<String>,
    /// The latest command whose output began.
    last: Option<Span>,
}

impl CommandSpans {
    /// A mark the emulator just reached at `(line, col)` of the primary
    /// screen. `text` is the command line an `E` mark carries.
    pub(crate) fn on_mark<T>(
        &mut self,
        term: &mut Term<T>,
        mark: ShellMark,
        line: Line,
        col: u16,
        text: Option<String>,
    ) {
        match mark {
            ShellMark::PromptStart => {
                self.close(term, line, col);
                self.drop_pending(term);
            }
            ShellMark::PromptEnd => {
                self.drop_pending(term);
                self.pending = term.hold_row(line).map(|row| Held { row, col });
            }
            ShellMark::CommandLine(_) => {
                self.pending_text = text;
            }
            ShellMark::OutputStart => {
                // On the alternate screen (a program inside tmux, say) the
                // rows are not the primary's and cannot be held.
                let Some(row) = term.hold_row(line) else {
                    self.drop_pending(term);
                    return;
                };
                let span = Span {
                    command: self.pending.take(),
                    command_text: self.pending_text.take(),
                    output: Held { row, col },
                    end: None,
                };
                if let Some(old) = self.last.replace(span) {
                    release_span(term, old);
                }
            }
            ShellMark::CommandEnd(_) => self.close(term, line, col),
        }
    }

    /// End the running command at `(line, col)`, if one is running.
    fn close<T>(&mut self, term: &mut Term<T>, line: Line, col: u16) {
        if let Some(span) = self.last.as_mut().filter(|s| s.end.is_none()) {
            span.end = term.hold_row(line).map(|row| Held { row, col });
        }
    }

    fn drop_pending<T>(&mut self, term: &mut Term<T>) {
        if let Some(held) = self.pending.take() {
            term.release_row(held.row);
        }
        self.pending_text = None;
    }
}

fn release_span<T>(term: &mut Term<T>, span: Span) {
    for held in [span.command, Some(span.output), span.end].into_iter().flatten() {
        term.release_row(held.row);
    }
}

/// What the last command was and what it printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastCommand {
    /// The command line, when it can be told (the shell's own report, else
    /// the row the shell marked as the command line).
    pub command: Option<String>,
    /// Its output, whole, trailing blank lines dropped.
    pub output: String,
}

impl LastCommand {
    /// The command line, then the output, the way it read on screen.
    pub fn to_text(&self) -> String {
        match (&self.command, self.output.is_empty()) {
            (Some(cmd), true) => cmd.clone(),
            (Some(cmd), false) => format!("{cmd}\n{}", self.output),
            (None, _) => self.output.clone(),
        }
    }
}

/// Why there is nothing to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastCommandUnavailable {
    /// No command was marked on this pane: its shell does not report
    /// OSC 133, or nothing ran yet.
    NoneSeen,
    /// A full-screen program is up; its screen is not the shell's.
    AlternateScreen,
    /// The output is no longer in the terminal (it scrolled past the end
    /// of the history, the history was cleared, or a width change
    /// reflowed it).
    Gone,
}

impl crate::widget::TerminalState {
    /// The last command this pane's shell ran, with its output: what
    /// "Copy Last Command + Output" copies.
    pub fn last_command(&self) -> Result<LastCommand, LastCommandUnavailable> {
        use alacritty_terminal::grid::Dimensions;
        use alacritty_terminal::term::TermMode;

        let span = self.backend.spans.last.as_ref().ok_or(LastCommandUnavailable::NoneSeen)?;
        let term = &self.backend.term;
        if term.mode().contains(TermMode::ALT_SCREEN) {
            return Err(LastCommandUnavailable::AlternateScreen);
        }
        let row = |held: Held| term.held_row(held.row).map(|line| (line, held.col));
        let start = row(span.output).ok_or(LastCommandUnavailable::Gone)?;
        let end = match span.end {
            Some(held) => row(held).ok_or(LastCommandUnavailable::Gone)?,
            // Still running: what it printed so far, up to the cursor.
            None => {
                let cursor = term.grid().cursor.point;
                (cursor.line, cursor.column.0 as u16)
            }
        };
        let output = text_between(self, start, end, term.grid().columns());
        let command = span.command_text.clone().or_else(|| {
            let (line, col) = row(span.command?)?;
            let abs = term.grid().history_size() as i64 + i64::from(line.0);
            self.logical_line_from_abs(abs, col).map(|t| t.trim().to_string())
        });
        Ok(LastCommand { command: command.filter(|c| !c.is_empty()), output })
    }
}

/// The text from `start` (inclusive) to `end` (exclusive), the way a
/// selection of it would copy, trailing blank lines dropped.
fn text_between(
    state: &crate::widget::TerminalState,
    start: (Line, u16),
    end: (Line, u16),
    columns: usize,
) -> String {
    // The last cell before `end`: the end of the row above when `end`
    // sits at a row's first column (the usual case: the next prompt
    // starts a fresh row).
    let last = if end.1 > 0 {
        (end.0, end.1 - 1)
    } else {
        (end.0 - 1, columns.saturating_sub(1) as u16)
    };
    if (last.0, last.1) < (start.0, start.1) {
        return String::new();
    }
    let sel = crate::widget::Selection {
        start: (start.1, start.0.0),
        end: (last.1, last.0.0),
        block: false,
    };
    state.get_selection_text(&sel).trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::TerminalState;

    const A: &str = "\x1b]133;A\x07";
    const B: &str = "\x1b]133;B\x07";
    const C: &str = "\x1b]133;C\x07";
    const D: &str = "\x1b]133;D;0\x07";

    /// One integrated round: prompt, typed command, its output, the end.
    fn run(term: &mut TerminalState, command: &str, output: &str) {
        term.process(format!("{A}$ {B}{command}\r\n{C}{output}{D}").as_bytes());
    }

    #[test]
    fn nothing_marked_is_nothing_to_copy() {
        let mut term = TerminalState::new_no_pty(40, 10).unwrap();
        term.process(b"$ ls\r\nfile\r\n$ ");
        assert_eq!(term.last_command(), Err(LastCommandUnavailable::NoneSeen));
    }

    #[test]
    fn the_command_and_its_output_without_the_next_prompt() {
        let mut term = TerminalState::new_no_pty(40, 10).unwrap();
        run(&mut term, "echo hi", "hi\r\n");
        term.process(format!("{A}$ {B}").as_bytes());
        let last = term.last_command().unwrap();
        assert_eq!(last.command.as_deref(), Some("echo hi"));
        assert_eq!(last.output, "hi");
        assert_eq!(last.to_text(), "echo hi\nhi");
    }

    #[test]
    fn a_command_with_no_output_copies_just_the_command() {
        let mut term = TerminalState::new_no_pty(40, 10).unwrap();
        run(&mut term, "cd /tmp", "");
        term.process(format!("{A}$ {B}").as_bytes());
        let last = term.last_command().unwrap();
        assert_eq!(last.output, "");
        assert_eq!(last.to_text(), "cd /tmp");
    }

    #[test]
    fn the_latest_command_wins() {
        let mut term = TerminalState::new_no_pty(40, 10).unwrap();
        run(&mut term, "echo one", "one\r\n");
        run(&mut term, "echo two", "two\r\n");
        assert_eq!(term.last_command().unwrap().to_text(), "echo two\ntwo");
    }

    #[test]
    fn a_long_output_comes_back_whole() {
        let mut term = TerminalState::new_no_pty(40, 10).unwrap();
        let output: String = (1..=500).map(|i| format!("{i}\r\n")).collect();
        run(&mut term, "seq 500", &output);
        let last = term.last_command().unwrap();
        let lines: Vec<&str> = last.output.lines().collect();
        assert_eq!(lines.len(), 500);
        assert_eq!(lines[0], "1");
        assert_eq!(lines[499], "500");
        assert!(lines.iter().enumerate().all(|(i, l)| *l == (i + 1).to_string()));
    }

    #[test]
    fn a_running_command_copies_what_it_printed_so_far() {
        let mut term = TerminalState::new_no_pty(40, 10).unwrap();
        term.process(format!("{A}$ {B}tail -f log\r\n{C}first\r\nsecond\r\n").as_bytes());
        assert_eq!(term.last_command().unwrap().output, "first\nsecond");
    }

    #[test]
    fn output_pushed_past_a_full_history_is_reported_gone() {
        // A history of five rows makes the rotation reachable in a test.
        let mut term = TerminalState::new_no_pty_with_scrollback(40, 4, 5).unwrap();
        run(&mut term, "echo old", "old\r\n");
        // Ten more rows: past a history of five, so "old" is gone, and
        // the rows now where it was belong to other text.
        for i in 0..10 {
            term.process(format!("filler {i}\r\n").as_bytes());
        }
        assert_eq!(term.last_command(), Err(LastCommandUnavailable::Gone));
    }

    #[test]
    fn output_still_in_a_full_history_reads_its_own_text() {
        let mut term = TerminalState::new_no_pty_with_scrollback(40, 4, 5).unwrap();
        for i in 0..20 {
            term.process(format!("before {i}\r\n").as_bytes());
        }
        run(&mut term, "echo kept", "kept\r\n");
        // The history is full and keeps rotating; the span moves with it.
        term.process(format!("{A}$ {B}").as_bytes());
        term.process(b"\r\nmore\r\n");
        assert_eq!(term.last_command().unwrap().to_text(), "echo kept\nkept");
    }

    #[test]
    fn a_full_screen_program_is_not_read_as_the_shell() {
        let mut term = TerminalState::new_no_pty(40, 10).unwrap();
        run(&mut term, "echo hi", "hi\r\n");
        term.process(b"\x1b[?1049h");
        assert_eq!(term.last_command(), Err(LastCommandUnavailable::AlternateScreen));
        term.process(b"\x1b[?1049l");
        assert_eq!(term.last_command().unwrap().output, "hi");
    }
}
