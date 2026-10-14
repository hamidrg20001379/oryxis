//! The mouse router: bindable buttons, and who owns each one.
//!
//! Mouse buttons are bindings, not settings (CLAUDE.md). Back / Forward
//! yield to a visible file surface before anything else, the wheel click
//! stays terminal-only because the canvas spends it, and
//! `mouse_binding_owner` is the single authority both layers consult so
//! they cannot drift.

use iced::Task;

use crate::app::{SftpMessage, Message, Oryxis};
use crate::hotkeys::{FamilyMatch, HotkeyAction};
use crate::state::View;

impl Oryxis {
    /// Whether a bare middle click pastes the selection.
    ///
    /// DERIVED from the binding table rather than kept as a setting of
    /// its own: the gesture is an ordinary chord on
    /// `TerminalPasteSelection`, so two sources of truth would let the
    /// Shortcuts editor and the Terminal toggle disagree. Settings >
    /// Terminal's toggle is a shortcut for adding / removing this one
    /// chord, which is why binding middle-click to something else reads
    /// as the toggle going off: it did.
    pub(crate) fn middle_click_pastes(&self) -> bool {
        self.hotkey_bindings
            .get(&HotkeyAction::TerminalPasteSelection)
            .is_some_and(|b| b.contains(&crate::hotkeys::middle_click_chord()))
    }

    /// Add / remove the bare middle-click chord on
    /// `TerminalPasteSelection`. Adding goes through the same
    /// conflict resolution as a recorded binding, so turning the toggle
    /// on while another action holds middle-click takes it from that
    /// action and says so, instead of minting a duplicate that would
    /// make the first match win silently.
    pub(crate) fn set_middle_click_paste(&mut self, on: bool) -> Task<Message> {
        let chord = crate::hotkeys::middle_click_chord();
        let action = HotkeyAction::TerminalPasteSelection;
        if on {
            return self.commit_captured_binding(action, crate::hotkeys::HotkeySlot::Add, chord);
        }
        let mut binds = self.hotkey_bindings.get(&action).cloned().unwrap_or_default();
        if !binds.remove(&chord) {
            return Task::none();
        }
        self.persist_setting(&format!("hotkey_{}", action.id()), &binds.serialize());
        // Same invariant the capture's Delete branch and the boot
        // migration hold: an emptied list drops out of the map rather
        // than sitting there empty.
        if binds.is_empty() {
            self.hotkey_bindings.remove(&action);
        } else {
            self.hotkey_bindings.insert(action, binds);
        }
        Task::none()
    }

    /// Whether Ctrl+wheel zooms the font.
    ///
    /// DERIVED from the binding table the way `middle_click_pastes` is:
    /// the gesture is the pair of Ctrl+wheel chords on `FontZoomIn` /
    /// `FontZoomOut`, so Settings > Terminal's toggle and the Shortcuts
    /// editor read one state. On means BOTH halves are there; a pair
    /// the user edited down to one direction reads as off, and turning
    /// the toggle on puts the missing half back.
    pub(crate) fn wheel_zoom_bound(&self) -> bool {
        Self::WHEEL_ZOOM_PAIR.iter().all(|(action, direction)| {
            self.hotkey_bindings
                .get(action)
                .is_some_and(|b| b.contains(&crate::hotkeys::wheel_zoom_chord(*direction)))
        })
    }

    /// The two chords the wheel-zoom toggle owns.
    const WHEEL_ZOOM_PAIR: [(HotkeyAction, crate::hotkeys::WheelDirection); 2] = [
        (HotkeyAction::FontZoomIn, crate::hotkeys::WheelDirection::Up),
        (HotkeyAction::FontZoomOut, crate::hotkeys::WheelDirection::Down),
    ];

    /// Add / remove the Ctrl+wheel chords on the font zoom actions.
    /// Adding goes through the same conflict resolution as a recorded
    /// binding, so turning the toggle on while another action holds one
    /// of the chords takes it from that action and says so.
    pub(crate) fn set_wheel_zoom(&mut self, on: bool) -> Task<Message> {
        let mut tasks = Vec::new();
        for (action, direction) in Self::WHEEL_ZOOM_PAIR {
            let chord = crate::hotkeys::wheel_zoom_chord(direction);
            if on {
                if self.hotkey_bindings.get(&action).is_some_and(|b| b.contains(&chord)) {
                    continue;
                }
                tasks.push(self.commit_captured_binding(
                    action,
                    crate::hotkeys::HotkeySlot::Add,
                    chord,
                ));
                continue;
            }
            let mut binds = self.hotkey_bindings.get(&action).cloned().unwrap_or_default();
            if !binds.remove(&chord) {
                continue;
            }
            self.persist_setting(&format!("hotkey_{}", action.id()), &binds.serialize());
            // Same invariant as `set_middle_click_paste`: an emptied
            // list drops out of the map rather than sitting there empty.
            if binds.is_empty() {
                self.hotkey_bindings.remove(&action);
            } else {
                self.hotkey_bindings.insert(action, binds);
            }
        }
        Task::batch(tasks)
    }

    /// Wheel branch of the Shortcuts capture, reached only while one is
    /// armed (the subscription forwards the wheel then and only then).
    ///
    /// A notch with no chord-making modifier is left alone WITHOUT a
    /// toast: the capture sits on a scrollable page, and a bare wheel
    /// is the user scrolling it, not a rejected binding. Only a notch
    /// that could be a chord is judged.
    pub(crate) fn handle_hotkey_wheel_capture(
        &mut self,
        direction: crate::hotkeys::WheelDirection,
    ) -> Task<Message> {
        let Some((action, slot)) = self.editing_hotkey else {
            return Task::none();
        };
        // Same belt-and-suspenders gate as the button path: a capture
        // left armed on another screen must not silently rebind.
        if self.cur_view() != View::Settings
            || self.settings_section != crate::state::SettingsSection::Shortcuts
        {
            self.editing_hotkey = None;
            return Task::none();
        }
        let Some(binding) = crate::hotkeys::binding_from_wheel(direction, &self.modifiers) else {
            return Task::none();
        };
        if !action.accepts_wheel() {
            self.set_toast(crate::i18n::t("hotkey_wheel_terminal_only").to_string());
            return super::toast_clear_after_secs(3);
        }
        self.commit_captured_binding(action, slot, binding)
    }

    /// A bindable mouse button was pressed anywhere in the window.
    /// Either records it (a Shortcuts capture is armed) or fires
    /// whatever it is bound to.
    ///
    /// Left / Right never get here (the subscription filters them out
    /// with `MouseButton::from_iced`); they belong to the canvas.
    pub(crate) fn handle_mouse_button_press(
        &mut self,
        button: iced::mouse::Button,
    ) -> Task<Message> {
        if self.editing_hotkey.is_some() {
            return self.handle_hotkey_mouse_capture(button);
        }
        // Directory back / forward first, when a file surface is what the
        // buttons are over. These are what the OS itself calls them
        // (Windows sends APPCOMMAND_BROWSER_BACKWARD / FORWARD for
        // XBUTTON1 / XBUTTON2), so every browser and file manager answers
        // them this way and a user's hand already expects it.
        //
        // It runs BEFORE the bindable actions, and that order is the
        // whole policy: a visible file surface wins the two buttons, a
        // user binding gets them everywhere else. Back / Forward are
        // genuinely contested (they are the thumb pair on an ordinary
        // five-button mouse, not exotic extras), so the choice is which
        // context yields, not whether they may be bound at all. Same
        // shape as the keyboard side, where a bare Ctrl+letter binding
        // yields to the PTY inside a terminal and fires elsewhere.
        if let Some(task) = self.file_surface_nav(button) {
            return task;
        }
        self.dispatch_mouse_binding(button)
    }

    /// Fire the action bound to a SIDE button, from anywhere in the
    /// window.
    ///
    /// Which pairs belong here is `HotkeyAction::mouse_binding_owner`,
    /// shared with `views::terminal::terminal_mouse_resolver`. In
    /// practice: side buttons, minus the five gestures that need canvas
    /// state. The wheel click never reaches this path, so a middle
    /// click over a list can't fire an action the way it would over the
    /// canvas.
    ///
    /// The view gates mirror the keyboard router's exactly, for the same
    /// reasons: a terminal action outside a terminal tab (and a vault
    /// one outside the vault) is skipped rather than dispatched into a
    /// no-op.
    /// Walk the visited directories of whichever file surface is on
    /// screen. `None` when neither is, so the press falls through to the
    /// bindable actions.
    ///
    /// A visible file surface CONSUMES these buttons even with nowhere
    /// to go (`Some(Task::none())`, not `None`). The alternative reads as
    /// a broken binding: "back closes the tab, except in Files, except at
    /// the start of the history where it closes the tab again" is a rule
    /// no user can hold. Which surface is up is a fact the user can see;
    /// how deep its history happens to be is not.
    ///
    /// "Up one level" deliberately does NOT live on these buttons, even
    /// though some clients put it there: after a jump through the path bar
    /// or the recents dropdown, back and up point at different places, and
    /// a button labelled back must go back. Up stays on the `..` row.
    fn file_surface_nav(&mut self, button: iced::mouse::Button) -> Option<Task<Message>> {
        let back = match button {
            iced::mouse::Button::Back => true,
            iced::mouse::Button::Forward => false,
            _ => return None,
        };
        // The SFTP surface (standalone tab or a hybrid tab in Files mode)
        // owns the buttons whenever it is up; otherwise the terminal
        // sidebar's Files tab does, and only while it is the visible tab.
        if self.sftp_surface_visible() {
            let side = self.cur_sftp().focused_side;
            let pane = self.cur_sftp().pane(side);
            let current = if pane.is_remote {
                pane.remote_path.clone()
            } else {
                pane.local_path.display().to_string()
            };
            let pane = self.sftp.pane_mut(side);
            let target = if back {
                pane.nav_go_back(current)
            } else {
                pane.nav_go_forward(current)
            };
            let Some(target) = target else {
                return Some(Task::none());
            };
            let is_remote = self.cur_sftp().pane(side).is_remote;
            return Some(Task::done(if is_remote {
                Message::Sftp(SftpMessage::SftpNavigateRemote(side, target))
            } else {
                Message::Sftp(SftpMessage::SftpNavigateLocal(
                    side,
                    std::path::PathBuf::from(target),
                ))
            }));
        }
        if !self.sidebar_tab_shown(crate::state::TerminalSidebarTab::Files) {
            return None;
        }
        // No active pane means no Files tab is really mounted, so this
        // is not a file surface after all: fall through rather than
        // swallow the press.
        let pane = self.active_pane_mut()?;
        let current = pane.files.path.clone();
        let target = if back {
            pane.files.nav_go_back(current)
        } else {
            pane.files.nav_go_forward(current)
        };
        let Some(target) = target else {
            return Some(Task::none());
        };
        Some(Task::done(Message::SidebarFiles(
            crate::app::SidebarFilesMessage::SidebarFilesNavigate(target),
        )))
    }

    fn dispatch_mouse_binding(&mut self, button: iced::mouse::Button) -> Task<Message> {
        let Some(button) = crate::hotkeys::MouseButton::from_iced(button) else {
            return Task::none();
        };
        if !button.is_side_button() {
            return Task::none();
        }
        // A blocking modal owns input, same as for chords.
        if self.any_modal_blocks_input() {
            return Task::none();
        }
        // "In a terminal" is a FOCUSED TERMINAL TAB, not
        // `active_view == Terminal`: workspace-mode tabs run under the
        // Dashboard view (see the keyboard router's note).
        let in_terminal = self.cur_view() == View::Terminal || self.cur_active_tab().is_some();
        let mods = self.modifiers;
        let mut hit: Option<HotkeyAction> = None;
        for &action in HotkeyAction::all() {
            if action.mouse_binding_owner(button) != crate::hotkeys::MouseBindingOwner::App {
                continue;
            }
            if action.terminal_only() && !in_terminal {
                continue;
            }
            if action.vault_only() && !self.in_vault_area() {
                continue;
            }
            if self
                .hotkey_bindings
                .get(&action)
                .is_some_and(|b| b.match_mouse(button, &mods))
            {
                hit = Some(action);
                break;
            }
        }
        let Some(action) = hit else {
            return Task::none();
        };
        tracing::debug!(action = action.id(), "mouse binding matched");
        self.dispatch_hotkey_action(action, FamilyMatch::Plain)
    }

    /// Mouse branch of the Shortcuts capture. Reached only with a
    /// capture armed, so it just has to prove the editor is still the
    /// visible surface before it writes.
    fn handle_hotkey_mouse_capture(
        &mut self,
        button: iced::mouse::Button,
    ) -> Task<Message> {
        let Some((action, slot)) = self.editing_hotkey else {
            return Task::none();
        };
        // Same belt-and-suspenders gate as the keyboard path: a capture
        // left armed on another screen must not silently rebind.
        if self.cur_view() != View::Settings
            || self.settings_section != crate::state::SettingsSection::Shortcuts
        {
            self.editing_hotkey = None;
            return Task::none();
        }
        let Some(binding) = crate::hotkeys::binding_from_mouse(button, &self.modifiers) else {
            return Task::none();
        };
        // The wheel click is terminal-only (it is the one bindable
        // button the app doesn't own window-wide). Say so rather than
        // swallowing the press, which would read as a dead button.
        let crate::hotkeys::PrimaryKey::Mouse(pressed) = binding.primary else {
            return Task::none();
        };
        if !action.accepts_mouse_button(pressed) {
            self.set_toast(crate::i18n::t("hotkey_mouse_terminal_only").to_string());
            return super::toast_clear_after_secs(3);
        }
        self.commit_captured_binding(action, slot, binding)
    }

    /// Write a captured binding into `slot`, resolving conflicts and
    /// persisting. Shared by the keyboard and mouse capture paths so a
    /// mouse binding is subject to exactly the same conflict rules as a
    /// chord.
    pub(crate) fn commit_captured_binding(
        &mut self,
        action: HotkeyAction,
        slot: crate::hotkeys::HotkeySlot,
        new_binding: crate::hotkeys::HotkeyBinding,
    ) -> Task<Message> {
        // A snippet's own chord is not the editor's to take: the binding
        // table dispatches first, so taking it would silence the snippet
        // with no sign of why. The snippet recorder refuses a chord the
        // table holds; this is the other half. The capture stays armed so
        // the next keypress can try another chord.
        if let Some(snippet) = self.snippets.iter().find(|sn| {
            sn.hotkey
                .as_deref()
                .and_then(crate::hotkeys::HotkeyBinding::parse)
                .is_some_and(|held| held.shares_keypress(&new_binding))
        }) {
            let toast = crate::i18n::t("hotkey_taken_by_snippet").replace("{name}", &snippet.label);
            self.set_toast(toast);
            return super::toast_clear_after_secs(3);
        }
        // Conflict resolution: take the chord away from whichever other
        // action holds it, and surface a toast that names *the action*
        // (not the key combo) so the family case reads "Switch to
        // specific Tab is now unbound" instead of "Ctrl+1 is now
        // unbound".
        //
        // The loser gives up only the disputed CHORD and keeps the rest
        // of its list. The single-binding model had to unbind the loser
        // outright, and papered over that by auto-rebinding it to its
        // factory default when that default happened to be free; with a
        // list there is nothing to paper over, and an action that still
        // has chords is simply still bound.
        //
        // Only an action that can see the same keypress is a conflict:
        // a vault-only action and a tab-only one never do
        // (`scopes_overlap`), so the chord stays on both and the screen
        // decides which fires.
        // A family chord can meet several actions at once (Alt+←/→ is
        // both pane-left's Alt+Left and pane-right's Alt+Right), so every
        // one of them gives the keypress up; the toast names them all.
        let mut conflicts: Vec<HotkeyAction> = self
            .hotkey_bindings
            .iter()
            .filter(|(a, b)| {
                **a != action
                    && b.shares_keypress(&new_binding)
                    && HotkeyAction::scopes_overlap(**a, action)
            })
            .map(|(a, _)| *a)
            .collect();
        conflicts.sort_by_key(|a| HotkeyAction::all().iter().position(|x| x == a));
        let mut unbound: Vec<&'static str> = Vec::new();
        let mut trimmed: Vec<&'static str> = Vec::new();
        for other in &conflicts {
            let mut left = self
                .hotkey_bindings
                .get(other)
                .cloned()
                .unwrap_or_default();
            left.remove_sharing(&new_binding);
            self.persist_setting(&format!("hotkey_{}", other.id()), &left.serialize());
            if left.is_empty() {
                self.hotkey_bindings.remove(other);
                unbound.push(crate::i18n::t(other.label_key()));
            } else {
                self.hotkey_bindings.insert(*other, left);
                trimmed.push(crate::i18n::t(other.label_key()));
            }
        }
        let conflict_toast: Option<Task<Message>> = (!conflicts.is_empty()).then(|| {
            let mut lines = Vec::new();
            if !unbound.is_empty() {
                lines.push(crate::i18n::t("hotkey_conflict_unbound").replace("{action}", &unbound.join(", ")));
            }
            if !trimmed.is_empty() {
                lines.push(crate::i18n::t("hotkey_conflict_chord_removed").replace("{action}", &trimmed.join(", ")));
            }
            self.set_toast(lines.join(" "));
            super::toast_clear_after_secs(3)
        });

        let mut binds = self.hotkey_bindings.get(&action).cloned().unwrap_or_default();
        binds.set(slot, new_binding);
        self.persist_setting(&format!("hotkey_{}", action.id()), &binds.serialize());
        self.hotkey_bindings.insert(action, binds);
        self.editing_hotkey = None;

        conflict_toast.unwrap_or_else(Task::none)
    }
}
