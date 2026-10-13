//! Core SFTP-pane handler slices: navigation, filtering, property
//! edits, row interactions, drag-and-drop, edit-in-place. The single
//! biggest domain in the dispatch table.
//!
//! Pane operations are side-addressed: a `SftpPaneSide` (Left / Right)
//! names which pane, and the handler branches on `pane(side).is_remote`
//! to choose filesystem vs SFTP behaviour, so either pane can be Local
//! or a remote host.
//!
//! Routing lives in `handle_sftp_domain` below, an exhaustive
//! per-variant match that sends every `SftpMessage` straight to the
//! slice that owns it (the `dispatch_sftp_transfers` / `_files` /
//! `_archive` sibling files, plus this module's submodules):
//!
//! - `hosts`    : mount / connect / session-reuse / remount / retry,
//!   plus the host picker.
//! - `tabs`     : SFTP tab lifecycle (select / close / pin / menu).
//! - `listing`  : navigation + listings (remote / local / up), the
//!   path-bar edit flow, sort / filter / hidden toggles.
//! - `layout`   : pane menus, column toggles / resize / auto-fit,
//!   split and log resizes.
//! - `entries`  : rename / delete / new-entry flows.
//! - `selection`: row clicks / selection, drag arming, type-ahead and
//!   the SFTP keyboard handling.

#![allow(clippy::result_large_err)]

mod entries;
mod hosts;
mod layout;
mod listing;
mod selection;
mod tabs;

use iced::Task;

use crate::app::{SftpMessage, Message, Oryxis};
use crate::sftp_helpers::parent_path;

/// First listing of a freshly mounted SFTP client: try the caller's
/// preferred directory (the sidebar Files promotion, a saved path)
/// first, falling back to the home directory when it doesn't resolve
/// or list (deleted, no permission), so a stale hint degrades to the
/// normal mount instead of an error.
pub(crate) async fn initial_remote_listing(
    client: &oryxis_ssh::SftpClient,
    hint: Option<String>,
) -> Result<(String, Vec<oryxis_ssh::SftpEntry>), String> {
    if let Some(h) = hint
        && let Ok(path) = client.canonicalize(&h).await
        && let Ok(entries) = client.list_dir(&path).await
    {
        return Ok((path, entries));
    }
    let path = client.canonicalize(".").await.unwrap_or_else(|_| "/".to_string());
    let entries = client.list_dir(&path).await.map_err(|e| e.to_string())?;
    Ok((path, entries))
}

impl Oryxis {
    /// Apply the in-progress inline rename (Enter, or a click outside the
    /// input). Logs on success; a remote rename runs async and re-lists the
    /// directory via `SftpRenamed`. No-op when nothing is being renamed or
    /// the new name is blank. Does not touch `swallow_next_activate` (the
    /// keyboard-commit path sets that itself).
    fn commit_rename(&mut self) -> Task<Message> {
        let Some(rn) = self.sftp.rename.take() else {
            return Task::none();
        };
        let new_name = rn.input.trim().to_string();
        // One plain path component (rejects empty, ".", ".." and
        // separators): "." / ".." would rename onto the directory itself
        // or its parent, a separator would silently relocate the entry.
        if !crate::sftp_helpers::is_safe_remote_entry_name(&new_name) {
            return Task::none();
        }
        // Unchanged name: close the editor silently. The commit also fires
        // on any click outside the input, and a remote SSH_FXP_RENAME onto
        // its own path fails with SSH_FX_FAILURE (the target exists), so
        // without this a no-op edit surfaces a spurious "Failure" error.
        let unchanged = if self.cur_sftp().pane(rn.side).is_remote {
            rn.original_path.rsplit('/').next() == Some(new_name.as_str())
        } else {
            std::path::Path::new(&rn.original_path)
                .file_name()
                .is_some_and(|n| n == std::ffi::OsStr::new(&new_name))
        };
        if unchanged {
            return Task::none();
        }
        if !self.cur_sftp().pane(rn.side).is_remote {
            let original = std::path::PathBuf::from(&rn.original_path);
            let Some(parent) = original.parent().map(|p| p.to_path_buf()) else {
                self.sftp.pane_mut(rn.side).error = Some("Cannot rename root".into());
                return Task::none();
            };
            let dest = parent.join(&new_name);
            match std::fs::rename(&original, &dest) {
                Ok(()) => self.push_sftp_log(
                    crate::state::SftpLogLevel::Ok,
                    format!("{} {}", crate::i18n::t("sftp_log_renamed"), new_name),
                ),
                Err(e) => self.sftp.pane_mut(rn.side).error = Some(e.to_string()),
            }
            self.refresh_sftp_local(rn.side);
            Task::none()
        } else {
            let Some(client) = self.cur_sftp().pane(rn.side).client.clone() else {
                return Task::none();
            };
            let parent = parent_path(&rn.original_path);
            let dest = if parent == "/" {
                format!("/{}", new_name)
            } else {
                format!("{}/{}", parent.trim_end_matches('/'), new_name)
            };
            let from = rn.original_path;
            let side = rn.side;
            let reload_path = self.cur_sftp().pane(side).remote_path.clone();
            Task::perform(
                async move { client.rename(&from, &dest).await.map_err(|e| e.to_string()) },
                move |result| match result {
                    Ok(()) => Message::Sftp(SftpMessage::SftpRenamed(side, reload_path.clone(), new_name.clone())),
                    Err(e) => Message::Sftp(SftpMessage::SftpOpResult(side, e, true)),
                },
            )
        }
    }

    /// Entry point for the SFTP domain: routes every variant straight to
    /// the sub-handler slice that owns it (transfers / files / archive /
    /// hosts / tabs / listing / layout / entries / selection). Exhaustive
    /// on purpose: a new `SftpMessage` variant fails to compile until it
    /// is listed in its owner's group, so it can never be silently
    /// dropped. `route_sftp_async` re-dispatches through
    /// `dispatch_message`, which lands here.
    pub(crate) fn handle_sftp_domain(&mut self, message: SftpMessage) -> Task<Message> {
        match message {
            // Transfers legitimately decline every message when no SFTP tab
            // owns the continuation (tab closed mid-transfer): quiet drop,
            // matching the old chain's fall-through.
            m @ (SftpMessage::SftpToggleTransferPanel
            | SftpMessage::SftpTransferTick
            | SftpMessage::SftpDownload(..)
            | SftpMessage::SftpDownloadTo(..)
            | SftpMessage::SftpDownloadDestPicked(..)
            | SftpMessage::SftpDuplicate(..)
            | SftpMessage::SftpFileHovered
            | SftpMessage::SftpFilesHoveredLeft
            | SftpMessage::SftpFileDropped(..)
            | SftpMessage::SftpDropFlush
            | SftpMessage::SftpUploadFolder(..)
            | SftpMessage::SftpDownloadFolder(..)
            | SftpMessage::SftpDuplicateFolder(..)
            | SftpMessage::SftpResolveOverwrite(..)
            | SftpMessage::SftpToggleApplyToAll
            | SftpMessage::SftpUploadBatch(..)
            | SftpMessage::SftpDownloadBatch(..)
            | SftpMessage::SftpUploadSelection
            | SftpMessage::SftpDownloadSelection
            | SftpMessage::SftpDuplicateSelection
            | SftpMessage::SftpTransferConflict(..)
            | SftpMessage::SftpTransferQueueReady(..)
            | SftpMessage::SftpTransferNext(..)
            | SftpMessage::SftpTransferItemDone(..)
            | SftpMessage::SftpTransferError(..)
            | SftpMessage::SftpCancelTransfer
            | SftpMessage::SftpRelay(..)
            | SftpMessage::SftpRelayFolder(..)
            | SftpMessage::SftpRelayMove(..)
            | SftpMessage::SftpRelayMoveFolder(..)) => self
                .handle_sftp_transfers(m)
                // Transfers legitimately decline the whole group when no
                // SFTP tab owns the continuation: quiet drop, EXCEPT the
                // OS file drop, which then belongs to the terminal (#106),
                // and its hover pair, which keeps the sidebar Files
                // drop hint honest with no SFTP tab open (issue #167).
                // With no SFTP tab open at all, the owner gate declines
                // before the FileDropped arm can route it, so the
                // fallback has to happen here.
                .unwrap_or_else(|m| match m {
                    SftpMessage::SftpFileDropped(path) => {
                        self.os_drop_hover = false;
                        self.buffer_terminal_drop(path)
                    }
                    SftpMessage::SftpFileHovered => {
                        self.os_drop_hover = true;
                        Task::none()
                    }
                    SftpMessage::SftpFilesHoveredLeft => {
                        self.os_drop_hover = false;
                        Task::none()
                    }
                    _ => Task::none(),
                }),
            m @ (SftpMessage::SftpStartEdit(..)
            | SftpMessage::SftpOpenLocal(..)
            | SftpMessage::SftpRevealInExplorer(..)
            | SftpMessage::SftpEditWatchTick
            | SftpMessage::SftpStartEditWith(..)
            | SftpMessage::SftpPickEditorFor(..)
            | SftpMessage::SftpToggleOpenGroup
            | SftpMessage::SftpEditWatchReady(..)
            | SftpMessage::SftpEditPromptChoice(..)
            | SftpMessage::SftpEditReopenChoice(..)
            | SftpMessage::SftpEditToast(..)
            | SftpMessage::SftpEditWatchUploadDone(..)
            | SftpMessage::SftpShowProperties(..)
            | SftpMessage::SftpPropertiesLoaded(..)
            | SftpMessage::SftpPropertiesToggleBit(..)
            | SftpMessage::SftpPropertiesModeInput(..)
            | SftpMessage::SftpPropertiesApply
            | SftpMessage::SftpPropertiesDone(..)
            | SftpMessage::SftpPropertiesClose) => self
                .handle_sftp_files(m)
                .unwrap_or_else(crate::dispatch::unrouted),
            m @ (SftpMessage::ZipIndexed(..)
            | SftpMessage::ArchiveDone(..)
            | SftpMessage::SftpZipOpen(..)
            | SftpMessage::SftpZipNavigate(..)
            | SftpMessage::SftpZipClose(..)
            | SftpMessage::SftpZipCopyOut(..)
            | SftpMessage::SftpArchiveExtract(..)
            | SftpMessage::SftpArchiveCompress(..)
            | SftpMessage::SftpToolsProbed(..)) => self
                .handle_sftp_archive(m)
                .unwrap_or_else(crate::dispatch::unrouted),
            m @ (SftpMessage::HostMounted(..)
            | SftpMessage::RemoteError(..)
            | SftpMessage::SftpSetInitialPath(..)
            | SftpMessage::SftpClearInitialPath(..)
            | SftpMessage::SftpPickHost(..)
            | SftpMessage::SftpOpenPicker(..)
            | SftpMessage::SftpPickLocal
            | SftpMessage::SftpClosePicker
            | SftpMessage::SftpRemountPane(..)
            | SftpMessage::SftpPickerSearch(..)
            | SftpMessage::OpenSftpForConnection(..)
            | SftpMessage::OpenSftpConsoleForHost(..)
            | SftpMessage::OpenSftpConsoleForTab(..)
            | SftpMessage::SftpCancelRemoteLoad(..)
            | SftpMessage::SftpRetryRemote(..)) => self
                .handle_sftp_hosts(m)
                .unwrap_or_else(crate::dispatch::unrouted),
            m @ (SftpMessage::SelectSftpTab(..)
            | SftpMessage::CloseSftpTab(..)
            | SftpMessage::CloseSftpTabLiveConfirmed(..)
            | SftpMessage::NewSftpTab
            | SftpMessage::ConfirmCloseSftpTab
            | SftpMessage::CancelCloseSftpTab
            | SftpMessage::ToggleSftpTabPin(..)
            | SftpMessage::ShowSftpTabMenu(..)
            | SftpMessage::CloseOtherSftpTabs(..)
            | SftpMessage::SftpTabHovered(..)
            | SftpMessage::SftpTabUnhovered(..)) => self
                .handle_sftp_tabs(m)
                .unwrap_or_else(crate::dispatch::unrouted),
            m @ (SftpMessage::SftpRemoteLoaded(..)
            | SftpMessage::SftpNavigateRemote(..)
            | SftpMessage::SftpNavigateLocal(..)
            | SftpMessage::SftpUp(..)
            | SftpMessage::SftpRefreshLocal(..)
            | SftpMessage::SftpToggleHidden(..)
            | SftpMessage::SftpFilter(..)
            | SftpMessage::SftpStartEditPath(..)
            | SftpMessage::SftpEditPath(..)
            | SftpMessage::SftpPathHistoryToggle(..)
            | SftpMessage::SftpPathHistoryClose
            | SftpMessage::SftpPathHistoryPick(..)
            | SftpMessage::SftpCommitPath(..)
            | SftpMessage::SftpCancelEditPath
            | SftpMessage::SftpSort(..)
            | SftpMessage::SftpListScrolled(..)
            | SftpMessage::SftpListPanned(..)
            | SftpMessage::SftpLocalListed(..)) => self
                .handle_sftp_listing(m)
                .unwrap_or_else(crate::dispatch::unrouted),
            m @ (SftpMessage::SftpToggleActions(..)
            | SftpMessage::SftpToggleDrives(..)
            | SftpMessage::SftpCloseMenus
            | SftpMessage::SftpToggleColumn(..)
            | SftpMessage::SftpColResizeStart(..)
            | SftpMessage::SftpColAutoFit(..)
            | SftpMessage::SftpColDragStart(..)
            | SftpMessage::SftpColHovered(..)
            | SftpMessage::SftpColUnhovered(..)
            | SftpMessage::SftpToggleFilterSearch(..)
            | SftpMessage::SftpToggleLog
            | SftpMessage::SftpLogResizeStart
            | SftpMessage::SftpSplitResizeStart) => self
                .handle_sftp_layout(m)
                .unwrap_or_else(crate::dispatch::unrouted),
            m @ (SftpMessage::SftpStartRename(..)
            | SftpMessage::SftpRenameInput(..)
            | SftpMessage::SftpRenameCommit
            | SftpMessage::SftpRenamed(..)
            | SftpMessage::SftpAskDelete(..)
            | SftpMessage::SftpAskDeleteSelection
            | SftpMessage::SftpConfirmDelete
            | SftpMessage::SftpCancelDelete
            | SftpMessage::SftpEntriesRemoved(..)
            | SftpMessage::SftpStartNewEntry(..)
            | SftpMessage::SftpNewEntryInput(..)
            | SftpMessage::SftpNewEntryCommit
            | SftpMessage::SftpNewEntryCancel
            | SftpMessage::SftpOpResult(..)) => self
                .handle_sftp_entries(m)
                .unwrap_or_else(crate::dispatch::unrouted),
            m @ (SftpMessage::SftpRowRightClick(..)
            | SftpMessage::SftpBackgroundRightClick(..)
            | SftpMessage::SftpRowMenuClose
            | SftpMessage::SftpCopyPath(..)
            | SftpMessage::SftpCopySelectionPaths(..)
            | SftpMessage::SftpTypeAheadFire(..)
            | SftpMessage::SftpRowEnter(..)
            | SftpMessage::SftpRowExit(..)
            | SftpMessage::SftpNameHovered(..)
            | SftpMessage::SftpNameUnhovered(..)
            | SftpMessage::SftpSlowRenameFire(..)
            | SftpMessage::SftpMouseLeftPressed
            | SftpMessage::SftpRowPressed(..)
            | SftpMessage::SftpParentRowClicked(..)
            | SftpMessage::SftpSelectRow(..)) => self
                .handle_sftp_selection(m)
                .unwrap_or_else(crate::dispatch::unrouted),
        }
    }
}
