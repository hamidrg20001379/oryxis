//! The editable path header and its recent-directories dropdown.
//!
//! Committing a typed path is not a plain navigation: it resolves
//! `~`, rejects what does not exist, and keeps the edit open when
//! the target is unreachable so the typed text is not lost.

use super::*;

impl Oryxis {
    pub(super) fn handle_sidebar_files_path_bar(
        &mut self,
        message: SidebarFilesMessage,
    ) -> Task<Message> {
        match message {
            SidebarFilesMessage::SidebarFilesStartEditPath => {
                let Some(pane) = self.active_pane_mut() else {
                    return Task::none();
                };
                if pane.files.client.is_none() {
                    return Task::none();
                }
                pane.files.path_editing = Some(pane.files.path.clone());
                // Drop the keyboard straight into the input (mirrors the
                // SFTP pane's path editing).
                return crate::widgets::focus_input(iced::widget::Id::new(
                    "sidebar-files-path",
                ));
            }
            SidebarFilesMessage::SidebarFilesEditPath(s) => {
                if let Some(pane) = self.active_pane_mut()
                    && pane.files.path_editing.is_some()
                {
                    pane.files.path_editing = Some(s);
                }
            }
            SidebarFilesMessage::SidebarFilesEditBlur => {
                if let Some(pane) = self.active_pane_mut() {
                    let files = &mut pane.files;
                    files.path_editing = None;
                    files.rename = None;
                    files.new_entry = None;
                    files.path_history_open = false;
                    files.selected.clear();
                    files.selection_anchor = None;
                    files.click_tracker.reset();
                    files.parent_selected = false;
                }
            }
            SidebarFilesMessage::SidebarFilesPathHistoryToggle => {
                if let Some(pane) = self.active_pane_mut() {
                    pane.files.path_history_open = !pane.files.path_history_open;
                }
            }
            SidebarFilesMessage::SidebarFilesPathHistoryClose => {
                if let Some(pane) = self.active_pane_mut() {
                    pane.files.path_history_open = false;
                }
            }
            SidebarFilesMessage::SidebarFilesPathHistoryPick(path) => {
                if let Some(pane) = self.active_pane_mut() {
                    pane.files.path_history_open = false;
                }
                // Navigate also closes a pending path edit, so picking
                // from the dropdown mid-edit leaves a clean header.
                return Task::done(Message::SidebarFiles(
                    SidebarFilesMessage::SidebarFilesNavigate(path),
                ));
            }
            SidebarFilesMessage::SidebarFilesCommitPath => {
                let Some(pane) = self.active_pane_mut() else {
                    return Task::none();
                };
                let Some(input) = pane.files.path_editing.take() else {
                    return Task::none();
                };
                let input = input.trim().to_string();
                if input.is_empty() {
                    return Task::none();
                }
                // `~`-relative input expands against the session home;
                // anything else goes to the server's canonicalize (which
                // resolves relative segments) before listing.
                let target = expand_cwd(&input, pane.files.home.as_deref())
                    .unwrap_or(input);
                let Some(client) = pane.files.client.clone() else {
                    return Task::none();
                };
                // Typing a path away from the shell's cwd unpins follow,
                // same rule as clicking into a folder.
                if pane.files.follow() {
                    pane.files.follow_disabled = true;
                }
                let pane_id = pane.id;
                // Same optimistic adoption as SidebarFilesNavigate; the
                // canonicalized path from the listing replaces this
                // best-effort value when it lands.
                pane.files.path = target.clone();
                pane.files.entries.clear();
                pane.files.loading = true;
                pane.files.error = None;
                let seq = pane.files.next_req();
                return Task::perform(
                    async move {
                        let path = client
                            .canonicalize(&target)
                            .await
                            .map_err(|e| e.to_string())?;
                        let entries =
                            client.list_dir(&path).await.map_err(|e| e.to_string())?;
                        Ok::<_, String>((path, entries))
                    },
                    move |result| match result {
                        Ok((path, entries)) => {
                            Message::SidebarFiles(SidebarFilesMessage::SidebarFilesListed(pane_id, seq, path, entries))
                        }
                        Err(e) => Message::SidebarFiles(SidebarFilesMessage::SidebarFilesError(pane_id, seq, e)),
                    },
                );
            }
            // The parent routed us here, so anything else is a
            // grouping mistake, not a runtime case.
            m => return crate::dispatch::unrouted(m),
        }
        Task::none()
    }
}
