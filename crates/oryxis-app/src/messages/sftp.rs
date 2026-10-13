//! SFTP surface: dual-pane browser, tabs, columns, transfers, zip browsing,
//! properties, rename/new-entry, per-owner routing (see `SftpFor`).

use std::sync::Arc;
use uuid::Uuid;
use oryxis_ssh::SshSession;

/// SFTP async-completion messages that ride the `SftpFor` owner-routing
/// envelope (`route_sftp_async`). Grouped into their own enum so
/// `SftpFor` can carry `Box<SftpMessage>` instead of `Box<Message>`,
/// making it a compile error to route a non-SFTP message through the
/// buffer-owner swap path. This is the first `Message` sub-enum (the
/// pilot for splitting the god-enum); new message-heavy areas should be
/// born as their own sub-enum rather than flat `Message` variants.
///
/// Reached through [`Message::Sftp`]: the dispatcher unwraps the
/// envelope in `route_sftp_async` and re-dispatches as `Message::Sftp`,
/// which the SFTP handler chain matches.
#[derive(Debug, Clone)]
pub enum SftpMessage {
    /// Initial mount finished: the live session + SFTP channel, the
    /// session home and the first listing for the picked pane.
    HostMounted(
        crate::state::SftpPaneSide,
        String,
        Arc<SshSession>,
        oryxis_ssh::SftpClient,
        String,
        Vec<oryxis_ssh::SftpEntry>,
    ),
    /// A remote pane operation (mount / listing) failed; `SftpPaneSide`
    /// names the pane whose error banner shows the message.
    RemoteError(crate::state::SftpPaneSide, String),
    /// Central directory parsed (archive real path, mount token
    /// captured at spawn, payload or error). A token that no longer
    /// matches the pane means the pane was remounted (or switched back
    /// to Local) while the index was read: the result is dropped.
    ZipIndexed(
        crate::state::SftpPaneSide,
        String,
        crate::state::ArchiveOpToken,
        Result<crate::state::ZipIndexedPayload, String>,
    ),
    /// Archive operation finished: log label or error. The payload
    /// carries which pane the op changed (refresh / error target) and
    /// which pane it marked busy, each with the mount token captured at
    /// spawn, so completions clear / apply exactly what this op touched
    /// and stale (post-remount) results are dropped.
    ArchiveDone(crate::state::ArchiveDone),
    SftpPickHost(usize),
    SftpRemoteLoaded(crate::state::SftpPaneSide, u64, String, Vec<oryxis_ssh::SftpEntry>),
    /// Navigate a *remote* pane to a POSIX path.
    SftpNavigateRemote(crate::state::SftpPaneSide, String),
    /// Navigate a *local* pane to a filesystem path.
    SftpNavigateLocal(crate::state::SftpPaneSide, std::path::PathBuf),
    /// Go up one directory in the given pane (local or remote).
    SftpUp(crate::state::SftpPaneSide),
    /// A left press landed on a row of the dual-pane browser, at this
    /// (content-space) position. Published by the row's press publisher
    /// AHEAD of the row's own release message, and judged there against
    /// the previous press for a double click (`crate::double_click`).
    SftpRowPressed(crate::state::SftpClickKey, iced::Point),
    /// Left click (release) on a pane's `..` row: a single click puts the
    /// keyboard cursor on it, a double click goes up one folder.
    SftpParentRowClicked(crate::state::SftpPaneSide),
    /// Refresh a local pane's listing from its current path.
    SftpRefreshLocal(crate::state::SftpPaneSide),
    /// Open the host picker, choosing for the given pane.
    SftpOpenPicker(crate::state::SftpPaneSide),
    /// Pick "Local" for the left pane (only offered there).
    SftpPickLocal,
    SftpClosePicker,
    /// Focus the SFTP tab at this `sftp_tabs` index (swap its state into the
    /// active buffer and switch the surface to it).
    SelectSftpTab(usize),
    /// Close the SFTP tab at this index. Guards against an in-flight transfer
    /// / unsaved edit-session via a confirmation modal.
    CloseSftpTab(usize),
    /// The "close this tab with a live session?" guard was answered yes
    /// for the standalone SFTP tab with this id (resolved to a row at
    /// execution time, never carried as one).
    CloseSftpTabLiveConfirmed(uuid::Uuid),
    /// Open a fresh, empty SFTP tab (host picker) and focus it.
    NewSftpTab,
    /// Proceed with closing the SFTP tab pending confirmation (after the
    /// in-flight-transfer / unsaved-edit warning).
    ConfirmCloseSftpTab,
    /// Dismiss the SFTP close-guard modal without closing.
    CancelCloseSftpTab,
    /// Toggle the pinned state of the SFTP tab at this index.
    ToggleSftpTabPin(usize),
    /// Open the right-click context menu for the SFTP tab at this index.
    ShowSftpTabMenu(usize),
    /// Close every SFTP tab except the one at this index.
    CloseOtherSftpTabs(usize),
    /// Mount connection `usize` into a specific pane side (regardless of the
    /// picker target). Used to re-mount a restored pinned SFTP tab's pane(s).
    SftpRemountPane(crate::state::SftpPaneSide, usize),
    /// Cursor entered the SFTP tab at this index (hover + live-slide target).
    SftpTabHovered(usize),
    /// Cursor left the SFTP tab at this index. Indexed for the same reason
    /// `TabsMessage::TabUnhovered` is.
    SftpTabUnhovered(usize),
    SftpPickerSearch(String),
    SftpToggleHidden(crate::state::SftpPaneSide),
    SftpFilter(crate::state::SftpPaneSide, String),
    SftpToggleActions(crate::state::SftpPaneSide),
    SftpToggleDrives(crate::state::SftpPaneSide),
    SftpCloseMenus,
    /// Toggle visibility of an optional file-list column (Size / Modified /
    /// Type / Permissions / Owner) for one pane. Per-pane; also updates the
    /// persisted template.
    SftpToggleColumn(crate::state::SftpPaneSide, crate::state::SftpColumn),
    /// Begin dragging a column's right-edge resize handle.
    SftpColResizeStart(crate::state::SftpPaneSide, crate::state::SftpColumn),
    /// Double-click a column's resize handle: auto-fit the column to the
    /// widest value across every row (visible or not).
    SftpColAutoFit(crate::state::SftpPaneSide, crate::state::SftpColumn),
    /// Press on a column header: arms a reorder drag (promoted to active on
    /// move; a release without movement falls through to the sort click).
    SftpColDragStart(crate::state::SftpPaneSide, crate::state::SftpColumn),
    /// Cursor entered / left a column header (reorder drop target).
    SftpColHovered(crate::state::SftpPaneSide, crate::state::SftpColumn),
    SftpColUnhovered(crate::state::SftpPaneSide, crate::state::SftpColumn),
    /// Toggle this pane's collapsed filter popover (narrow layout).
    SftpToggleFilterSearch(crate::state::SftpPaneSide),
    /// Toggle the FileZilla-style message-log panel at the bottom of the view.
    SftpToggleLog,
    /// Begin dragging the horizontal divider above the message-log panel to
    /// resize its height.
    SftpLogResizeStart,
    /// Begin dragging the center divider between the two SFTP panes.
    SftpSplitResizeStart,
    /// Open a new SFTP tab mounted on the saved connection at this index
    /// (host-card context menu). Reuses a live SSH session if one is open,
    /// otherwise connects.
    OpenSftpForConnection(usize),
    /// Open an interactive SFTP console on a saved host (issue #188),
    /// from the host card's menu.
    ///
    /// Carries the host's ID rather than its index, unlike its neighbour
    /// above: the card menu is anchored to the host, and an index would
    /// be resolved against a list that a re-sort under the open menu can
    /// have changed.
    OpenSftpConsoleForHost(uuid::Uuid),
    /// Open an interactive SFTP console for the terminal tab at this
    /// index, on the host that tab is connected to and at the directory
    /// its shell had reached.
    OpenSftpConsoleForTab(usize),
    SftpStartEditPath(crate::state::SftpPaneSide),
    SftpEditPath(crate::state::SftpPaneSide, String),
    /// Toggle this pane's path-history dropdown (issue #85).
    SftpPathHistoryToggle(crate::state::SftpPaneSide),
    /// Close any open path-history dropdown.
    SftpPathHistoryClose,
    /// Navigate to a directory picked from the history dropdown.
    SftpPathHistoryPick(crate::state::SftpPaneSide, String),
    SftpCommitPath(crate::state::SftpPaneSide),
    #[allow(dead_code)] // wired by upcoming Esc handler
    SftpCancelEditPath,
    SftpSort(crate::state::SftpPaneSide, crate::state::SftpSortColumn),
    SftpRowRightClick(crate::state::SftpPaneSide, String, bool),
    /// Right-click on the empty area of a pane (not a row). Opens the
    /// directory-level context menu anchored at the cursor.
    SftpBackgroundRightClick(crate::state::SftpPaneSide),
    SftpRowMenuClose,
    /// Copy a full path (row entry or the pane's current directory) to
    /// the clipboard. The string arrives already side-formatted (POSIX
    /// for remote entries, OS-native for local ones).
    SftpCopyPath(String),
    /// Copy every selected path in the given pane, one per line.
    SftpCopySelectionPaths(crate::state::SftpPaneSide),
    SftpStartRename(crate::state::SftpPaneSide, String),
    /// The cursor entered the drawn file-name label of a row. Gates the
    /// slow-click rename to the name itself, Explorer / Finder style.
    SftpNameHovered(crate::state::SftpPaneSide, String),
    /// The cursor left a file-name label.
    SftpNameUnhovered(crate::state::SftpPaneSide, String),
    /// Deferred slow-click rename `(side, path, click generation)`: sent
    /// a double-click window after the arming release and dropped when a
    /// newer click has bumped the generation meanwhile.
    SftpSlowRenameFire(crate::state::SftpPaneSide, String, u64),
    SftpRenameInput(String),
    SftpRenameCommit,
    /// A remote rename succeeded: `(side, dir to reload, new basename)`.
    /// Logs the rename, then re-lists the directory.
    SftpRenamed(crate::state::SftpPaneSide, String, String),
    SftpAskDelete(crate::state::SftpPaneSide, String, bool),
    SftpAskDeleteSelection,
    SftpConfirmDelete,
    SftpCancelDelete,
    /// Remote delete succeeded: drop these (full) paths from the given
    /// pane's listing in place, no re-list.
    SftpEntriesRemoved(crate::state::SftpPaneSide, Vec<String>),
    /// Deferred type-ahead search fire. Carries the generation it was
    /// scheduled for; runs only if no newer keystroke superseded it
    /// (debounce, so fast typing searches once with the full buffer).
    SftpTypeAheadFire(u64),
    /// A pane's file list scrolled: carries the side, the new absolute
    /// vertical offset (px) and the visible viewport height (px). Stored so
    /// keyboard navigation only scrolls when the cursor reaches an edge.
    SftpListScrolled(crate::state::SftpPaneSide, f32, f32),
    /// The overflow layout's outer horizontal scrollable panned: carries
    /// the side and the new absolute horizontal offset (px). Stored so
    /// draw-time (content-space) rects, e.g. the Menu-key row anchor, can
    /// be mapped back to the screen while the columns are panned.
    SftpListPanned(crate::state::SftpPaneSide, f32),
    SftpStartNewEntry(crate::state::SftpPaneSide, crate::state::SftpEntryKind),
    SftpNewEntryInput(String),
    SftpNewEntryCommit,
    SftpNewEntryCancel,
    /// Async local-directory listing landed (side, pane listing seq,
    /// listed path, rows or error). Emitted by `spawn_local_listing`;
    /// stale seqs are dropped.
    SftpLocalListed(
        crate::state::SftpPaneSide,
        u64,
        std::path::PathBuf,
        Result<Vec<crate::state::LocalEntry>, String>,
    ),
    SftpRowEnter(crate::state::SftpPaneSide, String, bool),
    /// Carries the row being LEFT. The hovered row is cleared only when
    /// it matches: iced publishes enter / exit in tree order, so moving up
    /// the list delivers the new row's `enter` BEFORE the old row's
    /// `exit`, and an unconditional clear threw the fresh value away.
    SftpRowExit(crate::state::SftpPaneSide, String),
    SftpMouseLeftPressed,
    SftpSelectRow(crate::state::SftpPaneSide, String, bool),
    /// "Open / Edit" on a remote row: download a temp copy, hand it to the
    /// OS file association and watch it in the background. The same
    /// pipeline as `SftpStartEditWith`, with the default opener.
    SftpStartEdit(crate::state::SftpPaneSide, String),
    /// Open a local file in the OS default app, no temp copy, no
    /// mtime watch. Edits land on the file directly.
    SftpOpenLocal(std::path::PathBuf),
    /// Reveal a local file/folder in the OS file manager (local pane
    /// only). Folders open in place; files open their folder selected.
    /// Carries the absolute path and whether it's a directory.
    SftpRevealInExplorer(std::path::PathBuf, bool),
    SftpEditWatchTick,
    /// Open a remote file with a chosen local application (issue #84):
    /// downloads a temp copy, spawns the opener, and registers a
    /// background watch that confirms each save via the save dialog.
    SftpStartEditWith(
        crate::state::SftpPaneSide,
        String,
        crate::state::SftpEditOpener,
    ),
    /// "Other application..." on a remote row (issue #114): raise the OS
    /// file picker, then run `SftpStartEditWith` with the chosen
    /// executable as a one-time opener. The counterpart to the configured
    /// editor, and the only "open with" that works on Linux, where the
    /// OS application picker has no stable cross-desktop CLI.
    SftpPickEditorFor(crate::state::SftpPaneSide, String),
    /// Expand / collapse the row menu's "Open with" family. Deliberately
    /// does NOT close the menu, like the Columns toggles.
    SftpToggleOpenGroup,
    /// The temp copy is written and the opener spawned: register the
    /// background watch.
    SftpEditWatchReady(crate::state::EditSession),
    /// A button of the save-confirmation dialog was pressed, for the watch
    /// owning this temp file (the dialog can be answering for a watch that
    /// lives on a parked tab, so it never means "the first dirty one").
    SftpEditPromptChoice(crate::state::SftpEditPromptChoice, std::path::PathBuf),
    /// A button of the reopen-or-redownload dialog was pressed.
    SftpEditReopenChoice(crate::state::SftpEditReopenChoice),
    /// Surface an edit-flow message as a toast. Used by the paths that
    /// have no pane to report into (a relaunched opener, sidebar edits).
    SftpEditToast(String),
    /// A watch upload finished: re-arm the entry (keyed by temp path)
    /// with the temp mtime captured at upload time, or surface the error.
    SftpEditWatchUploadDone(std::path::PathBuf, Result<std::time::SystemTime, String>),
    /// Remember this remote directory as the host's SFTP landing folder
    /// (`Connection.sftp_initial_path`), from the pane's context menu.
    SftpSetInitialPath(crate::state::SftpPaneSide, String),
    /// Forget the host's saved SFTP landing folder: fresh mounts go back
    /// to the login directory.
    SftpClearInitialPath(crate::state::SftpPaneSide),
    SftpCancelRemoteLoad(crate::state::SftpPaneSide),
    /// Retry the last failed remote action, either re-list the
    /// current path (if a session is still mounted) or re-run the
    /// full host-pick flow (if the connect itself failed).
    SftpRetryRemote(crate::state::SftpPaneSide),
    SftpShowProperties(crate::state::SftpPaneSide, String, bool),
    SftpPropertiesLoaded(crate::state::PropertiesView),
    SftpPropertiesToggleBit(crate::state::PermBit),
    SftpPropertiesModeInput(String),
    SftpPropertiesApply,
    SftpPropertiesDone(Result<(), String>),
    SftpPropertiesClose,
    /// Open a zip archive (real full path) for virtual browsing.
    SftpZipOpen(crate::state::SftpPaneSide, String),
    /// Navigate to a directory INSIDE the browsed archive ("" = root).
    SftpZipNavigate(crate::state::SftpPaneSide, String),
    /// Leave virtual browsing, restoring the pane's real directory.
    SftpZipClose(crate::state::SftpPaneSide),
    /// Copy an entry (inner path, is_dir) out of the browsed archive
    /// into the OTHER pane's current directory.
    SftpZipCopyOut(crate::state::SftpPaneSide, String, bool),
    /// Extract an archive (real full path) next to itself.
    SftpArchiveExtract(crate::state::SftpPaneSide, String),
    /// Compress the clicked path (or the selection containing it) into
    /// an archive of the given kind, in the pane's current directory.
    SftpArchiveCompress(
        crate::state::SftpPaneSide,
        oryxis_archive::names::ArchiveKind,
        String,
    ),
    /// Archive operation finished: log label or error. The payload
    /// Once-per-mount remote tool probe result. The token is the mount
    /// generation the probe was spawned for; a stale one is dropped.
    SftpToolsProbed(
        crate::state::SftpPaneSide,
        crate::state::ArchiveOpToken,
        oryxis_archive::remote::RemoteShell,
        oryxis_archive::remote::ArchiveTools,
    ),
    /// Operation result for a remote pane. `SftpPaneSide` names the pane
    /// whose error banner should show the message on failure.
    SftpOpResult(crate::state::SftpPaneSide, String, bool),
    /// Toggle the per-file progress panel that drops down from the
    /// transfer status strip.
    SftpToggleTransferPanel,
    /// Periodic tick while a transfer runs: forces a redraw so the live
    /// byte-progress bar advances (it reads a shared atomic counter).
    SftpTransferTick,
    /// Download ONE remote file. A thin alias for a one-item
    /// `SftpDownloadBatch`, kept because the row menu and the
    /// "Download to..." wrapper both name a single path.
    SftpDownload(String),
    /// Pick the destination folder for `then` (any of the three download
    /// entry points), then run it. The picked folder rides in
    /// `download_dest_override`, which those handlers already consume, so
    /// the inner action needs no destination-aware variant of its own.
    SftpDownloadTo(Box<SftpMessage>),
    /// The folder picker answered; `None` means the user cancelled.
    SftpDownloadDestPicked(Option<std::path::PathBuf>, Box<SftpMessage>),
    SftpDuplicate(crate::state::SftpPaneSide, String),
    SftpFileHovered,
    SftpFilesHoveredLeft,
    SftpFileDropped(std::path::PathBuf),
    SftpDropFlush,
    SftpUploadFolder(std::path::PathBuf),
    SftpDownloadFolder(String),
    SftpDuplicateFolder(crate::state::SftpPaneSide, String),
    SftpResolveOverwrite(crate::state::OverwriteAction),
    SftpToggleApplyToAll,
    SftpUploadBatch(Vec<std::path::PathBuf>),
    /// Download an explicit list of remote entries, `(path, is_dir)`, as
    /// ONE queued transfer. The download mirror of `SftpUploadBatch`, and
    /// the only download entry point that shows the progress strip, so
    /// every caller routes through it, a single file included.
    SftpDownloadBatch(Vec<(String, bool)>),
    SftpUploadSelection,
    SftpDownloadSelection,
    SftpDuplicateSelection,
    SftpTransferConflict(Uuid, crate::state::OverwritePrompt, crate::state::TransferItem, u8),
    SftpTransferQueueReady(Uuid, crate::state::TransferState),
    /// Pop one item and dispatch to whichever slot is free. The Next
    /// handler picks the slot itself instead of carrying it in the
    /// message, that way pause/resume can spawn fresh chains without
    /// having to remember which slot was on which client. The `Uuid` is the
    /// owning SFTP tab.
    SftpTransferNext(Uuid),
    /// Slot freed up after a queue item completed successfully.
    SftpTransferItemDone(Uuid, u8),
    SftpTransferError(Uuid, String, u8),
    SftpCancelTransfer,
    /// Relay a single remote file from the `from` side's host to the
    /// other side's host (server-to-server). `from` is the source pane.
    SftpRelay(crate::state::SftpPaneSide, String),
    /// Relay a remote folder tree from the `from` side's host to the
    /// other side's host.
    SftpRelayFolder(crate::state::SftpPaneSide, String),
    /// Like [`SftpRelay`](Self::SftpRelay) but removes the source once
    /// the copy is verified. The removal never runs unless every queue
    /// item landed at the right size.
    SftpRelayMove(crate::state::SftpPaneSide, String),
    /// Folder counterpart of [`SftpRelayMove`](Self::SftpRelayMove).
    SftpRelayMoveFolder(crate::state::SftpPaneSide, String),
}
