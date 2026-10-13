//! Sidebar Files tab (per-pane SFTP mini-browser) messages.

use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum SidebarFilesMessage {
    SidebarFilesNavigate(String),
    SidebarFilesRefresh,
    SidebarFilesToggleFollow,
    SidebarFilesToggleHidden,
    /// Promote the sidebar browser to a full SFTP tab at its current
    /// directory.
    SidebarFilesExpand,
    /// Initial mount finished: the backend (SFTP channel, or the local
    /// filesystem for a local shell), the session home (for
    /// `~`-relative cwd expansion) plus the first listing. The `u64` is
    /// the request stamp (`PaneFiles::req_seq`) captured at dispatch; a
    /// mismatch on arrival means a newer request (or a disconnect
    /// reset) superseded this one and it is dropped.
    SidebarFilesMounted(
        Uuid,
        u64,
        crate::local_files::FilesClient,
        Option<String>,
        String,
        Vec<oryxis_ssh::SftpEntry>,
    ),
    /// A navigation / follow / refresh listing landed (same stamp rule).
    SidebarFilesListed(Uuid, u64, String, Vec<oryxis_ssh::SftpEntry>),
    SidebarFilesError(Uuid, u64, String),
    SidebarFilesRowHovered(usize),
    SidebarFilesRowUnhovered(usize),
    /// Left-click on a row: single-click selects it (highlight); a quick
    /// double-click on a directory enters it.
    SidebarFilesSelectRow(String, bool),
    /// A left press landed on a row, at this (content-space) position.
    /// Published ahead of the row's own press message and judged there
    /// against the previous press for a double click.
    SidebarFilesRowPressed(crate::state::FilesClickKey, iced::Point),
    /// Left-click on the `..` row: selects it; a double click goes up.
    SidebarFilesSelectParent,
    /// Right-click on a sidebar Files row: open its context menu
    /// (full path + is_dir), anchored at the cursor.
    ShowSidebarFilesRowMenu(String, bool),
    /// The header path is clickable (mirrors the SFTP pane's path
    /// editing): start / live-edit / commit typing a directory.
    SidebarFilesStartEditPath,
    SidebarFilesEditPath(String),
    SidebarFilesCommitPath,
    /// Path combo-box (issue #85): toggle / close the visited-directory
    /// dropdown, or jump straight to a picked entry.
    SidebarFilesPathHistoryToggle,
    SidebarFilesPathHistoryClose,
    SidebarFilesPathHistoryPick(String),
    /// A left click landed on the Files tab's dead space (no row, no
    /// input, no button captured it): blur = cancel any inline edit
    /// (path / rename / new entry) and close the history dropdown.
    SidebarFilesEditBlur,
    /// Open (or reveal) this tab's SFTP session at the given remote
    /// directory: the sidebar ⛶, the row context menu and the expand
    /// affordances all funnel here.
    SidebarFilesOpenSftpAt(String),
    /// Right-click on the list's empty area: directory-level menu
    /// (New file / New folder / Upload here / Refresh / Copy path).
    ShowSidebarFilesBackgroundMenu,
    /// Inline rename of a sidebar row: start (full path) / live input /
    /// commit. Esc via the sidebar router cancels.
    SidebarFilesStartRename(String),
    SidebarFilesRenameInput(String),
    SidebarFilesRenameCommit,
    /// Inline create (file or folder) at the top of the list.
    SidebarFilesStartNewEntry(crate::state::SftpEntryKind),
    SidebarFilesNewEntryInput(String),
    SidebarFilesNewEntryCommit,
    /// Delete an entry: ask (routes through the shared confirm dialog),
    /// then the confirmed op (recursive for directories).
    SidebarFilesDelete(String, bool),
    SidebarFilesDeleteConfirmed(String, bool),
    /// Bulk delete of the whole current selection (arm the same shared
    /// confirm, count-aware copy) and its confirmed op.
    SidebarFilesDeleteSelection(Vec<(String, bool)>),
    SidebarFilesDeleteConfirmedSelection(Vec<(String, bool)>),
    /// Copy every path of the current selection to the clipboard, one
    /// per line (the SFTP pane's bulk-copy rule).
    SidebarFilesCopySelectionPaths,
    /// Download a file to a local destination picked via the OS dialog.
    /// Only opens the dialog; a cancelled one ends the flow untouched.
    SidebarFilesDownload(String),
    /// Bulk download of the whole selection (files and folders) into a
    /// directory picked via the OS dialog.
    SidebarFilesDownloadSelection,
    /// The folder dialog returned a destination: enqueue every selected
    /// entry under it. Payload: pane id, destination directory, the
    /// selected `(remote path, is_dir)` targets (directories walk
    /// recursively like the SFTP pane's batch download).
    SidebarFilesDownloadSelectionPicked(Uuid, String, Vec<(String, bool)>),
    /// The save dialog returned a destination: enqueue the download on
    /// the pane's own runner. Payload: pane id, remote path, local
    /// destination file, and the size the listing reported when the row
    /// was clicked (a progress-total hint; the browser may have
    /// navigated away while the dialog was open).
    SidebarFilesDownloadPicked(Uuid, String, std::path::PathBuf, Option<u64>),
    /// One-shot op finished (download / upload): toast the outcome.
    SidebarFilesOpToast(String),
    /// Upload local file(s) picked via the OS dialog into a directory.
    /// Only opens the dialog; a cancelled dialog ends the flow with no
    /// state touched (in particular no request-stamp bump, which would
    /// strand an in-flight listing's completion).
    SidebarFilesUploadInto(String),
    /// The upload dialog returned actual picks: run the uploads on the
    /// pane's channel. Payload: pane id, destination directory, local
    /// paths.
    SidebarFilesUploadPicked(Uuid, String, Vec<std::path::PathBuf>),
    /// Open the shared Properties (permissions) modal for a sidebar
    /// entry, chmod-ing through the sidebar's own client.
    SidebarFilesShowProperties(String, bool),
    /// Edit-in-place for a sidebar file (temp download + OS editor +
    /// auto-upload), through the sidebar's own client.
    SidebarFilesEdit(String),
}
