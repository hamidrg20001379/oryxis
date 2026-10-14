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
            SettingsMessage::ShortcutSequenceNew => {
                self.shortcut_sequence_form = crate::hotkeys::ShortcutSequenceForm {
                    open: true,
                    editing: None,
                    ..Default::default()
                };
            }
            SettingsMessage::ShortcutSequenceEdit(index) => {
                if let Some(sequence) = self.shortcut_sequences.get(index) {
                    self.shortcut_sequence_form = crate::hotkeys::ShortcutSequenceForm {
                        open: true,
                        editing: Some(index),
                        trigger: sequence.trigger.serialize(),
                        steps: sequence.steps.clone(),
                        selected_step: None,
                        error: None,
                    };
                }
            }
            SettingsMessage::ShortcutSequenceCancelEdit => {
                self.shortcut_sequence_form = Default::default();
            }
            SettingsMessage::ShortcutSequenceTriggerChanged(value) => {
                self.shortcut_sequence_form.trigger = value;
                self.shortcut_sequence_form.error = None;
            }
            SettingsMessage::ShortcutSequenceStepPicked(label) => {
                self.shortcut_sequence_form.selected_step = crate::hotkeys::HotkeyAction::all()
                    .iter()
                    .copied()
                    .find(|action| crate::i18n::t(action.label_key()) == label.as_str());
            }
            SettingsMessage::ShortcutSequenceAddStep => {
                if let Some(action) = self.shortcut_sequence_form.selected_step.take() {
                    self.shortcut_sequence_form.steps.push(action);
                }
            }
            SettingsMessage::ShortcutSequenceRemoveStep(index) => {
                if index < self.shortcut_sequence_form.steps.len() {
                    self.shortcut_sequence_form.steps.remove(index);
                }
            }
            SettingsMessage::ShortcutSequenceMoveStep(index, up) => {
                let target = if up { index.checked_sub(1) } else { Some(index + 1) };
                if let Some(target) = target
                    && target < self.shortcut_sequence_form.steps.len()
                {
                    self.shortcut_sequence_form.steps.swap(index, target);
                }
            }
            SettingsMessage::ShortcutSequenceSave => {
                let trigger_text = self.shortcut_sequence_form.trigger.trim().to_string();
                let steps = self.shortcut_sequence_form.steps.clone();
                let editing = self.shortcut_sequence_form.editing;
                let Some(trigger) = crate::hotkeys::HotkeyBinding::parse(&trigger_text) else {
                    self.shortcut_sequence_form.error = Some("Enter a valid shortcut such as ctrl+alt+q".into());
                    return Ok(Task::none());
                };
                if steps.is_empty() {
                    self.shortcut_sequence_form.error = Some("Add at least one step".into());
                    return Ok(Task::none());
                }
                let sequence = crate::hotkeys::ShortcutSequence {
                    trigger,
                    steps,
                };
                match editing {
                    Some(index) if index < self.shortcut_sequences.len() => {
                        self.shortcut_sequences[index] = sequence;
                    }
                    _ => self.shortcut_sequences.push(sequence),
                }
                self.persist_setting(
                    "shortcut_sequences",
                    &crate::hotkeys::ShortcutSequence::serialize_list(&self.shortcut_sequences),
                );
                self.shortcut_sequence_form = Default::default();
            }
            SettingsMessage::ShortcutSequenceDelete(index) => {
                if index < self.shortcut_sequences.len() {
                    self.shortcut_sequences.remove(index);
                    if self.shortcut_sequence_form.editing == Some(index) {
                        self.shortcut_sequence_form = Default::default();
                    }
                    self.persist_setting(
                        "shortcut_sequences",
                        &crate::hotkeys::ShortcutSequence::serialize_list(&self.shortcut_sequences),
                    );
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

