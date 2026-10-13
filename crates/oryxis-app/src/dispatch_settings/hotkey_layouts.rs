//! Settings > Shortcuts, whole sets at once: a preset, the export, the
//! import. The rules a set obeys live in `hotkeys::layout`; this half
//! talks to the user (the confirm, the file dialogs, the toasts) and
//! writes the result the way the editor does, one settings row per action.

use super::*;
use crate::hotkeys::layout::{self, ImportError, LayoutEntry, Mode, Refused, Skipped};
use crate::hotkeys::{HotkeyAction, HotkeyBinding, ShortcutPreset};

/// The largest file the import reads. A shortcuts file is a few KB; this
/// keeps a wrong pick (a log, a disk image) from being read whole.
const MAX_IMPORT_BYTES: u64 = 1024 * 1024;

impl Oryxis {
    pub(super) fn handle_settings_hotkey_layouts(
        &mut self,
        message: SettingsMessage,
    ) -> Result<Task<Message>, SettingsMessage> {
        let task = match message {
            SettingsMessage::HotkeyPresetPicked(Some(preset)) => {
                self.offer_hotkey_layout(preset.layout(), preset.name(), Vec::new(), Mode::KeyboardOnly)
            }
            // Oryxis's own: the empty layout, which is the factory table.
            SettingsMessage::HotkeyPresetPicked(None) => {
                self.offer_hotkey_layout(Vec::new(), "Oryxis".to_string(), Vec::new(), Mode::Exact)
            }
            SettingsMessage::ExportHotkeys => self.export_hotkeys(),
            SettingsMessage::HotkeysExported(Ok(path)) => {
                self.show_toast(crate::i18n::t("hotkey_export_done").replace("{path}", &path))
            }
            SettingsMessage::HotkeysExported(Err(e)) => {
                self.show_toast(crate::i18n::t("hotkey_export_failed").replace("{error}", &e))
            }
            SettingsMessage::ImportHotkeys => import_hotkeys_task(),
            SettingsMessage::HotkeysFileRead(_, Err(e)) => {
                self.show_toast(crate::i18n::t("hotkey_import_failed").replace("{error}", &e))
            }
            SettingsMessage::HotkeysFileRead(name, Ok(text)) => match layout::parse_import(&text) {
                Ok(plan) => self.offer_hotkey_layout(plan.layout, name, plan.refused, Mode::Exact),
                Err(ImportError::NotJson | ImportError::NotShortcuts) => {
                    self.show_toast(crate::i18n::t("hotkey_import_not_shortcuts").to_string())
                }
                Err(ImportError::NewerVersion(v)) => self.show_toast(
                    crate::i18n::t("hotkey_import_newer").replace("{version}", &v.to_string()),
                ),
            },
            SettingsMessage::ApplyHotkeyLayout(entries, name, mode) => {
                self.apply_hotkey_layout(&entries, &name, mode)
            }
            m => return Err(m),
        };
        Ok(task)
    }

    /// Which set the bindings are now, for the selector: Oryxis's own, a
    /// preset applied unchanged, or neither (`None` inside `Some` is
    /// Oryxis; the outer `None` is a hand-made set). Derived every time,
    /// never stored, so an edit in the table below is reflected at once.
    pub(crate) fn current_hotkey_preset(&self) -> Option<Option<ShortcutPreset>> {
        if layout::layout_of(&self.hotkey_bindings).is_empty() {
            return Some(None);
        }
        let reserved = self.snippet_chords();
        ShortcutPreset::available()
            .iter()
            .find(|p| {
                let (map, _) = layout::apply_layout(&p.layout(), &reserved, Mode::KeyboardOnly);
                same_bindings(&map, &self.hotkey_bindings)
            })
            .map(|p| Some(*p))
    }

    /// The chords snippets hold: a set never takes them.
    fn snippet_chords(&self) -> Vec<HotkeyBinding> {
        self.snippets
            .iter()
            .filter_map(|sn| sn.hotkey.as_deref().and_then(HotkeyBinding::parse))
            .collect()
    }

    /// Ask before replacing anything the user set by hand, or when some
    /// rows will not land; otherwise apply at once (Reset all brings the
    /// factory set back, so nothing is lost).
    fn offer_hotkey_layout(
        &mut self,
        entries: Vec<LayoutEntry>,
        name: String,
        refused: Vec<Refused>,
        mode: Mode,
    ) -> Task<Message> {
        let (target, applied) = layout::apply_layout(&entries, &self.snippet_chords(), mode);
        if same_bindings(&target, &self.hotkey_bindings) && refused.is_empty() {
            return self.show_toast(crate::i18n::t("hotkey_layout_already").replace("{name}", &name));
        }
        let hand_made = !layout::layout_of(&self.hotkey_bindings).is_empty();
        // Everything the user would not see coming: rows that do not
        // land, chords a snippet keeps, and actions left with no
        // shortcut at all because the set took their only chord.
        let notes: Vec<String> = refused
            .iter()
            .map(refused_line)
            .chain(applied.skipped.iter().map(skipped_line))
            .chain(applied.unbound.iter().map(|a| {
                crate::i18n::t("hotkey_layout_unbinds").replace("{action}", crate::i18n::t(a.label_key()))
            }))
            .collect();
        if !hand_made && notes.is_empty() {
            return self.apply_hotkey_layout(&entries, &name, mode);
        }
        // Replacing hand-made chords is the headline when there are some;
        // otherwise the question is just whether to apply, with the notes.
        let (title, mut body) = if hand_made {
            (
                crate::i18n::t("hotkey_layout_confirm_title").to_string(),
                crate::i18n::t("hotkey_layout_confirm_body").replace("{name}", &name),
            )
        } else {
            (crate::i18n::t("hotkey_layout_confirm_body_clean").replace("{name}", &name), String::new())
        };
        if !notes.is_empty() {
            if !body.is_empty() {
                body.push_str("\n\n");
            }
            body.push_str(crate::i18n::t("hotkey_layout_confirm_skips"));
            for line in notes {
                body.push_str("\n- ");
                body.push_str(&line);
            }
        }
        self.error_dialog = Some(crate::state::ErrorDialog {
            title,
            body,
            link: None,
            action: Some(crate::state::ErrorDialogAction {
                label: crate::i18n::t("hotkey_layout_confirm_apply").to_string(),
                message: Box::new(Message::Settings(SettingsMessage::ApplyHotkeyLayout(entries, name, mode))),
                danger: hand_made,
            }),
        });
        Task::none()
    }

    /// Make `entries` the bindings and write every action's row.
    fn apply_hotkey_layout(&mut self, entries: &[LayoutEntry], name: &str, mode: Mode) -> Task<Message> {
        // Recomputed, not carried from the offer: a snippet can have taken
        // a chord while the confirm was up.
        let (map, _) = layout::apply_layout(entries, &self.snippet_chords(), mode);
        for (action, value) in layout::settings_rows(&map) {
            self.persist_setting(&format!("hotkey_{}", action.id()), &value);
        }
        self.hotkey_bindings = map;
        self.editing_hotkey = None;
        self.show_toast(crate::i18n::t("hotkey_layout_applied").replace("{name}", name))
    }

    /// Save the current layout to a file the user picks.
    fn export_hotkeys(&self) -> Task<Message> {
        let body = layout::export_file(&self.hotkey_bindings);
        Task::perform(
            tokio::task::spawn_blocking(move || {
                let path = rfd::FileDialog::new()
                    .set_title(crate::i18n::t("hotkey_export_dialog_title"))
                    .set_file_name("oryxis-shortcuts.json")
                    .add_filter("JSON", &["json"])
                    .save_file()?;
                Some(
                    std::fs::write(&path, body)
                        .map(|_| path.display().to_string())
                        .map_err(|e| e.to_string()),
                )
            }),
            |res| match res {
                Ok(Some(outcome)) => Message::Settings(SettingsMessage::HotkeysExported(outcome)),
                // Dialog dismissed or the blocking task died.
                _ => Message::NoOp,
            },
        )
    }
}

/// Pick a shortcuts file and read it, off the UI thread.
fn import_hotkeys_task() -> Task<Message> {
    Task::perform(
        tokio::task::spawn_blocking(|| {
            let path = rfd::FileDialog::new()
                .set_title(crate::i18n::t("hotkey_import_dialog_title"))
                .add_filter("JSON", &["json"])
                .add_filter("All files", &["*"])
                .pick_file()?;
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            Some((name, read_capped(&path)))
        }),
        |res| match res {
            Ok(Some((name, read))) => Message::Settings(SettingsMessage::HotkeysFileRead(name, read)),
            _ => Message::NoOp,
        },
    )
}

/// The file's text, refusing anything past [`MAX_IMPORT_BYTES`].
fn read_capped(path: &std::path::Path) -> Result<String, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut text = String::new();
    file.take(MAX_IMPORT_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() as u64 > MAX_IMPORT_BYTES {
        return Err(crate::i18n::t("hotkey_import_too_big").to_string());
    }
    Ok(text)
}

/// Two maps bind the same chords to the same actions (absent = empty).
fn same_bindings(a: &crate::hotkeys::HotkeyMap, b: &crate::hotkeys::HotkeyMap) -> bool {
    HotkeyAction::all().iter().all(|action| {
        let of = |m: &crate::hotkeys::HotkeyMap| m.get(action).filter(|x| !x.is_empty()).cloned();
        of(a) == of(b)
    })
}

/// One refused row, as the confirm lists it.
fn refused_line(r: &Refused) -> String {
    match r {
        Refused::UnknownAction(id) => crate::i18n::t("hotkey_import_unknown").replace("{id}", id),
        Refused::Unreadable(id) => crate::i18n::t("hotkey_import_unreadable").replace("{id}", id),
        Refused::UnsafeChord { action, chord } => crate::i18n::t("hotkey_import_unsafe")
            .replace("{action}", crate::i18n::t(action.label_key()))
            .replace("{chord}", chord),
    }
}

/// One chord left with a snippet, as the confirm lists it.
fn skipped_line(s: &Skipped) -> String {
    match s {
        Skipped::Snippet { action, chord } => crate::i18n::t("hotkey_layout_snippet_kept")
            .replace("{action}", crate::i18n::t(action.label_key()))
            .replace("{chord}", &chord.badges().join("+")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_past_the_cap_is_refused() {
        let dir = std::env::temp_dir().join(format!("oryxis-hotkeys-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.json");
        std::fs::write(&path, vec![b' '; (MAX_IMPORT_BYTES + 10) as usize]).unwrap();
        assert!(read_capped(&path).is_err());
        std::fs::write(&path, b"{}").unwrap();
        assert_eq!(read_capped(&path).as_deref(), Ok("{}"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
