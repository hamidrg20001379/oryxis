//! Keyboard router for the modal / overlay-menu layer.
//!
//! Runs from the `|v| Message::Terminal(TerminalMessage::KeyboardEvent(v))` arm in `dispatch_terminal.rs`
//! BEFORE the vault keynav router: while a navigable modal, dropdown
//! menu or the burger menu is open, this layer owns the movement and
//! activation keys. It never consumes Esc (that stays with
//! `close_topmost_modal` in `shortcuts.rs`) and never consumes
//! printable characters (picker search fields keep receiving them
//! through iced's real focus).
//!
//! Selection is INDEX-based over `RowAction`s recorded during view()
//! (see `keynav/slots.rs`), tagged with a `ModalSurface` so a stale
//! selection from a previous surface is inert. Three families:
//!
//! - Confirm dialogs: open with the ring on their default (action)
//!   button, Enter/Space activate, Tab and arrows cycle the buttons.
//! - Search pickers (new-tab, tab-jump, group pickers): typing stays
//!   in the input, Up/Down move the row selection, Enter activates
//!   the selection or the top match.
//! - Row menus (kebabs, sort, overflow, burger): open with the first
//!   row ringed, Up/Down move, Enter/Space fire the row's click
//!   message (mouse parity, including how the menu closes).

use iced::keyboard;
use iced::Task;

use crate::app::{Message, Oryxis, TabsMessage};
use crate::keynav::movement::index_move;
use crate::keynav::{ModalSurface, RowAction};
use crate::state::Modal;

/// How a surface treats keys beyond the shared movement set.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SurfaceFamily {
    /// Buttons only (or buttons + inputs, see `has_input`).
    Confirm,
    /// Filtered list under a live search input.
    Picker,
    /// Plain row menu.
    Menu,
}

impl Oryxis {
    /// The topmost keyboard-navigable modal surface, resolved in the
    /// same priority order as `close_topmost_modal` (overlay first,
    /// then `ESC_ORDER`, then the burger menu). Returns `None` for
    /// surfaces that own their own keys (KBI prompt, SFTP dialogs,
    /// rename inputs, theme/icon editors, hover popovers) so those
    /// keep today's behavior untouched, and while a hotkey capture
    /// is pending (the capture must see raw keys).
    pub(crate) fn modal_nav_surface(&self) -> Option<(ModalSurface, SurfaceFamily)> {
        use crate::state::OverlayContent as OC;
        if self.editing_hotkey.is_some() {
            return None;
        }
        // No vault could be opened: the error screen is the whole window,
        // and nothing else can be on it.
        if self.vault.is_none() && self.vault_ui.state == crate::state::VaultState::Loading {
            return Some((ModalSurface::VaultOpenFailed, SurfaceFamily::Confirm));
        }
        if let Some(ov) = &self.overlay {
            let family = match &ov.content {
                // Hover popover and the floating search field have no
                // row rows to navigate. The password-suggest popup
                // (#117) has rows but is deliberately NOT a modal
                // surface: it sits over a live PTY and owns keys only
                // once the user engages it, which
                // `handle_password_suggest_key` decides earlier in the
                // router. Letting the generic layer claim it would eat
                // Up/Down/Enter from the shell underneath.
                OC::SplitMenu | OC::ToolbarSearch | OC::PasswordSuggest { .. } => return None,
                OC::GroupPicker(_) | OC::CloudDiscoverGroupPicker => SurfaceFamily::Picker,
                _ => SurfaceFamily::Menu,
            };
            return Some((
                ModalSurface::Overlay(std::mem::discriminant(&ov.content)),
                family,
            ));
        }
        // The modals are drawn in one window (`floats_here`): a window
        // that does not show the open modal has no rows for it, and
        // claiming the surface there would swallow Tab and the arrows in
        // that window's terminal for as long as the modal stays up.
        for &m in Modal::ESC_ORDER {
            if self.floats_here() && self.is_modal_open(m) {
                let family = match m {
                    // Command palette input carries no on_submit (unlike
                    // NewTabPicker), so the router's Enter path activates
                    // the selection-or-top-match directly.
                    Modal::NewTabPicker | Modal::TabJump | Modal::CommandPalette => {
                        SurfaceFamily::Picker
                    }
                    // Chain editor: the add-a-hop sub-view is a search
                    // picker; the chain list navigates as a row menu.
                    Modal::ChainEditor => {
                        if self.chain_editor_adding {
                            SurfaceFamily::Picker
                        } else {
                            SurfaceFamily::Menu
                        }
                    }
                    Modal::FolderDelete
                    | Modal::CarefulPaste
                    | Modal::SnippetVars
                    | Modal::ErrorDialog
                    | Modal::ClearHistoryConfirm
                    | Modal::SshImport
                    | Modal::ShareDialog
                    | Modal::CloudImportConfirm
                    // Dial security prompts: the REFUSING button is the
                    // default-ringed action on both, so a stray Enter can
                    // never trust a host key or spawn a command proxy
                    // (Esc refuses either way, via ESC_ORDER). Their rows
                    // are recorded by the shared button builders
                    // (`host_key_buttons` / `proxy_command_buttons`), so
                    // the inline connect-progress prompts navigate
                    // identically to the standalone cards.
                    | Modal::HostKey
                    | Modal::ProxyCommand
                    // Security prompt: Deny is the default-ringed action.
                    | Modal::AgentConfirm
                    // Save-confirmation for an edit watch: Yes is the
                    // default-ringed action, Esc skips the save.
                    | Modal::SftpEditPrompt
                    // Reopen-or-redownload: "Reopen the local copy" is the
                    // default-ringed action (it never loses work).
                    | Modal::SftpEditReopen
                    // Read-only cert viewer: Close is the default action;
                    // Remove (when present) is the other recorded row.
                    | Modal::CertificateViewer
                    // Kill-the-process confirm (issue #96): Cancel is
                    // the default-ringed action, so a stray Enter can
                    // never take a remote service down.
                    | Modal::MonitorKill
                    // Manual-lock confirm: Cancel is the default-ringed
                    // action too, so a stray Enter can never sever every
                    // live connection.
                    | Modal::LockVaultConfirm
                    // "Let this rule run a snippet" (C6): Don't send is
                    // the default-ringed action, so a stray Enter can
                    // never hand the session to remote output.
                    | Modal::TriggerConfirm
                    // "Open this link?": Cancel is the default-ringed
                    // action, so a stray Enter never hands a remote
                    // host's URL to the browser.
                    | Modal::TerminalLinkConfirm
                    // "Save Output As...": Clipboard is the default-ringed
                    // row. The user asked for it, nothing in it destroys
                    // anything, and it is the one answer that finishes
                    // without a second dialog.
                    | Modal::TerminalSaveOutput
                    // The update offer: Later is the default-ringed row
                    // in every state, so a stray Enter on a dialog that
                    // opened itself neither downloads nor restarts.
                    | Modal::UpdateOffer
                    // "Run on <host>?": Cancel is the default-ringed
                    // row, so a stray Enter never runs a script as root
                    // on a remote machine. Same class as ProxyCommand.
                    | Modal::RelayDeployConfirm
                    // The Sync host picker: its rows are recorded by the
                    // shared builder, Enter picks the ringed host, and
                    // the search field keeps the caret (`has_input`).
                    | Modal::SyncHostPicker
                    // The host editor's terminal theme picker: its filter,
                    // tone chips, cards and Close are recorded in display
                    // order, Enter picks the ringed card, and the filter
                    // keeps the caret (`has_input`). No default row: a
                    // stray Enter must not change the host's theme.
                    | Modal::ThemePicker
                    // The highlight-rule editor is a form, but it walks
                    // like a confirm: Tab / arrows step its rows, Enter
                    // fires the default (Save). Its text fields keep the
                    // caret because it declares `has_input`.
                    | Modal::HighlightRuleEditor => SurfaceFamily::Confirm,
                    // Rename inputs (on_submit), editors, pickers with
                    // their own model: out of this layer. That includes
                    // the theme IMPORT modals: their multiline paste
                    // text_editor makes an Enter-confirms default row
                    // actively harmful (Enter must insert a newline),
                    // and iced can't report editor focus to gate on.
                    // Esc still closes them via ESC_ORDER.
                    _ => return None,
                };
                return Some((ModalSurface::Modal(m), family));
            }
        }
        if self.panels.burger_menu {
            return Some((ModalSurface::Burger, SurfaceFamily::Menu));
        }
        // The SFTP right-click menu is a plain context menu: arrows move,
        // Enter activates, Esc closes (via close_topmost_modal). Lowest
        // precedence, but it never coexists with the surfaces above.
        if self.cur_sftp().row_menu.is_some() {
            return Some((ModalSurface::SftpRowMenu, SurfaceFamily::Menu));
        }
        // The connect-progress card, below everything above: any dialog,
        // menu or prompt drawn over it (or inside it) owns the keys first.
        // The host-key and command-proxy prompts are `Modal`s and were
        // answered by the loop above; the keyboard-interactive form is a
        // `Modal` too but sits outside `ESC_ORDER`, so the blocking check
        // is what yields to it (its inputs keep Enter for their submit).
        if self.connect_progress_on_screen() && !self.any_modal_blocks_input() {
            return Some((ModalSurface::ConnectProgress, SurfaceFamily::Confirm));
        }
        None
    }

    /// Whether the surface carries a text input, in which case the
    /// caret keeps Left/Right and Space (only Tab/Up/Down cycle).
    fn modal_surface_has_input(surface: ModalSurface) -> bool {
        matches!(
            surface,
            ModalSurface::Modal(
                Modal::ShareDialog
                    | Modal::SshImport
                    | Modal::CloudImportConfirm
                    // The snippet-variables prompt is a column of value
                    // text_inputs; the caret must keep Space (and Left/Right)
                    // so typing a value never fires the default Confirm and
                    // submits the snippet with partial values.
                    | Modal::SnippetVars
                    // Name / pattern / hex fields: the caret keeps Space
                    // (a rule named "Disk full" needs one) and Left /
                    // Right, except on a picker row the user stepped onto.
                    | Modal::HighlightRuleEditor
                    // The host picker's search field.
                    | Modal::SyncHostPicker
                    // The theme picker's filter field.
                    | Modal::ThemePicker
            )
        )
    }

    /// Entry point, called before `handle_keynav_key`. Returns
    /// `Some(task)` when the key was consumed by the modal layer.
    pub(crate) fn handle_modal_nav_key(
        &mut self,
        event: &keyboard::Event,
    ) -> Option<Task<Message>> {
        let (surface, family) = self.modal_nav_surface()?;
        // A selection left over from a different surface is dead
        // weight; drop it so this surface starts at its default.
        if let Some((tag, _)) = self.keynav.modal.selected
            && tag != surface
        {
            self.keynav.modal.selected = None;
        }
        let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
            return None;
        };
        if modifiers.control() || modifiers.alt() || modifiers.logo() {
            return None;
        }
        // The connect-progress card appears by itself (a dial started
        // from anywhere, a failure landing), so a key aimed elsewhere
        // must never act on it: Enter and Space only activate a row the
        // KEYBOARD ringed, never the row a resting cursor happens to be
        // over (a hover selection is invisible until a key shows it),
        // and there is no default row to fall back on.
        let ring_shown = self.keynav.modal.kbd.get()
            && matches!(self.keynav.modal.selected, Some((tag, _)) if tag == surface);
        // Any key reaching the modal router flips the modality gate:
        // from here on the (possibly hover-made) selection shows its
        // ring, until the next hover hides it again (focus-visible).
        self.keynav.modal.kbd.set(true);
        if surface == ModalSurface::ConnectProgress
            && let Some(task) = Self::connect_progress_key(key, ring_shown)
        {
            return task.map(|()| {
                if matches!(key, keyboard::Key::Named(keyboard::key::Named::Escape)) {
                    self.keynav.modal.selected = None;
                    self.keynav.modal.kbd.set(false);
                    Task::none()
                } else {
                    self.modal_nav_activate(surface).unwrap_or_else(Task::none)
                }
            });
        }
        let has_input = Self::modal_surface_has_input(surface);
        let len = self.keynav.modal.items.borrow().len();

        // Space activates like Enter on input-less surfaces; with an
        // input present it must keep typing spaces.
        let is_space = matches!(key, keyboard::Key::Named(keyboard::key::Named::Space))
            || matches!(key, keyboard::Key::Character(c) if c.as_str() == " ");
        if is_space {
            if family == SurfaceFamily::Picker || has_input {
                return None;
            }
            return self.modal_nav_activate(surface);
        }
        let keyboard::Key::Named(named) = key else {
            // Printable characters always reach the real input.
            return None;
        };
        use keyboard::key::Named;
        match named {
            Named::Escape => None,
            Named::Enter => {
                // The new-tab picker's Enter is OWNED here, not by its
                // search input: the fork's `on_submit` fires on any
                // Enter, modifiers included, and would connect the row
                // in a tab on the very Ctrl+Enter the binding table
                // answers with a new window. This router already
                // declines a modified Enter above, so the plain one
                // submits (selection-aware, see `NewTabPickerSubmit`)
                // and a chord on Enter falls through to the table.
                if surface == ModalSurface::Modal(Modal::NewTabPicker) {
                    return Some(Task::done(Message::Tabs(TabsMessage::NewTabPickerSubmit)));
                }
                self.modal_nav_activate(surface)
            }
            Named::Tab => {
                // Cycle rows in confirms and pickers (Tab walks out of the
                // search field into the list, Shift+Tab walks back);
                // consumed no-op in plain menus so a literal \t never
                // lands anywhere and arrows stay the movement keys there.
                if matches!(family, SurfaceFamily::Confirm | SurfaceFamily::Picker)
                    && len > 0
                {
                    self.modal_nav_step(surface, !modifiers.shift());
                }
                Some(Task::none())
            }
            Named::ArrowUp | Named::ArrowDown => {
                if len == 0 {
                    return Some(Task::none());
                }
                self.modal_nav_step(surface, matches!(named, Named::ArrowDown));
                Some(Task::none())
            }
            Named::ArrowLeft | Named::ArrowRight => {
                let rtl = crate::i18n::is_rtl_layout();
                let forward = matches!(named, Named::ArrowRight) != rtl;
                // A picker row the user EXPLICITLY stepped onto cycles
                // its options even on a surface that also carries text
                // fields: the ring says which control the arrows act on,
                // and a picker is never a caret. Explicit only, so a
                // surface whose default row happens to be a picker keeps
                // handing Left/Right to whatever field has focus.
                if matches!(self.keynav.modal.selected, Some((tag, _)) if tag == surface)
                    && let Some(task) = self.modal_nav_cycle_option(surface, forward)
                {
                    return Some(task);
                }
                if has_input || family == SurfaceFamily::Picker {
                    // Caret movement in the surface's input.
                    return None;
                }
                // Picker rows cycle their options; plain rows treat
                // Left/Right as movement (confirm buttons sit side by
                // side).
                if let Some(task) = self.modal_nav_cycle_option(surface, forward) {
                    return Some(task);
                }
                if len > 0 {
                    self.modal_nav_step(surface, forward);
                }
                Some(Task::none())
            }
            Named::Home | Named::End => {
                if len == 0 {
                    return Some(Task::none());
                }
                let idx = if matches!(named, Named::Home) { 0 } else { len - 1 };
                self.keynav.modal.selected = Some((surface, idx));
                Some(Task::none())
            }
            _ => None,
        }
    }

    /// The connect-progress card's own answer to Enter, Space and Esc,
    /// before the shared confirm handling (which would activate row 0
    /// when nothing is ringed). `None` = not one of those keys, let the
    /// shared router handle it (Tab and arrows move the ring as on any
    /// confirm). `Some(None)` = decline, the key goes on as it would
    /// without this surface. `Some(Some(()))` = act: Enter / Space
    /// activate the ringed row, Esc drops the ring.
    ///
    /// Esc never closes the card. Its Close removes the tab, and
    /// nothing else in the app closes a tab on a bare Esc (Ctrl+W is
    /// that key); while the dial is live, closing would also abort it.
    /// So Esc only returns the card to idle, the vault area's meaning,
    /// and declines when there is no ring to drop.
    fn connect_progress_key(key: &keyboard::Key, ring_shown: bool) -> Option<Option<()>> {
        use keyboard::key::Named;
        let activate = matches!(key, keyboard::Key::Named(Named::Enter | Named::Space))
            || matches!(key, keyboard::Key::Character(c) if c.as_str() == " ");
        if activate || matches!(key, keyboard::Key::Named(Named::Escape)) {
            return Some(ring_shown.then_some(()));
        }
        None
    }

    /// Move the selection one step (wrapping), starting from the
    /// effective position (explicit selection or surface default).
    fn modal_nav_step(&mut self, surface: ModalSurface, forward: bool) {
        let len = self.keynav.modal.items.borrow().len();
        let cur = self.modal_nav_effective(surface);
        if let Some(next) = index_move(len, cur, forward) {
            self.keynav.modal.selected = Some((surface, next));
        }
    }

    /// Enter/Space on the effective row: dispatch its activate
    /// message, focus its input, or (picker family with no explicit
    /// selection) activate the top match.
    fn modal_nav_activate(&mut self, surface: ModalSurface) -> Option<Task<Message>> {
        let idx = self.modal_nav_effective(surface).or({
            // Picker Enter with no selection: the first (top) row.
            if self.keynav.modal.items.borrow().is_empty() {
                None
            } else {
                Some(0)
            }
        })?;
        let action: RowAction = self.keynav.modal.items.borrow().get(idx)?.clone();
        if let Some(id) = action.focus {
            self.keynav.modal.selected = None;
            return Some(crate::widgets::focus_input(id));
        }
        let msg = action.activate?;
        Some(self.update(msg))
    }

    /// Left/Right on a picker row: fire the prepared neighbor
    /// on_select message. `None` when the effective row is not a
    /// picker (caller falls back to movement).
    fn modal_nav_cycle_option(
        &mut self,
        surface: ModalSurface,
        forward: bool,
    ) -> Option<Task<Message>> {
        let idx = self.modal_nav_effective(surface)?;
        let action: RowAction = self.keynav.modal.items.borrow().get(idx)?.clone();
        let msg = if forward { action.next } else { action.prev }?;
        // Keep the selection on the row so repeated arrows keep
        // cycling.
        self.keynav.modal.selected = Some((surface, idx));
        Some(self.update(msg))
    }
}

#[cfg(test)]
mod tests {
    use crate::app::Oryxis;
    use iced::keyboard::{key::Named, Key};

    /// The connect-progress card appears by itself, so a bare Enter /
    /// Space / Esc acts on it only once the keyboard has ringed a row;
    /// every other key is left to the shared router.
    #[test]
    fn the_progress_card_acts_only_on_a_ringed_row() {
        let enter = Key::Named(Named::Enter);
        let space = Key::Named(Named::Space);
        let space_char = Key::Character(" ".into());
        let esc = Key::Named(Named::Escape);
        for key in [&enter, &space, &space_char, &esc] {
            assert_eq!(Oryxis::connect_progress_key(key, true), Some(Some(())));
            assert_eq!(Oryxis::connect_progress_key(key, false), Some(None));
        }
        for key in [Key::Named(Named::Tab), Key::Named(Named::ArrowDown), Key::Character("a".into())] {
            assert_eq!(Oryxis::connect_progress_key(&key, true), None);
            assert_eq!(Oryxis::connect_progress_key(&key, false), None);
        }
    }
}
