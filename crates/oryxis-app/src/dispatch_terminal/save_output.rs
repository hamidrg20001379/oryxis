//! "Save Output As...": a pane's whole buffer, to the clipboard or a file.
//!
//! The text is read when the menu row is clicked and parked on the app,
//! not when a destination is picked. The save dialog can stay up for as
//! long as the user browses folders, and the session keeps printing the
//! whole time; what was asked for is the output as it stood at the click.
//! Parked on `Oryxis` rather than carried by a message, because a buffer
//! can hold anything a remote host printed and messages derive `Debug`.

use iced::Task;
use uuid::Uuid;

use super::*;

/// Where the parked output goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaveOutputDestination {
    Clipboard,
    File,
}

/// A buffer waiting for the user to say where it goes
/// (`Modal::TerminalSaveOutput` while `Some`).
pub(crate) struct SaveOutputCard {
    /// The pane's label, run through Privacy Mode's redactor: the dialog
    /// names it and the suggested file name is built from it.
    pub host_label: String,
    /// Scrollback plus screen, as read at the click.
    pub text: String,
    /// Lines in `text`, for the dialog.
    pub lines: usize,
}

impl Oryxis {
    /// Read `pane_id`'s buffer and ask where it goes. An empty buffer
    /// asks nothing, like Copy Screen on an empty viewport.
    pub(crate) fn open_save_output(&mut self, pane_id: Uuid) -> Task<Message> {
        self.overlay = None;
        let Some(pane) = self.pane_by_id(pane_id) else {
            return Task::none();
        };
        // The lock ends with the read: the dialog and everything after it
        // work on the copy.
        let text = match pane.terminal.lock() {
            Ok(state) => state.all_text(),
            Err(_) => return Task::none(),
        };
        if text.is_empty() {
            return Task::none();
        }
        let label = pane.label.clone();
        let host_label = self.privacy_display_label(&label, &label, &self.privacy_terms());
        self.save_output = Some(SaveOutputCard {
            host_label,
            lines: text.lines().count(),
            text,
        });
        Task::none()
    }

    /// Send the parked buffer where the user picked. The dialog closes
    /// either way: a file save asks its own question next.
    pub(crate) fn resolve_save_output(&mut self, to: SaveOutputDestination) -> Task<Message> {
        let Some(card) = self.save_output.take() else {
            return Task::none();
        };
        match to {
            SaveOutputDestination::Clipboard => self.update(Message::CopyToClipboard(card.text)),
            SaveOutputDestination::File => {
                let default_name = output_file_name(&card.host_label, chrono::Local::now());
                Task::perform(
                    tokio::task::spawn_blocking(move || {
                        let path = rfd::FileDialog::new()
                            .set_title(crate::i18n::t("terminal_save_output_dialog_title"))
                            .set_file_name(&default_name)
                            .add_filter("Text", &["txt"])
                            .save_file()?;
                        Some(
                            std::fs::write(&path, card.text)
                                .map(|_| path.display().to_string())
                                .map_err(|e| e.to_string()),
                        )
                    }),
                    |res| match res {
                        Ok(Some(outcome)) => {
                            Message::Terminal(TerminalMessage::TerminalSaveOutputWritten(outcome))
                        }
                        // Dialog dismissed or the blocking task died:
                        // nothing to report.
                        _ => Message::NoOp,
                    },
                )
            }
        }
    }

    /// Toast the file write's outcome.
    pub(crate) fn save_output_written(&mut self, outcome: Result<String, String>) -> Task<Message> {
        match outcome {
            Ok(path) => self.show_toast(crate::i18n::t("terminal_save_output_done").replace("{path}", &path)),
            Err(e) => self.show_toast(crate::i18n::t("terminal_save_output_failed").replace("{error}", &e)),
        }
    }
}

/// The name the save dialog suggests: the pane's label and the moment of
/// the click, so two saves of one session never offer the same name.
fn output_file_name(host_label: &str, at: chrono::DateTime<chrono::Local>) -> String {
    format!(
        "{}-{}.txt",
        crate::util::sanitize_file_stem(host_label),
        at.format("%Y%m%d-%H%M%S"),
    )
}

#[cfg(test)]
mod tests {
    use super::output_file_name;
    use chrono::TimeZone;

    #[test]
    fn file_name_carries_the_label_and_the_moment() {
        let at = chrono::Local.with_ymd_and_hms(2026, 10, 13, 9, 5, 7).unwrap();
        assert_eq!(output_file_name("admin@db-01", at), "admin_db-01-20261013-090507.txt");
    }

    #[test]
    fn file_name_never_starts_with_a_dash() {
        let at = chrono::Local.with_ymd_and_hms(2026, 10, 13, 9, 5, 7).unwrap();
        assert!(!output_file_name("-rf", at).starts_with('-'));
    }
}
