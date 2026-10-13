//! A whole set of bindings at once: applying one, writing one to a file,
//! reading one back.
//!
//! The Shortcuts editor changes one chord at a time; a preset and an
//! imported file change many, and every one of them has to obey the same
//! rules a single capture does. Those rules live here, pure, so the
//! preset tables, the file format and the editor cannot drift apart:
//!
//! - a chord must be one the editor would record (`is_safe`, and a family
//!   action keeps its own suffix: only `1...9` / `←/→` modifiers change);
//! - a chord taken by one action leaves every other action that can see
//!   the same keypress (`scopes_overlap`), which keeps its other chords;
//!   "the same keypress" includes a family and its members
//!   (`HotkeyBinding::shares_keypress`), and a family cannot give up one
//!   member, so it leaves whole;
//! - a chord a snippet holds is never taken: the binding table dispatches
//!   first and would silently steal the snippet's key.
//!
//! A LAYOUT is what differs from the factory table, never a full copy of
//! it: a file written today and read after a release that moves a factory
//! chord keeps the new chord for everything the file never touched.
//!
//! Two ways to apply one ([`Mode`]): a FILE is exact (it was written from
//! a full list, mouse and wheel chords included), a PRESET is keyboard
//! only (it was measured from another app's keymap, which says nothing
//! about Ctrl+wheel or the middle click, and dropping those would lose
//! the middle-click paste for good: its boot migration runs once).

use std::collections::BTreeMap;

use super::{default_bindings, HotkeyAction, HotkeyBinding, HotkeyBindings, HotkeyMap};

/// The marker a shortcuts file carries, and its format version.
pub const FILE_MARKER: &str = "oryxis_shortcuts";
pub const FILE_VERSION: u64 = 1;

/// One change of a layout: this action gets exactly these chords (an empty
/// list unbinds it).
pub type LayoutEntry = (HotkeyAction, HotkeyBindings);

/// Why a chord of a layout did not land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skipped {
    /// A snippet holds it.
    Snippet { action: HotkeyAction, chord: HotkeyBinding },
}

/// How a layout entry treats the action's existing chords.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The entry is the action's whole list.
    Exact,
    /// The entry replaces the action's KEYBOARD chords; its mouse and
    /// wheel chords stay.
    KeyboardOnly,
}

/// What applying a layout did besides the chords it set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    /// Actions that gave up a chord to the layout and kept none.
    pub unbound: Vec<HotkeyAction>,
    /// Chords the layout asked for and could not take.
    pub skipped: Vec<Skipped>,
}

/// `bindings`, normalized: absent and empty both mean unbound.
fn effective(map: &HotkeyMap, action: HotkeyAction) -> Option<&HotkeyBindings> {
    map.get(&action).filter(|b| !b.is_empty())
}

/// The factory table with `layout` applied, in order (a later entry wins a
/// chord over an earlier one). `reserved` are the chords snippets hold.
pub fn apply_layout(layout: &[LayoutEntry], reserved: &[HotkeyBinding], mode: Mode) -> (HotkeyMap, Applied) {
    let mut map = default_bindings();
    let mut applied = Applied::default();
    for (action, wanted) in layout {
        let mut chords = HotkeyBindings::default();
        for chord in wanted.iter() {
            if reserved.iter().any(|r| r.shares_keypress(chord)) {
                applied.skipped.push(Skipped::Snippet { action: *action, chord: *chord });
                continue;
            }
            chords.push(*chord);
        }
        // The chord leaves whoever else can see the same keypress; a
        // family that covers it leaves whole.
        let losers: Vec<HotkeyAction> = map
            .iter()
            .filter(|(other, held)| {
                **other != *action
                    && HotkeyAction::scopes_overlap(**other, *action)
                    && held.iter().any(|h| chords.iter().any(|c| h.shares_keypress(c)))
            })
            .map(|(other, _)| *other)
            .collect();
        for other in losers {
            if let Some(held) = map.get_mut(&other) {
                let gone: Vec<HotkeyBinding> = held
                    .iter()
                    .filter(|h| chords.iter().any(|c| h.shares_keypress(c)))
                    .copied()
                    .collect();
                for chord in &gone {
                    held.remove(chord);
                }
                if held.is_empty() {
                    map.remove(&other);
                    applied.unbound.push(other);
                }
            }
        }
        if mode == Mode::KeyboardOnly
            && let Some(kept) = map.get(action)
        {
            for chord in kept.iter().filter(|c| c.is_mouse() || c.is_wheel()) {
                chords.push(*chord);
            }
        }
        if chords.is_empty() {
            map.remove(action);
        } else {
            map.insert(*action, chords);
        }
    }
    applied.unbound.retain(|a| effective(&map, *a).is_none());
    applied.unbound.sort_by_key(|a| HotkeyAction::all().iter().position(|x| x == a));
    applied.unbound.dedup();
    (map, applied)
}

/// The settings row every action should hold so the next boot rebuilds
/// `map`: `""` where it matches the factory table (no override), the
/// serialized chords otherwise (`none` for a deliberate unbind).
pub fn settings_rows(map: &HotkeyMap) -> Vec<(HotkeyAction, String)> {
    let defaults = default_bindings();
    HotkeyAction::all()
        .iter()
        .map(|action| {
            let now = effective(map, *action);
            let value = if now == effective(&defaults, *action) {
                String::new()
            } else {
                now.cloned().unwrap_or_default().serialize()
            };
            (*action, value)
        })
        .collect()
}

/// The layout `map` is: every action whose chords differ from the factory
/// table, in catalog order.
pub fn layout_of(map: &HotkeyMap) -> Vec<LayoutEntry> {
    let defaults = default_bindings();
    HotkeyAction::all()
        .iter()
        .filter_map(|action| {
            let now = effective(map, *action);
            (now != effective(&defaults, *action)).then(|| (*action, now.cloned().unwrap_or_default()))
        })
        .collect()
}

/// A shortcuts file for `map`: its layout, keyed by action id.
pub fn export_file(map: &HotkeyMap) -> String {
    let bindings: BTreeMap<&str, String> = layout_of(map)
        .into_iter()
        .map(|(action, chords)| (action.id(), chords.serialize()))
        .collect();
    let doc = serde_json::json!({
        FILE_MARKER: FILE_VERSION,
        "bindings": bindings,
    });
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

/// Why a file is not a shortcuts file at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// Not JSON.
    NotJson,
    /// JSON without the marker, or without a `bindings` object.
    NotShortcuts,
    /// A version this build does not read.
    NewerVersion(u64),
}

/// A row of the file that does not land, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// No action of this build has that id (a newer build's action, or a
    /// typo).
    UnknownAction(String),
    /// The value is not a string, or none of its chords parse.
    Unreadable(String),
    /// A chord the editor would not record for this action.
    UnsafeChord { action: HotkeyAction, chord: String },
}

/// A file read: the layout it carries and the rows that do not land.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportPlan {
    pub layout: Vec<LayoutEntry>,
    pub refused: Vec<Refused>,
}

/// Read a shortcuts file. A row that cannot land is listed, never dropped
/// in silence, and never takes the rest of the file with it.
pub fn parse_import(text: &str) -> Result<ImportPlan, ImportError> {
    let doc: serde_json::Value = serde_json::from_str(text).map_err(|_| ImportError::NotJson)?;
    let version = doc.get(FILE_MARKER).and_then(|v| v.as_u64()).ok_or(ImportError::NotShortcuts)?;
    if version > FILE_VERSION {
        return Err(ImportError::NewerVersion(version));
    }
    let rows = doc.get("bindings").and_then(|b| b.as_object()).ok_or(ImportError::NotShortcuts)?;
    let defaults = default_bindings();
    let mut plan = ImportPlan::default();
    for action in HotkeyAction::all() {
        let Some(value) = rows.get(action.id()) else {
            continue;
        };
        let Some(text) = value.as_str() else {
            plan.refused.push(Refused::Unreadable(action.id().to_string()));
            continue;
        };
        match read_chords(*action, text, &defaults) {
            Ok(chords) => plan.layout.push((*action, chords)),
            Err(refused) => plan.refused.push(refused),
        }
    }
    for id in rows.keys() {
        if !HotkeyAction::all().iter().any(|a| a.id() == id) {
            plan.refused.push(Refused::UnknownAction(id.clone()));
        }
    }
    Ok(plan)
}

/// One row's chords, under the editor's rules.
fn read_chords(action: HotkeyAction, text: &str, defaults: &HotkeyMap) -> Result<HotkeyBindings, Refused> {
    let text = text.trim();
    if text == HotkeyBindings::default().serialize() {
        return Ok(HotkeyBindings::default());
    }
    let mut chords = HotkeyBindings::default();
    for word in text.split_whitespace() {
        let chord = HotkeyBinding::parse(word).ok_or_else(|| Refused::Unreadable(action.id().to_string()))?;
        if !chord_allowed(action, &chord, defaults) {
            return Err(Refused::UnsafeChord { action, chord: word.to_string() });
        }
        chords.push(chord);
    }
    if chords.is_empty() {
        return Err(Refused::Unreadable(action.id().to_string()));
    }
    Ok(chords)
}

/// Whether the editor would record `chord` for `action`: a safe chord, and
/// for a family action its own suffix (only the modifiers are editable).
pub fn chord_allowed(action: HotkeyAction, chord: &HotkeyBinding, defaults: &HotkeyMap) -> bool {
    if !chord.is_safe() {
        return false;
    }
    if action.primary_editable() {
        return true;
    }
    defaults
        .get(&action)
        .and_then(|b| b.primary())
        .is_some_and(|factory| factory.primary == chord.primary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkeys::PrimaryKey;
    use iced::keyboard::key::Named;

    fn chord(s: &str) -> HotkeyBinding {
        HotkeyBinding::parse(s).unwrap_or_else(|| panic!("chord {s}"))
    }

    fn chords(s: &str) -> HotkeyBindings {
        HotkeyBindings::parse(s).unwrap_or_else(|| panic!("chords {s}"))
    }

    #[test]
    fn an_empty_layout_is_the_factory_table() {
        let (map, applied) = apply_layout(&[], &[], Mode::Exact);
        assert_eq!(map, default_bindings());
        assert_eq!(applied, Applied::default());
        assert!(layout_of(&map).is_empty());
        assert!(settings_rows(&map).iter().all(|(_, v)| v.is_empty()));
    }

    #[test]
    fn a_taken_chord_leaves_the_action_that_held_it() {
        // The close-tab chord, whatever this platform ships it as.
        let factory = default_bindings()[&HotkeyAction::CloseActiveTab].clone();
        let held = factory.primary().expect("close tab ships bound");
        let (map, applied) =
            apply_layout(&[(HotkeyAction::TerminalSaveOutput, HotkeyBindings::single(held))], &[], Mode::Exact);
        assert!(map[&HotkeyAction::TerminalSaveOutput].contains(&held));
        assert!(map.get(&HotkeyAction::CloseActiveTab).is_none_or(|b| !b.contains(&held)));
        // It keeps any other chord it had; with none left it is reported.
        assert_eq!(
            applied.unbound.contains(&HotkeyAction::CloseActiveTab),
            factory.len() == 1
        );
    }

    #[test]
    fn a_vault_action_keeps_a_chord_a_tab_action_takes() {
        // Ctrl+Enter connects the ringed host card (vault only); a tab
        // verb taking it must not strip the vault side.
        let enter = chord("ctrl+enter");
        if !default_bindings()[&HotkeyAction::ConnectInNewWindow].contains(&enter) {
            return; // macOS ships Cmd+Enter; the rule is the same.
        }
        let (map, _) =
            apply_layout(&[(HotkeyAction::MoveTabToNewWindow, HotkeyBindings::single(enter))], &[], Mode::Exact);
        assert!(map[&HotkeyAction::ConnectInNewWindow].contains(&enter));
        assert!(map[&HotkeyAction::MoveTabToNewWindow].contains(&enter));
    }

    #[test]
    fn a_snippet_chord_is_never_taken() {
        let snip = chord("ctrl+alt+k");
        let (map, applied) =
            apply_layout(&[(HotkeyAction::TerminalSaveOutput, HotkeyBindings::single(snip))], &[snip], Mode::Exact);
        assert!(!map.contains_key(&HotkeyAction::TerminalSaveOutput));
        assert_eq!(
            applied.skipped,
            vec![Skipped::Snippet { action: HotkeyAction::TerminalSaveOutput, chord: snip }]
        );
    }

    #[test]
    fn a_family_leaves_whole_when_one_member_is_taken() {
        // Alt+Left for pane focus is a key of Alt+←/→ (tab cycling): the
        // family cannot keep Alt+Right alone, so it goes.
        let family = default_bindings()[&HotkeyAction::CycleTabs].clone();
        let Some(cycle) = family.primary() else { return };
        let left = HotkeyBinding { primary: PrimaryKey::Named(Named::ArrowLeft), ..cycle };
        let (map, applied) =
            apply_layout(&[(HotkeyAction::FocusPaneLeft, HotkeyBindings::single(left))], &[], Mode::Exact);
        assert!(map.get(&HotkeyAction::CycleTabs).is_none_or(|b| !b.contains(&cycle)));
        assert_eq!(applied.unbound.contains(&HotkeyAction::CycleTabs), family.len() == 1);
    }

    #[test]
    fn a_keyboard_preset_keeps_the_mouse_and_the_wheel() {
        let factory = default_bindings();
        let paste_sel = &factory[&HotkeyAction::TerminalPasteSelection];
        let zoom_in = &factory[&HotkeyAction::FontZoomIn];
        let layout = vec![
            (HotkeyAction::TerminalPasteSelection, chords("ctrl+shift+o")),
            (HotkeyAction::FontZoomIn, chords("ctrl+shift+u")),
        ];
        let (map, _) = apply_layout(&layout, &[], Mode::KeyboardOnly);
        for (action, factory) in [(HotkeyAction::TerminalPasteSelection, paste_sel), (HotkeyAction::FontZoomIn, zoom_in)] {
            for chord in factory.iter().filter(|c| c.is_mouse() || c.is_wheel()) {
                assert!(map[&action].contains(chord), "{action:?} lost {chord:?}");
            }
            assert!(map[&action].iter().all(|c| c.is_mouse() || c.is_wheel() || layout.iter().any(|(a, b)| *a == action && b.contains(c))));
        }
        let (exact, _) = apply_layout(&layout, &[], Mode::Exact);
        assert!(exact[&HotkeyAction::FontZoomIn].iter().all(|c| !c.is_wheel()));
    }

    #[test]
    fn a_file_round_trips_its_layout() {
        let layout = vec![
            (HotkeyAction::TerminalSaveOutput, chords("ctrl+shift+o")),
            (HotkeyAction::CloseActiveTab, HotkeyBindings::default()),
        ];
        let (map, _) = apply_layout(&layout, &[], Mode::Exact);
        let plan = parse_import(&export_file(&map)).expect("own file");
        assert!(plan.refused.is_empty(), "{:?}", plan.refused);
        let (again, _) = apply_layout(&plan.layout, &[], Mode::Exact);
        assert_eq!(layout_of(&again), layout_of(&map));
    }

    #[test]
    fn rows_that_cannot_land_are_listed_not_dropped() {
        let text = r#"{
            "oryxis_shortcuts": 1,
            "bindings": {
                "terminal_save_output": "ctrl+shift+o",
                "an_action_from_the_future": "ctrl+shift+q",
                "terminal_copy_screen": "k",
                "switch_to_tab_slot": "alt+f1",
                "close_active_tab": 7
            }
        }"#;
        let plan = parse_import(text).expect("a shortcuts file");
        assert_eq!(plan.layout, vec![(HotkeyAction::TerminalSaveOutput, chords("ctrl+shift+o"))]);
        assert!(plan.refused.contains(&Refused::UnknownAction("an_action_from_the_future".into())));
        assert!(plan.refused.contains(&Refused::UnsafeChord {
            action: HotkeyAction::TerminalCopyScreen,
            chord: "k".into()
        }));
        // A family action keeps its suffix: F1 is not "1...9".
        assert!(plan.refused.iter().any(|r| matches!(
            r,
            Refused::UnsafeChord { action: HotkeyAction::SwitchToTabSlot, .. }
        )));
        assert!(plan.refused.contains(&Refused::Unreadable("close_active_tab".into())));
    }

    #[test]
    fn a_file_that_is_not_ours_is_refused_whole() {
        assert_eq!(parse_import("not json"), Err(ImportError::NotJson));
        assert_eq!(parse_import(r#"{"bindings": {}}"#), Err(ImportError::NotShortcuts));
        assert_eq!(
            parse_import(r#"{"oryxis_shortcuts": 9, "bindings": {}}"#),
            Err(ImportError::NewerVersion(9))
        );
    }
}
