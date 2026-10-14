//! The Shortcuts editor: capturing a chord, resetting one, resetting all.
//!
//! Capture is armed here and answered by the routers in `shortcuts`,
//! which is why a mouse press is a settings message: it can be a
//! binding being recorded rather than one being fired.

use super::*;

impl Oryxis {
    pub(super) fn handle_settings_hotkeys(
        &mut self,
        message: SettingsMessage,
    ) -> Result<Task<Message>, SettingsMessage> {
        match message {
            SettingsMessage::StartEditingHotkey(action, slot) => {
                self.editing_hotkey = Some((action, slot));
            }
            SettingsMessage::MouseButtonPressed(button) => {
                return Ok(self.handle_mouse_button_press(button));
            }
            SettingsMessage::WheelCaptured(direction) => {
                return Ok(self.handle_hotkey_wheel_capture(direction));
            }
            SettingsMessage::ResetHotkey(action) => {
                let mut defaults = crate::hotkeys::default_bindings();
                match defaults.remove(&action) {
                    Some(d) => self.hotkey_bindings.insert(action, d),
                    None => self.hotkey_bindings.remove(&action),
                };
                // Empty value persists the absence of an override, so
                // future boots rehydrate to the default. Same
                // semantics as deleting the row, and distinct from the
                // UNBOUND token a deliberate unbind writes.
                self.persist_setting(&format!("hotkey_{}", action.id()), "");
            }
            SettingsMessage::ResetAllHotkeys => {
                self.hotkey_bindings = crate::hotkeys::default_bindings();
                for action in crate::hotkeys::HotkeyAction::all() {
                    self.persist_setting(&format!("hotkey_{}", action.id()), "");
                }
            }
            SettingsMessage::ToggleSecretVisibility(field) => {
                if !self.revealed_secrets.remove(&field) {
                    self.revealed_secrets.insert(field);
                }
            }
            m => return Err(m),
        }
        Ok(Task::none())
    }
}
