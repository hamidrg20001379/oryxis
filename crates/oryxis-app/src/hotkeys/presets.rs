//! Shortcut presets: another terminal's keymap, applied over Oryxis's.
//!
//! Every chord here was READ from the app's own shipped defaults on
//! 2026-10-13, never recalled: the source and line ride each table. A row
//! is here only when the app's action is the Oryxis action exactly; close
//! cousins (Tabby's broadcast that only arms, iTerm2's undo-close that
//! also brings windows back) are left out, and so is any chord this model
//! cannot express (Ctrl+Tab tab cycling, a tenth tab slot). A preset is the
//! app's chords for the actions it has, exactly (an Oryxis alternate the
//! app lacks goes), and keyboard only: mouse and wheel chords stay
//! (`layout::Mode::KeyboardOnly`). Actions the app has no chord for keep
//! Oryxis's, unless the preset takes their chord, which the Shortcuts
//! confirm names before anything changes.
//!
//! Re-measure before editing a row: these apps move their defaults.

use super::layout::LayoutEntry;
use super::{HotkeyAction, HotkeyBindings};

/// A keymap the Shortcuts editor offers as a starting point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShortcutPreset {
    /// iTerm2 (macOS only).
    ITerm2,
    /// Tabby, in the platform's own variant.
    Tabby,
}

impl ShortcutPreset {
    /// The presets that make sense on this platform.
    pub fn available() -> &'static [ShortcutPreset] {
        if cfg!(target_os = "macos") {
            &[ShortcutPreset::ITerm2, ShortcutPreset::Tabby]
        } else {
            &[ShortcutPreset::Tabby]
        }
    }

    /// The app's own name (a proper noun, not translated).
    pub fn name(self) -> String {
        match self {
            ShortcutPreset::ITerm2 => "iTerm2",
            ShortcutPreset::Tabby => "Tabby",
        }
        .to_string()
    }

    /// The preset's rows, in Oryxis's chord spelling.
    pub fn layout(self) -> Vec<LayoutEntry> {
        let rows: &[(HotkeyAction, &str)] = match self {
            ShortcutPreset::ITerm2 => ITERM2,
            ShortcutPreset::Tabby if cfg!(target_os = "macos") => TABBY_MACOS,
            ShortcutPreset::Tabby if cfg!(target_os = "windows") => TABBY_WINDOWS,
            ShortcutPreset::Tabby => TABBY_LINUX,
        };
        rows.iter()
            .filter_map(|(action, chords)| HotkeyBindings::parse(chords).map(|b| (*action, b)))
            .collect()
    }
}

impl std::fmt::Display for ShortcutPreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name())
    }
}

use HotkeyAction::*;

/// iTerm2, `master` 069b6e0dd8f3f0d82fc08ea5d294d28f7c5b986e:
/// menu equivalents from `sources/MainMenu/Base.lproj/MainMenu.xib`
/// (a key with no modifier mask is Cmd alone, an uppercase key implies
/// Shift), tab cycling and paging from `plists/DefaultGlobalKeyMap.plist`,
/// the tab-select modifier from `sources/Settings/iTermPreferences.m` L877.
/// https://github.com/gnachman/iTerm2/blob/069b6e0dd8f3f0d82fc08ea5d294d28f7c5b986e/
const ITERM2: &[(HotkeyAction, &str)] = &[
    (ShowNewTabPicker, "logo+t"),             // xib L211 "New Tab" keyEquivalent="t"
    (NewWindow, "logo+n"),                    // xib L197 "New Window" keyEquivalent="n"
    (CloseActiveTab, "logo+w"),               // xib L300 "Close" (closeCurrentSession:)
    (SplitPaneVertical, "logo+d"),            // xib L261 "Split Vertically..." "d"
    (SplitPaneHorizontal, "shift+logo+d"),    // xib L255 "Split Horizontally..." "D"
    (FocusPaneUp, "alt+logo+up"),             // xib L2013 "Select Pane Above", option+command
    (FocusPaneDown, "alt+logo+down"),         // xib L2020 "Select Pane Below"
    (FocusPaneLeft, "alt+logo+left"),         // xib L2027 "Select Pane Left"
    (FocusPaneRight, "alt+logo+right"),       // xib L2034 "Select Pane Right"
    (ToggleMaximizePane, "shift+logo+enter"), // xib L1268 "Maximize Active Pane", \r, shift+command
    (CycleTabs, "logo+arrows"),               // plist L40 / L47 0xf702 / 0xf703 + Cmd: Previous / Next Tab
    (SwitchToTabSlot, "logo+digit"),          // iTermPreferences.m L877 SwitchTabModifier = Command
    (TerminalCopy, "logo+c"),                 // xib L581 "Copy" "c"
    (TerminalPaste, "logo+v"),                // xib L607 "Paste" "v"
    (TerminalPasteSelection, "shift+logo+v"), // xib L624 "Paste Selection" "V"
    (TerminalSelectAll, "logo+a"),            // xib L782 "Select All" "a"
    (FocusViewSearch, "logo+f"),              // xib L814 "Find..." "f"
    (TerminalClearScrollback, "shift+logo+k"), // xib L1086 "Clear Scrollback Buffer" "K"
    (ToggleBroadcastInput, "alt+logo+i"),     // xib L395 "Broadcast Input to All Panes in Current Tab", option+command
    (ToggleFullscreen, "ctrl+logo+f"),        // xib L1117 "Toggle Full Screen", control+command
    (FontZoomIn, "logo+= shift+logo+="),      // xib L1281 "Make Text Bigger" "+" (Shift+= on US), beside Cmd+=
    (FontZoomOut, "logo+-"),                  // xib L1300 "Make Text Smaller" "-"
    (FontZoomReset, "logo+0"),                // xib L1287 "Make Text Normal Size" "0"
    (OpenSettings, "logo+,"),                 // xib L92 "Preferences..." ","
    (ScrollbackPageUp, "logo+pgup shift+pgup"), // plist L68 / L75 0xf72c + Cmd / Shift: page up
    (ScrollbackPageDown, "logo+pgdn shift+pgdn"), // plist L82 / L89 0xf72d + Cmd / Shift: page down
];

// Tabby, `master` 467ce8ebfa467fb88cf6d154c9a5891e3d3647f0:
// tabby-core/src/configDefaults.{windows,linux,macos}.yaml (tabs, panes,
// palette, fullscreen), tabby-terminal/src/config.ts (copy, paste, zoom,
// search, scroll), tabby-local/src/config.ts (new-tab), tabby-settings and
// tabby-electron config.ts (settings, new-window).
// https://github.com/Eugeny/tabby/blob/467ce8ebfa467fb88cf6d154c9a5891e3d3647f0/

/// Tabby on Windows (`configDefaults.windows.yaml`).
const TABBY_WINDOWS: &[(HotkeyAction, &str)] = &[
    (ShowNewTabPicker, "ctrl+shift+e"),       // windows.yaml L101 profile-selector 'Ctrl-Shift-E'
    (OpenLocalShell, "ctrl+shift+t"),         // tabby-local new-tab 'Ctrl-Shift-T' (the default local profile)
    (ReopenClosedTab, "ctrl+shift+z"),        // windows.yaml L7 reopen-tab 'Ctrl-Shift-Z'
    (RenameTab, "ctrl+shift+r"),              // windows.yaml L10 rename-tab 'Ctrl-Shift-R'
    (CycleTabs, "ctrl+shift+arrows"),         // windows.yaml L12-17 next-tab 'Ctrl-Shift-Right', previous-tab 'Ctrl-Shift-Left'
    (SwitchToTabSlot, "alt+digit"),           // windows.yaml L35 tab-1 'Alt-1' ... tab-9 'Alt-9'
    (SplitPaneVertical, "ctrl+shift+s"),      // windows.yaml L65 split-right 'Ctrl-Shift-S'
    (SplitPaneHorizontal, "ctrl+shift+d"),    // windows.yaml L67 split-bottom 'Ctrl-Shift-D'
    (FocusPaneRight, "ctrl+alt+right"),       // windows.yaml L71 pane-nav-right 'Ctrl-Alt-Right'
    (FocusPaneDown, "ctrl+alt+down"),         // windows.yaml L73 pane-nav-down
    (FocusPaneUp, "ctrl+alt+up"),             // windows.yaml L75 pane-nav-up
    (FocusPaneLeft, "ctrl+alt+left"),         // windows.yaml L77 pane-nav-left
    (ToggleMaximizePane, "ctrl+alt+enter"),   // windows.yaml L92 pane-maximize 'Ctrl-Alt-Enter'
    (ShowCommandPalette, "ctrl+shift+p"),     // windows.yaml L103 command-selector 'Ctrl-Shift-P'
    (ToggleFullscreen, "f11 alt+enter"),      // windows.yaml L2-4 toggle-fullscreen 'F11', 'Alt-Enter'
    (TerminalCopy, "ctrl+shift+c"),           // terminal config.ts L133 copy 'Ctrl-Shift-C'
    (TerminalPaste, "ctrl+shift+v shift+ins"), // terminal config.ts L136-138 paste 'Ctrl-Shift-V', 'Shift-Insert'
    (TerminalSelectAll, "ctrl+shift+a"),      // terminal config.ts L140 select-all 'Ctrl-Shift-A'
    (FontZoomIn, "ctrl+= ctrl+shift+="),      // terminal config.ts L142-144 zoom-in 'Ctrl-=', 'Ctrl-Shift-='
    (FontZoomOut, "ctrl+- ctrl+shift+-"),     // terminal config.ts L146-148 zoom-out 'Ctrl--', 'Ctrl-Shift--'
    (FontZoomReset, "ctrl+0"),                // terminal config.ts L150 reset-zoom 'Ctrl-0'
    (FocusViewSearch, "ctrl+shift+f"),        // terminal config.ts L160 search 'Ctrl-Shift-F'
    (ScrollbackPageUp, "alt+pgup"),           // terminal config.ts L171 scroll-page-up 'Alt-PageUp'
    (ScrollbackPageDown, "alt+pgdn"),         // terminal config.ts L174 scroll-page-down 'Alt-PageDown'
    (OpenSettings, "ctrl+,"),                 // tabby-settings settings 'Ctrl-,'
    (NewWindow, "ctrl+shift+n"),              // tabby-electron new-window 'Ctrl-Shift-N'
];

/// Tabby on Linux: the Windows table but for fullscreen, where
/// `configDefaults.linux.yaml` ships F11 alone.
const TABBY_LINUX: &[(HotkeyAction, &str)] = &[
    (ShowNewTabPicker, "ctrl+shift+e"),
    (OpenLocalShell, "ctrl+shift+t"),
    (ReopenClosedTab, "ctrl+shift+z"),
    (RenameTab, "ctrl+shift+r"),
    (CycleTabs, "ctrl+shift+arrows"),
    (SwitchToTabSlot, "alt+digit"),
    (SplitPaneVertical, "ctrl+shift+s"),
    (SplitPaneHorizontal, "ctrl+shift+d"),
    (FocusPaneRight, "ctrl+alt+right"),
    (FocusPaneDown, "ctrl+alt+down"),
    (FocusPaneUp, "ctrl+alt+up"),
    (FocusPaneLeft, "ctrl+alt+left"),
    (ToggleMaximizePane, "ctrl+alt+enter"),
    (ShowCommandPalette, "ctrl+shift+p"),
    (ToggleFullscreen, "f11"),                // linux.yaml toggle-fullscreen 'F11'
    (TerminalCopy, "ctrl+shift+c"),
    (TerminalPaste, "ctrl+shift+v shift+ins"),
    (TerminalSelectAll, "ctrl+shift+a"),
    (FontZoomIn, "ctrl+= ctrl+shift+="),
    (FontZoomOut, "ctrl+- ctrl+shift+-"),
    (FontZoomReset, "ctrl+0"),
    (FocusViewSearch, "ctrl+shift+f"),
    (ScrollbackPageUp, "alt+pgup"),
    (ScrollbackPageDown, "alt+pgdn"),
    (OpenSettings, "ctrl+,"),
    (NewWindow, "ctrl+shift+n"),
];

/// Tabby on macOS (`configDefaults.macos.yaml` and the `[Platform.macOS]`
/// blocks). Its tab cycling is Ctrl+Tab, which this model cannot express,
/// so tab cycling keeps Oryxis's chord.
const TABBY_MACOS: &[(HotkeyAction, &str)] = &[
    (ShowNewTabPicker, "logo+e"),             // profile-selector '⌘-E'
    (OpenLocalShell, "logo+t"),               // new-tab '⌘-T'
    (CloseActiveTab, "shift+logo+w"),         // close-pane '⌘-Shift-W'
    (ReopenClosedTab, "shift+logo+t"),        // reopen-tab '⌘-Shift-T'
    (RenameTab, "logo+r"),                    // rename-tab '⌘-R'
    (SwitchToTabSlot, "logo+digit"),          // tab-1..9 '⌘-1'..'⌘-9'
    (SplitPaneVertical, "shift+logo+d"),      // split-right '⌘-Shift-D'
    (SplitPaneHorizontal, "logo+d"),          // split-bottom '⌘-D'
    (FocusPaneLeft, "alt+logo+left"),         // pane-nav-left '⌘-⌥-Left'
    (FocusPaneRight, "alt+logo+right"),       // pane-nav-right '⌘-⌥-Right'
    (FocusPaneUp, "alt+logo+up"),             // pane-nav-up '⌘-⌥-Up'
    (FocusPaneDown, "alt+logo+down"),         // pane-nav-down '⌘-⌥-Down'
    (ToggleMaximizePane, "alt+logo+enter"),   // pane-maximize '⌘-⌥-Enter'
    (ShowCommandPalette, "shift+logo+p"),     // command-selector '⌘-Shift-P'
    (ToggleFullscreen, "ctrl+logo+f"),        // toggle-fullscreen 'Ctrl+⌘+F'
    (FocusViewSearch, "logo+f"),              // search '⌘-F'
    (ScrollbackPageUp, "alt+pgup"),           // scroll-page-up '⌥-PageUp'
    (ScrollbackPageDown, "alt+pgdn"),         // scroll-page-down '⌥-PageDown'
    (FontZoomIn, "logo+= shift+logo+="),      // zoom-in '⌘-=', '⌘-Shift-='
    (NewWindow, "logo+n"),                    // new-window '⌘-N'
    (OpenSettings, "logo+,"),                 // settings '⌘-,'
];

#[cfg(test)]
mod tests {
    use super::super::default_bindings;
    use super::super::layout::{apply_layout, chord_allowed, Mode};
    use super::*;

    /// Every row parses whole, and every chord is one the editor would
    /// record for its action: a row that silently lost a chord would ship
    /// a preset that is not the app it names.
    fn assert_table(rows: &[(HotkeyAction, &str)]) {
        let defaults = default_bindings();
        for (action, chords) in rows {
            let words = chords.split_whitespace().count();
            let parsed = HotkeyBindings::parse(chords).unwrap_or_else(|| panic!("{action:?}: {chords}"));
            assert_eq!(parsed.len(), words, "{action:?}: {chords}");
            for chord in parsed.iter() {
                assert!(chord_allowed(*action, chord, &defaults), "{action:?}: {}", chord.serialize());
            }
        }
        let mut seen = std::collections::HashSet::new();
        assert!(rows.iter().all(|(a, _)| seen.insert(*a)), "an action listed twice");
    }

    #[test]
    fn every_table_is_well_formed() {
        assert_table(ITERM2);
        assert_table(TABBY_WINDOWS);
        assert_table(TABBY_LINUX);
        assert_table(TABBY_MACOS);
    }

    /// What each preset takes from Oryxis actions it has no chord for: the
    /// conflict table of the research, kept as a guarantee. A preset that
    /// starts taking more must say so here first.
    #[test]
    fn each_preset_unbinds_exactly_what_it_must() {
        for preset in ShortcutPreset::available() {
            let (_, applied) = apply_layout(&preset.layout(), &[], Mode::KeyboardOnly);
            assert!(applied.skipped.is_empty());
            let expected: &[HotkeyAction] = match preset {
                ShortcutPreset::ITerm2 => &[NewHost, NewKey, DuplicateTab],
                ShortcutPreset::Tabby if cfg!(target_os = "macos") => &[DuplicateTab, NewHost],
                ShortcutPreset::Tabby => {
                    &[DuplicateTab, ReconnectTab, OpenSftp, OpenSftpConsole, ToggleTabFiles]
                }
            };
            let mut got = applied.unbound.clone();
            got.sort_by_key(|a| a.id());
            let mut want = expected.to_vec();
            want.sort_by_key(|a| a.id());
            assert_eq!(got, want, "{preset:?}");
        }
    }

    #[test]
    fn a_preset_keeps_the_middle_click_and_ctrl_wheel() {
        let factory = default_bindings();
        for preset in ShortcutPreset::available() {
            let (map, _) = apply_layout(&preset.layout(), &[], Mode::KeyboardOnly);
            for (action, chords) in &factory {
                for chord in chords.iter().filter(|c| c.is_mouse() || c.is_wheel()) {
                    assert!(map.get(action).is_some_and(|b| b.contains(chord)), "{preset:?} dropped {action:?} {}", chord.serialize());
                }
            }
        }
    }
}
