//! SFTP view state (split out of `state.rs`).

use super::*;

/// Per-pane state for the SFTP browser. Each pane is either Local
/// (`is_remote == false`, browsing the OS filesystem) or a mounted
/// remote host (`is_remote == true`, browsing via SFTP). The two panes
/// of [`SftpState`] are positional (Left / Right); their *nature* is
/// this `is_remote` flag, so either pane can be Local or remote
/// (with the rule that Local is only ever offered on the left).
#[derive(Default)]
pub(crate) struct PaneState {
    /// false = Local pane, true = Remote pane.
    pub is_remote: bool,
    // Remote connection (Some only when `is_remote` and connected).
    /// Currently mounted SSH session, if any. Cloned from the source host
    /// when the user picks one from the connection list.
    pub session: Option<Arc<SshSession>>,
    /// Active SFTP client (one channel per session).
    pub client: Option<SftpClient>,
    /// Label of the currently mounted host, shown in the breadcrumb.
    pub host_label: Option<String>,
    pub remote_path: String,
    pub remote_entries: Vec<SftpEntry>,
    pub remote_loading: bool,
    /// Sequence for in-flight async remote listings, mirroring
    /// `local_list_seq`: stamped on every `SftpNavigateRemote` spawn from
    /// the same process-global counter, carried by `SftpRemoteLoaded`, and
    /// results with a stale seq are dropped. Prevents a slow network
    /// listing from overwriting a newer navigation, and (because the
    /// counter is global) prevents a listing from clobbering the wrong
    /// surface's pane after a hybrid park/hoist swap.
    pub remote_list_seq: u64,
    // Local (used when `!is_remote`).
    pub local_path: std::path::PathBuf,
    pub local_entries: Vec<LocalEntry>,
    /// Sequence for in-flight async local listings: bumped on every
    /// `spawn_local_listing`, carried by `SftpLocalListed`, and results
    /// with a stale seq are dropped. Prevents a slow cold-path listing
    /// from overwriting (or hijacking the path of) a newer navigation.
    pub local_list_seq: u64,
    /// Whether the Windows-style drive picker dropdown is open. Only
    /// rendered on Windows hosts.
    pub drives_open: bool,
    // Shared per-pane UI.
    pub error: Option<String>,
    pub filter: String,
    /// Sort column + direction for this pane.
    pub sort: SftpSort,
    /// When false (default), entries whose name starts with `.` are
    /// hidden, matches `ls` / Finder / Explorer convention. Toggleable
    /// from each pane's Actions menu independently so the user can show
    /// hidden remote files without exposing all the local dotfiles.
    pub show_hidden: bool,
    /// When `Some`, the breadcrumb is replaced by a text input the user
    /// can type a full path into. The string is the in-progress edit.
    pub path_editing: Option<String>,
    /// Directories visited in this pane during the session, most recent
    /// first, deduped, capped at `PATH_HISTORY_CAP` (issue #85). Feeds the
    /// path-bar dropdown so the user can jump back without retyping.
    /// Per-pane and in-memory only: paths are host-scoped and remounting
    /// the pane on another host must not offer the previous host's tree.
    pub path_history: Vec<String>,
    /// Back / forward stacks for the side mouse buttons.
    ///
    /// Deliberately NOT the `path_history` above: that is a RECENCY list
    /// (a revisit moves the entry to the top), so stepping back would
    /// reorder it and the next step would bounce between two folders. A
    /// stack answers "where was I before", which is the question the
    /// buttons ask.
    pub nav_back: Vec<String>,
    pub nav_fwd: Vec<String>,
    /// Set while a back / forward step is in flight, so its arrival is not
    /// recorded as a fresh navigation. Without it, going back would push
    /// the folder just left onto the back stack and the pair would cancel
    /// each other out.
    pub nav_replay: bool,
    /// Whether this pane's path-history dropdown is open.
    pub path_history_open: bool,
    /// Actions popover anchored to this pane's header.
    pub actions_open: bool,
    /// Per-pane column configuration (visibility + order + widths). Seeded
    /// from the persisted global template when the tab is created, then
    /// edited independently so the Local and remote panes (and each tab)
    /// can show different columns.
    pub columns: SftpColumnState,
    /// True while this pane's collapsed filter input (narrow layout) is
    /// expanded into its floating popover.
    pub filter_open: bool,
    /// Last known absolute vertical scroll offset (px) of this pane's file
    /// list, fed from the scrollable's `on_scroll`. Used by keyboard
    /// navigation to scroll only when the cursor reaches a viewport edge.
    pub list_scroll_y: f32,
    /// Last known visible height (px) of this pane's file list viewport,
    /// also from `on_scroll`. `0.0` until the first scroll event (then the
    /// nav falls back to proportional snapping).
    pub list_viewport_h: f32,
    /// Last known absolute horizontal offset (px) of the overflow
    /// layout's outer scrollable, fed from its `on_scroll`. Used to map
    /// draw-time (content-space) rects back to the screen while the
    /// columns are panned; `Cell` so `view()` can zero it when the
    /// overflow layout (and with it the scrollable's own state) goes
    /// away, otherwise a stale pan would survive a window resize.
    pub list_pan_x: std::cell::Cell<f32>,
    /// Virtual zip-browse mode. While `Some`, the pane's entry list
    /// shows the INSIDE of a zip archive (listed from its central
    /// directory over ranged reads, nothing extracted) and the pane
    /// path carries the synthetic `<archive>!/<inner>` form that the
    /// navigation handlers intercept. Mutating operations are disabled
    /// for the pane while browsing.
    pub zip: Option<ZipBrowse>,
    /// Which archive tools the mounted host has, probed once per mount
    /// over the exec channel (`None` until the probe lands, or on a
    /// host whose exec channel is unavailable). Local panes never set
    /// it: the local side uses in-process Rust codecs.
    pub archive_tools: Option<(
        oryxis_archive::remote::RemoteShell,
        oryxis_archive::remote::ArchiveTools,
    )>,
    /// Label shown in the pane band while an archive operation
    /// (extract / compress / copy-out) is running; also blocks starting
    /// a second one on the same pane.
    pub archive_busy: Option<String>,
    /// Mount generation of this pane. Stamped from the process-global
    /// listing counter (`sftp_methods::next_list_seq`) by
    /// [`PaneState::note_mounted`], which the mount pipeline reaches
    /// through `spawn_archive_probe` on every `SftpMessage::HostMounted`. Async
    /// archive completions capture it (inside an [`ArchiveOpToken`]) at
    /// spawn time and drop their result when it no longer matches, so a
    /// slow zip index / extract / compress / copy-out / tool probe
    /// finishing after the pane was remounted to another host can't
    /// install the previous host's state. Globally-unique numbering
    /// (not a per-pane counter) means a token can't accidentally match
    /// another surface's pane after a hybrid park / hoist swap either.
    pub mount_seq: u64,
}

impl PaneState {
    /// Record leaving `previous` for a new directory. Clears the forward
    /// stack, because branching off mid-history is a new future.
    pub fn push_nav(&mut self, previous: String) {
        const NAV_CAP: usize = 100;
        // A back / forward step consumes the flag instead of recording:
        // its arrival is the history being replayed, not a new visit.
        if std::mem::take(&mut self.nav_replay) {
            return;
        }
        if previous.is_empty() {
            return;
        }
        self.nav_back.push(previous);
        self.nav_fwd.clear();
        if self.nav_back.len() > NAV_CAP {
            self.nav_back.remove(0);
        }
    }

    /// Pop the previous directory, remembering `current` so Forward can
    /// come back to it. `None` when there is nowhere to go.
    pub fn nav_go_back(&mut self, current: String) -> Option<String> {
        let target = self.nav_back.pop()?;
        self.nav_fwd.push(current);
        self.nav_replay = true;
        Some(target)
    }

    /// The mirror of [`Self::nav_go_back`].
    pub fn nav_go_forward(&mut self, current: String) -> Option<String> {
        let target = self.nav_fwd.pop()?;
        self.nav_back.push(current);
        self.nav_replay = true;
        Some(target)
    }

    /// Stamp a fresh mount generation. Called (via
    /// `spawn_archive_probe`) right after `SftpMessage::HostMounted` reset the
    /// pane for a new host: every archive completion still in flight
    /// for the previous mount now carries a stale token.
    pub(crate) fn note_mounted(&mut self) {
        self.mount_seq = crate::sftp_methods::next_list_seq();
    }

    /// Identity of "this pane, as currently mounted", captured when an
    /// archive operation is spawned and checked when its async
    /// completion lands. `is_remote` rides along because switching the
    /// pane back to Local doesn't remount (so doesn't bump
    /// `mount_seq`) but still invalidates any in-flight result.
    pub(crate) fn archive_op_token(&self) -> ArchiveOpToken {
        ArchiveOpToken {
            mount_seq: self.mount_seq,
            is_remote: self.is_remote,
        }
    }

    /// True when a token captured at op start still describes this
    /// pane: no remount happened and its Local / remote nature is
    /// unchanged. Handlers drop stale completions instead of applying
    /// them.
    pub(crate) fn archive_op_current(&self, token: ArchiveOpToken) -> bool {
        self.mount_seq == token.mount_seq && self.is_remote == token.is_remote
    }

    /// Whether the op that captured `token` still owns this pane's
    /// `archive_busy` flag. Ownership survives a Local switch (same
    /// generation, only `is_remote` flipped, and the per-pane start
    /// guard means no other op could have set the flag since) but not
    /// a remount: the mount reset sweeps the flag and a newer op may
    /// have claimed it in the meantime.
    pub(crate) fn archive_op_owns_busy(&self, token: ArchiveOpToken) -> bool {
        self.mount_seq == token.mount_seq
    }
}

/// Snapshot of a pane's mount identity at archive-op spawn time (see
/// [`PaneState::archive_op_token`]). Carried through the async archive
/// messages; a completion whose token is no longer current is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ArchiveOpToken {
    pub mount_seq: u64,
    pub is_remote: bool,
}

/// Payload of `Message::Sftp(SftpMessage::ArchiveDone)`. `side` is the pane whose
/// contents the operation changed (refresh target on success, error
/// surface on failure); `busy_side` is the pane the op marked
/// `archive_busy` at start. They differ only for copy-out, which marks
/// the SOURCE pane busy while writing into the destination (`side`).
/// Each side carries the token captured for its pane at spawn time so
/// the completion clears / applies exactly what this op touched.
#[derive(Debug, Clone)]
pub(crate) struct ArchiveDone {
    pub side: SftpPaneSide,
    pub token: ArchiveOpToken,
    pub busy_side: SftpPaneSide,
    pub busy_token: ArchiveOpToken,
    pub result: Result<String, String>,
}

/// Virtual zip-browse state for one pane (see [`PaneState::zip`]).
pub(crate) struct ZipBrowse {
    /// Real absolute path of the archive (remote path string, or the
    /// local `PathBuf` rendered lossy).
    pub archive_path: String,
    /// Archive file name, for the breadcrumb chip.
    pub archive_name: String,
    /// Current directory INSIDE the archive; `""` at the archive root.
    pub inner: String,
    /// Parsed central directory (immutable while browsing).
    pub index: Arc<oryxis_archive::browse::ZipIndex>,
    /// Live ranged-read handle for remote archives (`None` for a local
    /// archive, which is reopened per operation). Kept for the whole
    /// browse so entry extraction reuses the open server-side handle.
    pub remote_src: Option<Arc<oryxis_ssh::RemoteRangedFile>>,
    /// Where the pane returns on close.
    pub return_remote_path: String,
    pub return_local_path: std::path::PathBuf,
}

impl ZipBrowse {
    /// The synthetic pane path for the current position:
    /// `<archive>!` at the root, `<archive>!/<inner>` below it.
    pub fn synthetic_path(&self) -> String {
        if self.inner.is_empty() {
            format!("{}!", self.archive_path)
        } else {
            format!("{}!/{}", self.archive_path, self.inner)
        }
    }

    /// Parse a navigation target back into an inner path. `None` means
    /// the target is NOT inside this archive (a real path: the caller
    /// leaves browse mode and navigates normally).
    pub fn inner_from_synthetic(&self, path: &str) -> Option<String> {
        let rest = path.strip_prefix(&self.archive_path)?;
        let rest = rest.strip_prefix('!')?;
        if rest.is_empty() {
            return Some(String::new());
        }
        // Tolerate both separators: local PathBuf joins use the OS one.
        let rest = rest.strip_prefix(['/', '\\'])?;
        Some(rest.replace('\\', "/"))
    }
}

/// Payload of `Message::Sftp(SftpMessage::ZipIndexed)`: the parsed index plus, for
/// remote archives, the live ranged-read handle to keep for the browse.
#[derive(Clone)]
pub(crate) struct ZipIndexedPayload {
    pub index: Arc<oryxis_archive::browse::ZipIndex>,
    pub remote_src: Option<Arc<oryxis_ssh::RemoteRangedFile>>,
}

impl std::fmt::Debug for ZipIndexedPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZipIndexedPayload")
            .field("entries", &self.index.entries.len())
            .finish_non_exhaustive()
    }
}

/// State for the SFTP browser. Two panes, side-by-side: the left pane is
/// Local by default but can be switched to any host; the right pane is
/// always a remote host. When both panes are remote, a transfer between
/// them uses the server-to-server relay primitive instead of
/// upload/download.
pub(crate) struct SftpState {
    /// Left pane, Local by default.
    pub left: PaneState,
    /// Right pane, a remote host (never Local).
    pub right: PaneState,
    /// True while the host picker overlay is visible (default at boot,
    /// hidden once a host is chosen).
    pub picker_open: bool,
    /// Which pane the currently open picker is choosing a host for.
    pub picker_target: SftpPaneSide,
    /// Search filter applied to the host picker.
    pub picker_search: String,
    /// Right-click row context menu, anchored to the click location
    /// and operating on a specific entry.
    pub row_menu: Option<SftpRowMenu>,
    /// Inline rename editor, replaces the row visually with a text
    /// input until the user commits or cancels.
    pub rename: Option<SftpRename>,
    /// Pending destructive action, surfaces a confirmation modal.
    /// `Vec` (instead of `Option`) so the same modal handles both single
    /// right-click delete and bulk delete from a multi-selection, the
    /// modal copy adapts to the count.
    pub delete_confirm: Vec<SftpDeleteTarget>,
    /// New file / new folder modal, kind + in-progress name input.
    pub new_entry: Option<SftpNewEntry>,
    /// True while OS files are being dragged over the window. Drives the
    /// remote-pane drop highlight; cleared on `FilesHoveredLeft` or
    /// `FileDropped`.
    pub drop_active: bool,
    /// OS-drop burst collector: Windows delivers one FileDropped per
    /// file of a multi-select drop, so they're accumulated here and
    /// flushed as a single batch by `SftpDropFlush` (150ms debounce).
    pub pending_drops: Vec<std::path::PathBuf>,
    /// Currently hovered row across both panes. Updated continuously
    /// from MouseArea on_enter / on_exit on every visible row, and
    /// consumed by both the OS drop target picker and the internal
    /// drag-drop release handler.
    pub hovered_row: Option<(SftpPaneSide, String, bool)>,
    /// Identity of the row the current left press landed on, written by
    /// each row's `press_hit_reporter` wrapper and taken (consumed) by
    /// `SftpMouseLeftPressed`, which fires once per press.
    ///
    /// This is what makes grabbing a row for a drag deterministic.
    /// `hovered_row` cannot do that job: it is hover state maintained by
    /// MouseArea enter / exit, so a truncated name's tooltip overlay
    /// drops it and iced publishing transitions in tree order reorders
    /// it. Draw-time rects (`bounds_reporter`) cannot either: under a
    /// scrollable they are content-space while the mouse is screen-space,
    /// so a scrolled list hit the wrong row by exactly the scroll offset
    /// (issue #127). The press-time test happens where iced itself
    /// translates the cursor, so it is immune to both failure modes.
    pub row_press: crate::widgets::PressHitCell<(SftpPaneSide, String, bool)>,
    /// In-progress internal drag (file/folder being dragged from one
    /// pane to the other). Spans the press → drop window.
    pub drag: Option<SftpInternalDrag>,
    /// Folder transfer in progress (upload / download / local duplicate).
    /// Drives the bottom-of-view progress bar and serializes the queue
    /// of per-item operations so the SFTP connection isn't slammed.
    pub transfer: TransferSlot,
    /// One-shot destination override for the next upload, set by the
    /// drag-and-drop handler when the cursor lands on a specific remote
    /// folder, consumed by `SftpUpload` / `SftpUploadFolder`.
    pub upload_dest_override: Option<String>,
    /// Same idea for downloads, set when an internal drag from the
    /// remote pane lands on a specific local folder. Consumed by
    /// `SftpDownload` / `SftpDownloadFolder`.
    pub download_dest_override: Option<std::path::PathBuf>,
    /// Multi-row selection across both panes. Plain click on a file
    /// replaces this with a single entry; ctrl-click toggles; shift-click
    /// extends from `selection_anchor` within the same pane. Cleared
    /// whenever either pane navigates away.
    pub selected_rows: Vec<(SftpPaneSide, String)>,
    /// Last clicked row, origin point for shift-click range extension.
    /// Stays put across ctrl-click toggles so the range pivots from the
    /// initial selection point rather than the most recent toggle.
    pub selection_anchor: Option<(SftpPaneSide, String)>,
    /// Files opened in a local application from this surface: downloaded
    /// to a temp path, watched in the background (the surface stays fully
    /// usable) and confirmed per save through the save dialog,
    /// FileZilla-style. Multiple files at once.
    pub edit_watches: Vec<EditSession>,
    /// Pending overwrite confirmation, set when the user uploads a file
    /// whose name already exists in the destination. Cleared when the
    /// user picks an action.
    pub overwrite_prompt: Option<OverwritePrompt>,
    /// Open Properties modal for a single row. Carries the snapshot
    /// of the current metadata + the user's in-progress edits to the
    /// permission bits so Apply can diff.
    pub properties: Option<PropertiesView>,
    /// Type-ahead search buffer: characters typed while a row is selected,
    /// used to jump the selection to the first matching entry. Reset after
    /// a short pause between keystrokes.
    pub type_ahead: String,
    /// Instant of the last type-ahead keystroke, for the reset timeout.
    pub type_ahead_at: Option<std::time::Instant>,
    /// The previous completed type-ahead sequence. When the user re-types
    /// the same string (after a pause), the search advances to the next
    /// match instead of restarting, so repeated typing cycles results.
    pub type_ahead_committed: String,
    /// Judges each row press against the previous one, on the OS's
    /// double-click terms (`crate::double_click`). Fed by the rows'
    /// `SftpRowPressed`, which runs ahead of the row's own release.
    pub click_tracker: crate::double_click::ClickTracker<SftpClickKey>,
    /// What the LAST row press was, waiting for that row's release
    /// (`SftpSelectRow` / `SftpParentRowClicked`) to act on it. Every
    /// press overwrites it, and a release takes it only when the key
    /// matches, so a press that ended in a drag elsewhere is never read
    /// by a later click.
    pub press_verdict: Option<(SftpClickKey, crate::double_click::ClickReport)>,
    /// Row armed for inline rename by a slow second click (`(side, path)`):
    /// set on the press, deferred on release (iff no drag activated, so
    /// dragging an already-selected row still works) and only then armed
    /// as a real rename, see `click_gen`.
    pub pending_rename: Option<(SftpPaneSide, String)>,
    /// Row whose file NAME LABEL (the drawn text, not the whole row) is
    /// under the cursor. The slow-click rename only arms over the name,
    /// the way Explorer / Finder hit-test their label rect, so a click on
    /// the Size / Modified / empty area of an already-selected row can
    /// never start an edit.
    pub hovered_name: Option<(SftpPaneSide, String)>,
    /// Swallow the next row-activation keypress. Set when an inline input
    /// (rename / new-entry) commits on Enter: the same physical Enter also
    /// reaches the global keyboard subscription, which would otherwise
    /// activate the still-selected row (re-opening the just-renamed file).
    /// Consumed by the first keyboard event that follows the commit.
    pub swallow_next_activate: bool,
    /// Generation counter for the debounced type-ahead search. Each
    /// keystroke bumps it and schedules a deferred fire; only the fire
    /// whose generation still matches runs, so fast typing searches once
    /// (with the full buffer) instead of jumping on every key.
    pub type_ahead_gen: u64,
    /// Set when the last type-ahead keystroke repeated the same single
    /// character (Windows-Explorer style): the debounced search then advances
    /// to the *next* match for that character instead of narrowing the buffer,
    /// so pressing one letter repeatedly cycles through all its matches.
    pub type_ahead_cycle: bool,
    /// FileZilla-style message log for this SFTP tab: connect / list /
    /// transfer / error events. In-memory only, capped to the most recent
    /// `SFTP_LOG_CAP` entries. Shown when `log_open`.
    pub log: Vec<SftpLogEntry>,
    /// Whether the message-log panel at the bottom of the view is open.
    pub log_open: bool,
    /// Height of the message-log panel in pixels, resizable via the divider
    /// above it (issue #45). Clamped to [`SFTP_LOG_MIN_H`, `SFTP_LOG_MAX_H`].
    pub log_height: f32,
    /// Which pane currently holds keyboard focus for arrow / Enter / Tab
    /// navigation. Tab toggles it; a mouse click on a row follows it.
    /// Distinct from `selected_rows` so focus survives an empty pane (Tab
    /// can land on a pane with nothing selected yet).
    pub focused_side: SftpPaneSide,
    /// True when the keyboard cursor sits on the `..` (parent) row of the
    /// focused pane rather than on a real entry. Mutually exclusive with a
    /// selected row in that pane: setting it clears `selected_rows`.
    pub parent_cursor: bool,
    /// Suppress the mouse-hover row highlight after a keyboard navigation
    /// key, so the hover under a stationary cursor doesn't fight the
    /// keyboard cursor. Cleared on the next real mouse move.
    pub suppress_hover: bool,
    /// Where to drop the keyboard cursor once a pane's *next* directory
    /// listing loads. Set by folder descent (Right / Enter) and back-
    /// navigation so the cursor follows the move; consumed by the load
    /// handler (remote loads are async, so it can't be applied inline).
    pub pending_focus: Option<(SftpPaneSide, SftpPendingFocus)>,
    /// Screen-space bounds of the row (or the `..` parent row) that holds
    /// the keyboard cursor in the focused pane, written every frame by a
    /// `bounds_reporter` wrapper around that one row. Read when the Menu
    /// key / Shift+F10 opens the row context menu so it anchors on the
    /// focused row instead of the (irrelevant) mouse position.
    pub focus_row_bounds: crate::widgets::BoundsCell,
}

/// A row of the dual-pane browser as the double-click detector sees it:
/// the `..` row has no path, so it is its own key rather than a sentinel
/// string that a real entry could collide with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SftpClickKey {
    Parent(SftpPaneSide),
    Entry(SftpPaneSide, String),
}

/// The keyboard cursor target to apply after a directory listing loads.
#[derive(Debug, Clone)]
pub(crate) enum SftpPendingFocus {
    /// The `..` parent row (Enter / Right descent into a folder).
    Parent,
    /// The entry with this full path, if still present, used by back-
    /// navigation to land on the folder we just came out of. Falls back to
    /// the first entry, then `..`, when the path isn't in the new listing.
    Path(String),
}

/// Cap on retained SFTP log lines per tab; older lines are dropped.
pub(crate) const SFTP_LOG_CAP: usize = 500;

/// Default / min / max height for the resizable message-log panel.
pub(crate) const SFTP_LOG_DEFAULT_H: f32 = 160.0;
pub(crate) const SFTP_LOG_MIN_H: f32 = 80.0;
pub(crate) const SFTP_LOG_MAX_H: f32 = 600.0;

impl Default for SftpState {
    fn default() -> Self {
        // The derived `Default` for `PaneState` gives `is_remote == false`
        // for both panes; the right pane is always remote, so it has to be
        // constructed explicitly. Hand-writing this guarantees every
        // default site (tests, resets, boot) gets a correctly-natured
        // right pane.
        Self {
            left: PaneState {
                is_remote: false,
                ..Default::default()
            },
            right: PaneState {
                is_remote: true,
                ..Default::default()
            },
            picker_open: false,
            picker_target: SftpPaneSide::Right,
            picker_search: String::new(),
            row_menu: None,
            rename: None,
            delete_confirm: Vec::new(),
            new_entry: None,
            drop_active: false,
            pending_drops: Vec::new(),
            hovered_row: None,
            row_press: crate::widgets::new_press_hit_cell(),
            drag: None,
            transfer: TransferSlot::default(),
            upload_dest_override: None,
            download_dest_override: None,
            selected_rows: Vec::new(),
            selection_anchor: None,
            edit_watches: Vec::new(),
            overwrite_prompt: None,
            properties: None,
            type_ahead: String::new(),
            type_ahead_at: None,
            type_ahead_committed: String::new(),
            click_tracker: Default::default(),
            press_verdict: None,
            pending_rename: None,
            hovered_name: None,
            swallow_next_activate: false,
            type_ahead_gen: 0,
            type_ahead_cycle: false,
            log: Vec::new(),
            log_open: false,
            log_height: SFTP_LOG_DEFAULT_H,
            focused_side: SftpPaneSide::Left,
            parent_cursor: false,
            suppress_hover: false,
            pending_focus: None,
            focus_row_bounds: crate::widgets::new_bounds_cell(),
        }
    }
}

impl SftpState {
    /// The cursor left this name label: drop the highlight only while that
    /// label is still the one holding it. The probes are one per row in a
    /// single `Column`, so walking UP the list delivers the arriving row's
    /// enter BEFORE the departing row's exit (see `HoverState::leave`),
    /// and an unconditional clear would disarm the slow-click rename the
    /// cursor had just armed. `hovered_row` next door learned this the
    /// hard way, see `SftpRowExit`.
    pub(crate) fn leave_name(&mut self, name: (SftpPaneSide, String)) {
        if self.hovered_name.as_ref() == Some(&name) {
            self.hovered_name = None;
        }
    }

    pub(crate) fn pane(&self, side: SftpPaneSide) -> &PaneState {
        match side {
            SftpPaneSide::Left => &self.left,
            SftpPaneSide::Right => &self.right,
        }
    }

    pub(crate) fn pane_mut(&mut self, side: SftpPaneSide) -> &mut PaneState {
        match side {
            SftpPaneSide::Left => &mut self.left,
            SftpPaneSide::Right => &mut self.right,
        }
    }

    /// Dismiss every transient overlay menu: the row/background context
    /// menu and both panes' `⋮` actions + drive-picker dropdowns. Called
    /// by any menu action so the menu always closes on click.
    pub(crate) fn close_menus(&mut self) {
        self.row_menu = None;
        self.left.actions_open = false;
        self.right.actions_open = false;
        self.left.drives_open = false;
        self.right.drives_open = false;
        self.left.filter_open = false;
        self.right.filter_open = false;
        self.left.path_history_open = false;
        self.right.path_history_open = false;
    }

    /// The side of the remote pane used as an upload destination /
    /// download source. With the current model the right pane is always
    /// remote, and the left pane can also be remote; the upload/download
    /// paths only run with exactly one remote and one local pane, so we
    /// return the first remote side, preferring the right (the canonical
    /// remote pane). Returns `None` if neither pane is remote.
    pub(crate) fn remote_side(&self) -> Option<SftpPaneSide> {
        if self.right.is_remote {
            Some(SftpPaneSide::Right)
        } else if self.left.is_remote {
            Some(SftpPaneSide::Left)
        } else {
            None
        }
    }

    /// The side of the local pane (download destination / upload source).
    /// Returns `None` if neither pane is local.
    pub(crate) fn local_side(&self) -> Option<SftpPaneSide> {
        if !self.left.is_remote {
            Some(SftpPaneSide::Left)
        } else if !self.right.is_remote {
            Some(SftpPaneSide::Right)
        } else {
            None
        }
    }
}

/// Per-bit permission state shown by the Properties dialog. Maps 1-1
/// onto the POSIX rwxrwxrwx octal so Apply can rebuild a `u32` mode.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PermBits {
    pub user_r: bool,
    pub user_w: bool,
    pub user_x: bool,
    pub group_r: bool,
    pub group_w: bool,
    pub group_x: bool,
    pub other_r: bool,
    pub other_w: bool,
    pub other_x: bool,
}

impl PermBits {
    pub fn from_mode(mode: u32) -> Self {
        Self {
            user_r: mode & 0o400 != 0,
            user_w: mode & 0o200 != 0,
            user_x: mode & 0o100 != 0,
            group_r: mode & 0o040 != 0,
            group_w: mode & 0o020 != 0,
            group_x: mode & 0o010 != 0,
            other_r: mode & 0o004 != 0,
            other_w: mode & 0o002 != 0,
            other_x: mode & 0o001 != 0,
        }
    }
    pub fn to_mode(self) -> u32 {
        let mut m = 0u32;
        if self.user_r { m |= 0o400; }
        if self.user_w { m |= 0o200; }
        if self.user_x { m |= 0o100; }
        if self.group_r { m |= 0o040; }
        if self.group_w { m |= 0o020; }
        if self.group_x { m |= 0o010; }
        if self.other_r { m |= 0o004; }
        if self.other_w { m |= 0o002; }
        if self.other_x { m |= 0o001; }
        m
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PermBit {
    UserR, UserW, UserX,
    GroupR, GroupW, GroupX,
    OtherR, OtherW, OtherX,
}

#[derive(Debug, Clone)]
pub(crate) struct PropertiesView {
    pub side: SftpPaneSide,
    /// When set, Apply chmods through THIS client instead of resolving
    /// `side` against the live SFTP buffer: the terminal sidebar Files
    /// browser owns its own channel and its ops must not touch another
    /// surface's panes.
    pub client_override: Option<SftpClient>,
    /// Set when opened from the sidebar Files browser; the post-apply
    /// refresh re-lists the sidebar instead of an SFTP pane.
    pub from_sidebar: bool,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: Option<u32>,
    pub owner_uid: Option<u32>,
    pub owner_gid: Option<u32>,
    /// Original mode bits, used to detect unchanged Apply (no-op) and
    /// preserve the high bits (setuid/setgid/sticky) the dialog doesn't
    /// edit.
    pub original_mode: u32,
    pub bits: PermBits,
    /// Editable octal mode string (the numeric value the user can type
    /// directly, WinSCP-style). Two-way bound with `bits`: typing a valid
    /// octal value rewrites `bits`, and toggling a checkbox rewrites this.
    pub mode_input: String,
    /// True while the chmod task is in flight, disables the Apply
    /// button so the user can't double-fire.
    pub applying: bool,
    pub error: Option<String>,
}

/// Which way the conflicting transfer runs. The modal reads it to map the
/// two sizes onto its "Local · Remote" labels, and the resolve handler to
/// pick which side it writes to (an SFTP upload or a local download).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverwriteDirection {
    /// The file being written comes from the local filesystem, so
    /// `dst_dir` is a remote POSIX directory.
    Upload,
    /// The file being written comes from the remote host, so `dst_dir`
    /// is a local directory.
    Download,
}

#[derive(Debug, Clone)]
pub(crate) struct OverwritePrompt {
    /// Destination directory, as a string on both sides: a remote POSIX
    /// directory for an upload, a local one for a download (a local path
    /// does not survive a round trip through `String` on every platform,
    /// and `TransferItem` already stores both directions this way). The
    /// source path lives on the parked `TransferItem`, which is what the
    /// answer is applied to.
    pub dst_dir: String,
    pub basename: String,
    /// Size of the source file: local for an upload, remote for a
    /// download. The modal labels it by `direction`, not by field name.
    pub src_size: u64,
    /// Size of the file already sitting at the destination.
    pub dst_size: u64,
    pub direction: OverwriteDirection,
    /// True when the prompt is part of a multi-file transfer, surfaces
    /// the "apply to remaining" checkbox so the user doesn't have to
    /// re-answer for every collision.
    pub multi: bool,
    /// User-toggled state of the "apply to remaining" checkbox while
    /// the modal is open. Read on resolve; persisted as
    /// `TransferState.overwrite_default` if true.
    pub apply_to_all: bool,
    /// Who asked. Stamped when the prompt is stored, and carried back on
    /// every button the modal draws.
    ///
    /// The answer has to reach the SAME owner the question came from,
    /// and it cannot be re-derived when the click arrives: a sidebar
    /// transfer routinely runs with NO SFTP surface focused, so an
    /// unaddressed answer resolves to no owner at all, the transfers
    /// handler declines it, and the queue stays paused forever with its
    /// bar frozen. `None` only for prompts built before this field
    /// existed in a given flow; those fall back to the live buffer.
    pub owner: Option<uuid::Uuid>,
    /// When set, the conflict came from a terminal / sidebar Files drop
    /// upload (`begin_drop_sftp_upload`) on this pane, and resolving it
    /// resumes that upload instead of a transfer queue. The modal's
    /// answer stays unaddressed (`owner: None`) so it lands in the
    /// plain SFTP handler, which dispatches on this field.
    pub drop_upload_pane: Option<uuid::Uuid>,
}

impl OverwritePrompt {
    /// The two sizes mapped onto the conflict modal's fixed
    /// "Local · Remote" labels. `src_size` is the file being transferred,
    /// so which side it sits on flips with the direction: a download's
    /// source is the REMOTE file, and reporting it as the local one would
    /// invert the only number the user has to decide on.
    pub fn local_remote_sizes(&self) -> (u64, u64) {
        match self.direction {
            OverwriteDirection::Upload => (self.src_size, self.dst_size),
            OverwriteDirection::Download => (self.dst_size, self.src_size),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverwriteAction {
    /// Always overwrite the existing file.
    Replace,
    /// Only overwrite if the existing remote size differs from the local
    /// size, cheap proxy for "is it actually a different file?" without
    /// hashing both sides.
    ReplaceIfDifferent,
    /// Upload alongside with a "name copy" suffix instead of overwriting.
    Duplicate,
    /// Continue the file already there instead of replacing it, treating
    /// it as an interrupted copy of this one.
    ///
    /// Uploads only, and only when the destination is SHORTER than the
    /// source. Downloads resume by themselves from their own scratch
    /// file, so a completed file sitting at a download's destination is
    /// not a partial of anything and this is never offered for one.
    ///
    /// Never sticky: what the engine can check is that the destination's
    /// last block matches the source's, which is a claim about ONE pair
    /// of files. Applying it to files it has not seen would be a guess,
    /// and the thing being guessed at is whether to splice two files
    /// together.
    Resume,
    /// Don't upload at all.
    Cancel,
}

/// One file opened in a local application from a remote listing, watched
/// in the background: the surface stays fully usable while the editor
/// runs, and every save the watcher notices asks whether to send the file
/// back (FileZilla / MobaXterm semantics). Lives in `edit_watches`; there
/// is no blocking variant.
#[derive(Debug, Clone)]
pub(crate) struct EditSession {
    /// Client the upload goes through, captured at open time. A watch
    /// outlives the pane's own state (a parked tab keeps uploading), so it
    /// never re-resolves its channel from a pane. Every registered watch
    /// carries one; `None` only exists so the type stays constructible in
    /// tests, and the upload refuses it explicitly.
    pub client: Option<SftpClient>,
    pub remote_path: String,
    pub temp_path: std::path::PathBuf,
    /// Display label shown in the dialogs, basename of the remote file.
    pub label: String,
    /// Host label the file came from, shown by the save-confirmation
    /// dialog ("replace the remote file on {host}?"). Together with
    /// `remote_path` it identifies the file across surfaces.
    pub host: String,
    /// How the local application was launched, replayed verbatim when the
    /// user reopens the same file.
    pub opener: SftpEditOpener,
    /// Mtime of the temp file when it was first written (right after
    /// download). The watcher tick polls this to detect saves coming
    /// from the user's editor, and re-arms it after every upload / skip
    /// so each new save prompts again.
    pub initial_mtime: Option<std::time::SystemTime>,
    /// True once the watcher tick observes an mtime newer than
    /// `initial_mtime`: a save is waiting for the user's answer.
    pub dirty: bool,
    /// A watch upload is in flight; the tick and the dialog skip the
    /// entry until the completion message re-arms it.
    pub uploading: bool,
}

/// Collision dialog shown when the user opens a remote file that is
/// already being edited: the local temp copy exists and is watched, so
/// re-downloading it blind would throw away unsaved work (FileZilla asks
/// the same question).
#[derive(Debug, Clone)]
pub(crate) struct EditReopenPrompt {
    /// Temp file of the live watch this collides with, its identity.
    pub temp_path: std::path::PathBuf,
    /// Basename shown in the dialog.
    pub label: String,
    pub remote_path: String,
    pub host: String,
    /// True when the watch is holding a save that was never uploaded, the
    /// case where discarding the local copy actually loses work.
    pub pending_save: bool,
    /// Opener the live watch was launched with. Reopen replays THIS one so
    /// the edit continues in the application that already holds the file,
    /// whichever action the user used to come back to it.
    pub watch_opener: SftpEditOpener,
    /// Everything needed to replay the open on either branch. `opener` is
    /// the one the new request asked for, used by the fresh download.
    pub side: SftpPaneSide,
    pub opener: SftpEditOpener,
    pub client: SftpClient,
    /// Report failures of the replayed open as a toast instead of a pane
    /// error banner (the sidebar Files browser has no banner).
    pub to_toast: bool,
}

/// A button of the reopen-or-redownload dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SftpEditReopenChoice {
    /// Launch the application again on the existing local copy, keeping
    /// the watch (and any pending save) intact.
    Reopen,
    /// Drop the watch, delete the local copy and download the remote file
    /// again.
    Fresh,
    /// Do nothing.
    Cancel,
}

/// How "Open with..." resolves the local application for a remote file
/// (issue #84). `OsDefault` is the file-association open the classic
/// edit flow already used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SftpEditOpener {
    /// The single configured editor from Settings > SFTP.
    ConfiguredEditor,
    /// One specific application, chosen for this open only (issue #114).
    /// It carries the path rather than re-asking, so reopening the same
    /// file lands back in the application that already holds it instead
    /// of raising the file picker a second time.
    Editor(String),
    /// The OS "open with" application picker (Windows / macOS only).
    AskOs,
    /// The OS file association (`open::that`).
    OsDefault,
}

/// A button of the MobaXterm-style save-confirmation dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SftpEditPromptChoice {
    /// Upload this save.
    Yes,
    /// Upload this and every later save this app run, without asking.
    YesToAll,
    /// Persist auto-upload (Settings > SFTP toggle turns it back off).
    Autosave,
    /// Skip this save, keep watching.
    No,
    /// Stop watching this file (the temp copy stays on disk).
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransferKind {
    Upload,
    Download,
    /// Local-side `cp -r` equivalent, `std::fs` doesn't expose recursive
    /// copy so we walk the tree and copy each entry ourselves.
    DuplicateLocal,
    /// Server-to-server transfer: both `src` and `dst` are remote POSIX
    /// paths, on the source pane's host and the dest pane's host
    /// respectively. The runner streams via `SftpClient::relay_to`.
    Relay,
}

#[derive(Debug, Clone)]
pub(crate) struct TransferItem {
    /// Source path. For uploads/local-duplicate this is a local path;
    /// for downloads it's a remote POSIX path.
    pub src: String,
    /// Destination path. Mirrors the side rules of `src` swapped.
    pub dst: String,
    /// Folders are processed by ensuring the destination directory exists;
    /// files are read+written.
    pub is_dir: bool,
    /// Remote file size, populated only for download items from the
    /// directory listing that was walked. Passed to `download_to` as a
    /// hint so each file skips an extra `stat` round trip. `None` for
    /// uploads, local duplicates, and directories.
    pub size: Option<u64>,
}

/// One owner's transfer and everything drawn alongside it.
///
/// The dual-pane surface keeps these as loose fields on `SftpState` for
/// historical reasons; a new owner gets them grouped, and
/// `Oryxis::transfer_*` reads either shape. Only the queue runner and the
/// conflict resolver go through those accessors: the dual-pane entry
/// points (row menus, drag and drop, OS drops) are surface-specific and
/// keep touching `self.sftp` directly.
#[derive(Debug, Clone, Default)]
pub(crate) struct TransferSlot {
    pub state: Option<TransferState>,
    /// Bytes moved so far, written by the SFTP engine as chunks land and
    /// polled a few times a second for the bar. Replaced (never reset) per
    /// transfer, so a lingering worker from a cancelled one cannot spike
    /// the successor's count.
    pub bytes_done: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Sum of the known item sizes; 0 means "unknown", and the bar falls
    /// back to counting items.
    pub bytes_total: u64,
    /// Labels of the items finished so far, for the per-file panel.
    pub done_log: Vec<String>,
    pub panel_open: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct TransferState {
    pub kind: TransferKind,
    /// Top-level label shown in the progress bar, e.g. "my-folder".
    pub root_label: String,
    /// Pending items, popped one at a time as each operation completes.
    pub queue: std::collections::VecDeque<TransferItem>,
    /// Name of the item currently being processed; `None` between items.
    pub current: Option<String>,
    pub completed: usize,
    pub total: usize,
    /// Sticky overwrite decision, set when the user checks "Apply to
    /// remaining" in the conflict modal. Subsequent collisions auto-
    /// resolve with this action without re-prompting.
    pub overwrite_default: Option<OverwriteAction>,
    /// When `Some`, the current item has been popped and is waiting for
    /// the user to resolve a conflict modal. The path/size info is
    /// captured here so the resolve handler can reapply the action to
    /// the right destination without re-listing.
    pub pending_conflict_item: Option<TransferItem>,
    /// Slot that hit the conflict, needed so resolve uses the same
    /// SFTP client channel for the apply step.
    pub pending_conflict_slot: Option<u8>,
    /// One SFTP client per parallel slot. Empty for `DuplicateLocal`
    /// (no SFTP needed). For `Upload`/`Download` size is `concurrency`.
    /// For `Relay` these are the *source* host's clients.
    pub clients: Vec<SftpClient>,
    /// Destination-host SFTP client, populated only for `Relay`. The
    /// relay runs at concurrency 1 (a single dest client would otherwise
    /// contend on its inner lock / raw sessions across slots), so one
    /// dest client is enough.
    pub dest_client: Option<SftpClient>,
    /// Destination pane for a `Relay`. Needed so the finalize / cancel /
    /// error arms refresh the *destination* pane: a right-to-left relay
    /// has its destination on the left, which `remote_side()` (which
    /// prefers Right) would not pick. `None` for non-relay transfers.
    pub dest_side: Option<SftpPaneSide>,
    /// Per-slot "is in flight" flag. The Next handler picks the first
    /// `false` slot to dispatch to, keeping each slot mapped 1-1 with
    /// its `clients[i]` so workers never fight for the same channel.
    pub busy_slots: Vec<bool>,
    /// True while a conflict modal is up, workers exit on Next instead
    /// of popping more items, then get re-spawned by Resolve.
    pub paused: bool,
    /// Slot currently running a *directory* item. Directory items are
    /// ordering barriers: the tree walks enqueue a dir before its
    /// children (and after its own parent), so a dir may only start
    /// once every earlier item finished, and nothing behind it may
    /// start until it exists. While this is `Some`, Next dispatches
    /// nothing; ItemDone for the slot clears it and refills the pool.
    /// Without the barrier two slots would run `mkdir a` and
    /// `mkdir a/b` concurrently: the child mkdir fails on the missing
    /// parent and every upload into it dies with "No such file"
    /// (issue #63).
    pub dir_slot: Option<u8>,
    /// Set on a `Relay` that is a MOVE: the source paths to remove once
    /// the whole queue has drained, files first and directories after,
    /// deepest first. `None` on a copy, which never removes anything.
    ///
    /// The list is captured by the same walk that built the queue, so it
    /// describes exactly what was copied and cannot drift from it. It is
    /// only ever consumed from the finalize arm, which is reached solely
    /// when every item completed: any error clears `transfer` outright,
    /// so a failed move leaves the source untouched by construction
    /// (issue #115).
    pub move_sources: Option<Vec<TransferItem>>,
}

impl TransferState {
    /// Build a fresh transfer. `total` is derived from the queue and
    /// `busy_slots` gets one flag per slot (kept 1-1 with the dispatch
    /// loop's `clients`); all the progress / conflict fields start empty.
    /// `slots` is the parallel-worker count (1 for DuplicateLocal/Relay).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        kind: TransferKind,
        root_label: String,
        queue: std::collections::VecDeque<TransferItem>,
        clients: Vec<SftpClient>,
        dest_client: Option<SftpClient>,
        dest_side: Option<SftpPaneSide>,
        slots: u8,
    ) -> Self {
        Self {
            kind,
            root_label,
            total: queue.len(),
            queue,
            current: None,
            completed: 0,
            overwrite_default: None,
            pending_conflict_item: None,
            pending_conflict_slot: None,
            clients,
            dest_client,
            dest_side,
            busy_slots: vec![false; slots as usize],
            paused: false,
            dir_slot: None,
            move_sources: None,
        }
    }

    /// Turn a freshly built relay into a MOVE by attaching the source
    /// paths to remove after the copy is verified. Kept as a builder
    /// step rather than another `new` parameter so a copy can never
    /// acquire a removal list by an argument slipping one position.
    pub fn moving(mut self, sources: Vec<TransferItem>) -> Self {
        self.move_sources = Some(sources);
        self
    }
}

/// Which pane (by position) a side-addressed SFTP message / state item
/// refers to. This is *position* only; whether a pane is Local or remote
/// is its `PaneState::is_remote` flag, not its side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SftpPaneSide {
    Left,
    #[default]
    Right,
}

/// Internal drag state, a row being dragged from one pane towards the
/// other. The press position lets us suppress short jitters; only past
/// a small threshold do we treat the press+move as a drag rather than a
/// click. Multi-row drags carry the full set so a single drop fires N
/// transfers.
#[derive(Debug, Clone)]
pub(crate) struct SftpInternalDrag {
    pub origin_side: SftpPaneSide,
    /// `(path, is_dir)` per dragged item.
    pub items: Vec<(String, bool)>,
    /// Short label shown on the floating ghost, basename or "N items".
    pub label: String,
    /// Cursor position at left-press time. Used to gate `active` on
    /// distance threshold so accidental jitter doesn't get treated as
    /// a drag and steal click handling.
    pub press_pos: iced::Point,
    /// Once the cursor moves past a few pixels we commit to the drag
    /// the ghost renders, the drop highlight kicks in, and the eventual
    /// release dispatches a transfer instead of a click.
    pub active: bool,
}

/// In-progress reorder drag of a column header. Armed on header press,
/// promoted to `active` once the cursor moves past a threshold (so a plain
/// click still sorts). On release the dragged column moves to whichever
/// header the cursor is hovering (`hovered_col`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct SftpColDrag {
    pub side: SftpPaneSide,
    pub col: SftpColumn,
    pub press_x: f32,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct SftpRowMenu {
    pub side: SftpPaneSide,
    /// Stringified path, `String` for both panes since the modal /
    /// follow-up actions accept a path verbatim.
    pub path: String,
    pub is_dir: bool,
    /// Set when the menu was opened by right-clicking the empty area of
    /// the pane (not a row). The view then shows only directory-level
    /// actions and `path` holds the pane's current directory.
    pub is_background: bool,
    /// The "Open with" family is expanded (issue #114). Collapsed by
    /// default so the common one-click "Open / Edit" stays at the top of
    /// a menu that is already long; opening the group keeps the menu on
    /// screen, like the Columns toggles do.
    pub open_group: bool,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone)]
pub(crate) struct SftpRename {
    pub side: SftpPaneSide,
    /// Original full path; we rebuild the parent + new name on commit.
    pub original_path: String,
    pub input: String,
}

/// Target of the SFTP close-guard confirmation modal: a single tab, "close
/// every tab except this one", or a hybrid terminal tab's SFTP session.
/// Drives `pending_sftp_close`.
#[derive(Debug, Clone, Copy)]
pub(crate) enum PendingSftpClose {
    /// Close just the tab at this index.
    One(usize),
    /// Close every tab except the one at this index.
    Others(usize),
    /// Close ONLY the SFTP session of the hybrid terminal tab with this
    /// id (the tab itself stays). Keyed by id, not index: the strip can
    /// reorder while the modal is up.
    HybridSession(uuid::Uuid),
}

#[derive(Debug, Clone)]
pub(crate) struct SftpDeleteTarget {
    pub side: SftpPaneSide,
    pub path: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SftpEntryKind {
    File,
    Folder,
}

#[derive(Debug, Clone)]
pub(crate) struct SftpNewEntry {
    pub side: SftpPaneSide,
    pub kind: SftpEntryKind,
    pub input: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SftpSortColumn {
    Name,
    Modified,
    Size,
    // The optional columns sort too (issue #143): Kind by the same
    // label the cell shows, Permissions by the numeric mode,
    // Owner by uid then gid.
    Kind,
    Permissions,
    Owner,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SftpSort {
    pub column: SftpSortColumn,
    pub ascending: bool,
}

impl Default for SftpSort {
    fn default() -> Self {
        Self {
            column: SftpSortColumn::Name,
            ascending: true,
        }
    }
}

/// Sort modes available for the Hosts / Keychain / Snippets card
/// grids. Persisted per-list in the `settings` table under
/// `hosts_sort` / `keys_sort` / `snippets_sort` as the value of
/// `ListSort::as_storage_str()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ListSort {
    #[default]
    LabelAsc,
    LabelDesc,
    NewestFirst,
    OldestFirst,
}

impl ListSort {
    pub fn as_storage_str(self) -> &'static str {
        match self {
            ListSort::LabelAsc => "label_asc",
            ListSort::LabelDesc => "label_desc",
            ListSort::NewestFirst => "newest_first",
            ListSort::OldestFirst => "oldest_first",
        }
    }

    pub fn from_storage_str(s: &str) -> Self {
        match s {
            "label_desc" => ListSort::LabelDesc,
            "newest_first" => ListSort::NewestFirst,
            "oldest_first" => ListSort::OldestFirst,
            _ => ListSort::LabelAsc,
        }
    }

    /// Sort `items` in place using the row's label + creation
    /// timestamp. Labels are lowercased before comparison so case
    /// differences don't reorder rows the user thinks of as equal.
    pub fn sort_items<T, FLabel, FTime>(
        self,
        items: &mut [T],
        mut label_of: FLabel,
        mut created_at: FTime,
    ) where
        FLabel: FnMut(&T) -> String,
        FTime: FnMut(&T) -> chrono::DateTime<chrono::Utc>,
    {
        match self {
            // `sort_by_cached_key` lowercases each label once per item
            // instead of allocating two fresh Strings per comparison;
            // these sorts run on every redraw of the dashboard / keys /
            // snippets views.
            ListSort::LabelAsc => {
                items.sort_by_cached_key(|i| label_of(i).to_lowercase())
            }
            ListSort::LabelDesc => items.sort_by_cached_key(|i| {
                std::cmp::Reverse(label_of(i).to_lowercase())
            }),
            ListSort::NewestFirst => {
                items.sort_by_key(|i| std::cmp::Reverse(created_at(i)))
            }
            ListSort::OldestFirst => items.sort_by_key(|i| created_at(i)),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LocalEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
    /// Unix mode bits (`st_mode`), populated only on Unix; `None` on
    /// Windows where there is no POSIX mode. Drives the Permissions column.
    pub mode: Option<u32>,
    /// Owning uid / gid, Unix-only. Drive the Owner column.
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

/// Reorderable SFTP file-list columns. `Name` is a first-class member so it
/// can be dragged to any position like the rest; it carries the file icon +
/// filename, is always visible (never toggled off), and keeps a wider size
/// clamp than the data columns (see [`SftpColWidths::set`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SftpColumn {
    Name,
    Modified,
    Size,
    Kind,
    Permissions,
    Owner,
}

impl SftpColumn {
    /// Canonical order of every column (also the default ordering). Name
    /// leads, mirroring the historical fixed-first layout.
    pub const ALL: [SftpColumn; 6] = [
        SftpColumn::Name,
        SftpColumn::Modified,
        SftpColumn::Size,
        SftpColumn::Kind,
        SftpColumn::Permissions,
        SftpColumn::Owner,
    ];

    /// The optional data columns (everything except the always-visible
    /// Name), used to build the column-toggle menu.
    pub const DATA: [SftpColumn; 5] = [
        SftpColumn::Modified,
        SftpColumn::Size,
        SftpColumn::Kind,
        SftpColumn::Permissions,
        SftpColumn::Owner,
    ];

    pub fn key(self) -> &'static str {
        match self {
            SftpColumn::Name => "name",
            SftpColumn::Modified => "modified",
            SftpColumn::Size => "size",
            SftpColumn::Kind => "kind",
            SftpColumn::Permissions => "permissions",
            SftpColumn::Owner => "owner",
        }
    }

    pub fn from_key(s: &str) -> Option<Self> {
        match s.trim() {
            "name" => Some(SftpColumn::Name),
            "modified" => Some(SftpColumn::Modified),
            "size" => Some(SftpColumn::Size),
            "kind" => Some(SftpColumn::Kind),
            "permissions" => Some(SftpColumn::Permissions),
            "owner" => Some(SftpColumn::Owner),
            _ => None,
        }
    }

    pub fn default_width(self) -> f32 {
        match self {
            // The Name cell holds the icon + filename, so it defaults wider.
            SftpColumn::Name => SFTP_NAME_DEFAULT_W,
            SftpColumn::Modified => 140.0,
            SftpColumn::Size => 80.0,
            SftpColumn::Kind => 160.0,
            SftpColumn::Permissions => 116.0,
            SftpColumn::Owner => 120.0,
        }
    }

    /// The sort column this header maps to. Every column sorts (issue
    /// #143 promoted the optional Type / Permissions / Owner trio);
    /// `Option` stays so a future display-only column has somewhere to
    /// say no.
    pub fn sort_column(self) -> Option<SftpSortColumn> {
        match self {
            SftpColumn::Name => Some(SftpSortColumn::Name),
            SftpColumn::Modified => Some(SftpSortColumn::Modified),
            SftpColumn::Size => Some(SftpSortColumn::Size),
            SftpColumn::Kind => Some(SftpSortColumn::Kind),
            SftpColumn::Permissions => Some(SftpSortColumn::Permissions),
            SftpColumn::Owner => Some(SftpSortColumn::Owner),
        }
    }
}

/// Minimum / maximum width a data column can be dragged to.
pub(crate) const SFTP_COL_MIN_W: f32 = 56.0;
pub(crate) const SFTP_COL_MAX_W: f32 = 420.0;

/// Ceiling for a double-click auto-fit (issue #45), wider than the manual
/// drag max so a long value fully fits; the list scrolls horizontally past
/// the pane. Bounded so a pathological filename can't blow the column up.
pub(crate) const SFTP_AUTOFIT_MAX_W: f32 = 1200.0;

/// Which optional columns the SFTP file lists render. Held per pane.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SftpColumns {
    pub size: bool,
    pub modified: bool,
    pub kind: bool,
    pub permissions: bool,
    pub owner: bool,
}

impl Default for SftpColumns {
    fn default() -> Self {
        // Name / Modified / Size on (today's layout); Type / Permissions /
        // Owner off so the default view is unchanged.
        Self {
            size: true,
            modified: true,
            kind: false,
            permissions: false,
            owner: false,
        }
    }
}

impl SftpColumns {
    pub fn is_visible(self, col: SftpColumn) -> bool {
        match col {
            // Name is always shown: a file list with no filename is useless.
            SftpColumn::Name => true,
            SftpColumn::Size => self.size,
            SftpColumn::Modified => self.modified,
            SftpColumn::Kind => self.kind,
            SftpColumn::Permissions => self.permissions,
            SftpColumn::Owner => self.owner,
        }
    }

    pub fn toggle(&mut self, col: SftpColumn) {
        match col {
            // Name can't be hidden, so toggling it is a no-op.
            SftpColumn::Name => {}
            SftpColumn::Size => self.size = !self.size,
            SftpColumn::Modified => self.modified = !self.modified,
            SftpColumn::Kind => self.kind = !self.kind,
            SftpColumn::Permissions => self.permissions = !self.permissions,
            SftpColumn::Owner => self.owner = !self.owner,
        }
    }

    pub fn as_storage_str(self) -> String {
        // Name is implicitly always-on, so its visibility isn't persisted.
        SftpColumn::DATA
            .iter()
            .filter(|c| self.is_visible(**c))
            .map(|c| c.key())
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn from_storage_str(s: &str) -> Self {
        let has = |k: &str| s.split(',').any(|p| p.trim() == k);
        Self {
            size: has("size"),
            modified: has("modified"),
            kind: has("kind"),
            permissions: has("permissions"),
            owner: has("owner"),
        }
    }
}

/// Per-column widths, in pixels. The Name width spans the whole leading cell
/// (file icon + filename), so it keeps a wider clamp than the data columns.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SftpColWidths {
    pub name: f32,
    pub modified: f32,
    pub size: f32,
    pub kind: f32,
    pub permissions: f32,
    pub owner: f32,
}

impl Default for SftpColWidths {
    fn default() -> Self {
        Self {
            name: SftpColumn::Name.default_width(),
            modified: SftpColumn::Modified.default_width(),
            size: SftpColumn::Size.default_width(),
            kind: SftpColumn::Kind.default_width(),
            permissions: SftpColumn::Permissions.default_width(),
            owner: SftpColumn::Owner.default_width(),
        }
    }
}

impl SftpColWidths {
    pub fn get(self, col: SftpColumn) -> f32 {
        match col {
            SftpColumn::Name => self.name,
            SftpColumn::Modified => self.modified,
            SftpColumn::Size => self.size,
            SftpColumn::Kind => self.kind,
            SftpColumn::Permissions => self.permissions,
            SftpColumn::Owner => self.owner,
        }
    }

    pub fn set(&mut self, col: SftpColumn, w: f32) {
        // Name spans the icon + filename and so has its own (wider) clamp.
        let w = if col == SftpColumn::Name {
            w.clamp(SFTP_NAME_MIN_W, SFTP_NAME_MAX_W)
        } else {
            w.clamp(SFTP_COL_MIN_W, SFTP_COL_MAX_W)
        };
        self.put(col, w);
    }

    /// Like [`set`], but for double-click auto-fit (issue #45). Auto-fit is an
    /// explicit "show me the whole value" gesture, so it uses a much wider
    /// ceiling than the manual-drag clamp: a long filename should fully fit
    /// (the file list scrolls horizontally past the pane anyway) instead of
    /// being capped mid-name. Still bounded so a pathological value can't
    /// produce an unusable multi-thousand-pixel column.
    pub fn set_autofit(&mut self, col: SftpColumn, w: f32) {
        let min = if col == SftpColumn::Name { SFTP_NAME_MIN_W } else { SFTP_COL_MIN_W };
        self.put(col, w.clamp(min, SFTP_AUTOFIT_MAX_W));
    }

    fn put(&mut self, col: SftpColumn, w: f32) {
        match col {
            SftpColumn::Name => self.name = w,
            SftpColumn::Modified => self.modified = w,
            SftpColumn::Size => self.size = w,
            SftpColumn::Kind => self.kind = w,
            SftpColumn::Permissions => self.permissions = w,
            SftpColumn::Owner => self.owner = w,
        }
    }
}

/// Full column configuration for one pane: which data columns are visible,
/// their left-to-right order, and their widths. Held per pane (so the Local
/// and remote panes of each SFTP tab are independent) and seeded from the
/// persisted global template on tab creation.
#[derive(Debug, Clone)]
pub(crate) struct SftpColumnState {
    pub visible: SftpColumns,
    pub order: Vec<SftpColumn>,
    pub width: SftpColWidths,
}

/// Default / min / max width for the Name column (the leading icon + filename
/// cell). Wider than a data column because it holds the file icon too.
pub(crate) const SFTP_NAME_DEFAULT_W: f32 = 260.0;
pub(crate) const SFTP_NAME_MIN_W: f32 = 96.0;
pub(crate) const SFTP_NAME_MAX_W: f32 = 600.0;

impl Default for SftpColumnState {
    fn default() -> Self {
        Self {
            visible: SftpColumns::default(),
            order: SftpColumn::ALL.to_vec(),
            width: SftpColWidths::default(),
        }
    }
}

impl SftpColumnState {
    /// The visible data columns in their current order.
    pub fn ordered_visible(&self) -> Vec<SftpColumn> {
        self.order
            .iter()
            .copied()
            .filter(|c| self.visible.is_visible(*c))
            .collect()
    }

    pub fn toggle(&mut self, col: SftpColumn) {
        self.visible.toggle(col);
    }

    /// Move `dragged` to the `target` header's slot (no-op if they're the
    /// same). Direction-aware so it feels natural both ways: dragging a
    /// column rightward drops it *after* the target, leftward drops it
    /// *before*. Operates on the full order vector so hidden columns keep
    /// their relative slots.
    pub fn reorder(&mut self, dragged: SftpColumn, target: SftpColumn) {
        if dragged == target {
            return;
        }
        let Some(from) = self.order.iter().position(|c| *c == dragged) else {
            return;
        };
        let Some(target_idx) = self.order.iter().position(|c| *c == target) else {
            return;
        };
        let moving_right = from < target_idx;
        self.order.remove(from);
        // Recompute the target slot after removal, then offset by one when
        // dropping to the right of the target.
        let mut to = self
            .order
            .iter()
            .position(|c| *c == target)
            .unwrap_or(self.order.len());
        if moving_right {
            to += 1;
        }
        self.order.insert(to, dragged);
    }

    pub fn order_storage(&self) -> String {
        self.order
            .iter()
            .map(|c| c.key())
            .collect::<Vec<_>>()
            .join(",")
    }

    pub fn width_storage(&self) -> String {
        self.order
            .iter()
            .map(|c| format!("{}:{}", c.key(), self.width.get(*c).round() as i32))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Rebuild the order from a stored CSV of keys, appending any column the
    /// stored string omits (forward-compatible if a new column is added).
    pub fn apply_order_storage(&mut self, s: &str) {
        let mut order: Vec<SftpColumn> = Vec::new();
        for part in s.split(',') {
            if let Some(c) = SftpColumn::from_key(part)
                && !order.contains(&c)
            {
                order.push(c);
            }
        }
        // Migration: orders persisted before Name became a reorderable
        // column omit the "name" key. Prepend it so those users keep the
        // historical Name-first layout instead of Name jumping to the end
        // (which the generic append-missing pass below would otherwise do).
        if !order.contains(&SftpColumn::Name) {
            order.insert(0, SftpColumn::Name);
        }
        for c in SftpColumn::ALL {
            if !order.contains(&c) {
                order.push(c);
            }
        }
        self.order = order;
    }

    pub fn apply_width_storage(&mut self, s: &str) {
        for part in s.split(',') {
            let Some((k, v)) = part.split_once(':') else {
                continue;
            };
            let Ok(w) = v.trim().parse::<f32>() else {
                continue;
            };
            // "name" round-trips through `from_key` like every other column
            // now; `set` applies the Name-specific clamp.
            if let Some(c) = SftpColumn::from_key(k) {
                self.width.set(c, w);
            }
        }
    }

    pub fn visibility_storage(&self) -> String {
        self.visible.as_storage_str()
    }

    pub fn apply_visibility_storage(&mut self, s: &str) {
        self.visible = SftpColumns::from_storage_str(s);
    }
}

#[cfg(test)]
mod column_tests {
    use super::*;

    #[test]
    fn legacy_order_without_name_keeps_name_first() {
        // Orders persisted before Name became reorderable omit "name".
        // The migration must prepend it, not let the append-missing pass
        // push Name to the rightmost slot.
        let mut cols = SftpColumnState::default();
        cols.apply_order_storage("modified,size,kind");
        assert_eq!(cols.order.first(), Some(&SftpColumn::Name));
        // Every column is still present exactly once.
        for c in SftpColumn::ALL {
            assert_eq!(cols.order.iter().filter(|x| **x == c).count(), 1);
        }
    }

    #[test]
    fn explicit_name_position_is_preserved() {
        // A stored order that already places Name (e.g. user dragged it to
        // the middle) round-trips without being moved back to the front.
        let mut cols = SftpColumnState::default();
        cols.apply_order_storage("modified,name,size");
        assert_eq!(
            cols.order,
            vec![
                SftpColumn::Modified,
                SftpColumn::Name,
                SftpColumn::Size,
                SftpColumn::Kind,
                SftpColumn::Permissions,
                SftpColumn::Owner,
            ]
        );
    }

    #[test]
    fn name_width_uses_its_own_clamp() {
        // The Name clamp is wider than the data clamp, so a 520px Name
        // survives where a data column would be capped at SFTP_COL_MAX_W.
        let mut w = SftpColWidths::default();
        w.set(SftpColumn::Name, 520.0);
        assert_eq!(w.get(SftpColumn::Name), 520.0);
        w.set(SftpColumn::Size, 520.0);
        assert_eq!(w.get(SftpColumn::Size), SFTP_COL_MAX_W);
    }

    #[test]
    fn name_is_always_visible_and_never_toggles() {
        let mut vis = SftpColumns::default();
        assert!(vis.is_visible(SftpColumn::Name));
        vis.toggle(SftpColumn::Name);
        assert!(vis.is_visible(SftpColumn::Name));
        // Name visibility is not written to the persisted string.
        assert!(!vis.as_storage_str().split(',').any(|p| p == "name"));
    }
}

#[cfg(test)]
mod archive_op_tests {
    use super::*;

    fn remote_pane() -> PaneState {
        PaneState {
            is_remote: true,
            ..Default::default()
        }
    }

    #[test]
    fn token_survives_unrelated_pane_mount() {
        // A mount on one pane must not invalidate (or unlock the busy
        // flag of) an op running on the OTHER pane: the busy clear and
        // staleness checks are strictly per-pane.
        let mut left = remote_pane();
        let mut right = remote_pane();
        left.note_mounted();
        right.note_mounted();
        let right_token = right.archive_op_token();
        // The left pane remounts while the right pane's op is in
        // flight; the right pane's completion must still apply.
        left.note_mounted();
        assert!(right.archive_op_current(right_token));
        assert!(right.archive_op_owns_busy(right_token));
    }

    #[test]
    fn remount_invalidates_token_and_busy_ownership() {
        let mut pane = remote_pane();
        pane.note_mounted();
        let token = pane.archive_op_token();
        assert!(pane.archive_op_current(token));
        // Remounted to another host mid-op: the result is stale and,
        // because the mount reset swept `archive_busy` (a newer op may
        // own it now), the completion must not clear the flag either.
        pane.note_mounted();
        assert!(!pane.archive_op_current(token));
        assert!(!pane.archive_op_owns_busy(token));
    }

    #[test]
    fn local_switch_invalidates_result_but_keeps_busy_ownership() {
        // Switching the pane back to Local doesn't remount (no
        // `note_mounted`), so the generation is unchanged: the result
        // is stale (wrong pane nature) but the busy flag is still this
        // op's to clear, nothing else could have set it.
        let mut pane = remote_pane();
        pane.note_mounted();
        let token = pane.archive_op_token();
        pane.is_remote = false;
        assert!(!pane.archive_op_current(token));
        assert!(pane.archive_op_owns_busy(token));
    }

    #[test]
    fn mount_generations_are_globally_unique() {
        // Two panes never share a generation, so a token captured on
        // one surface's pane can't match another's after a hybrid
        // park / hoist swap.
        let mut a = remote_pane();
        let mut b = remote_pane();
        a.note_mounted();
        b.note_mounted();
        assert_ne!(a.mount_seq, b.mount_seq);
        assert!(!b.archive_op_current(a.archive_op_token()));
    }
}

/// Severity of a [`SftpLogEntry`], drives its colour in the log panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SftpLogLevel {
    Info,
    Ok,
    Warn,
    Error,
}

/// One line in the FileZilla-style SFTP message log.
#[derive(Debug, Clone)]
pub(crate) struct SftpLogEntry {
    /// Wall-clock time the entry was recorded, formatted "HH:MM:SS" at
    /// push time so the view does not re-derive it every redraw.
    pub time: String,
    pub level: SftpLogLevel,
    pub text: String,
}
