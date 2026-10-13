//! What the user pressed: the primary key or mouse button, the
//! modifiers around it, the serialized form persisted in the settings
//! table, and the matcher that turns an incoming iced event into a
//! [`FamilyMatch`].
//!
//! `action.rs` decides what MAY be bound; this decides what a binding
//! IS and whether an event hits one.

use std::fmt::Write;

use iced::keyboard::{key::Named, Key, Modifiers};

/// A mouse button that can stand in for the primary key of a binding.
///
/// Left and Right are deliberately NOT in this set. Both are the
/// terminal canvas's own gestures (select / the PuTTY right-click
/// scheme), and binding either would take a gesture away from the
/// terminal with no way back. Everything else is fair game: none of
/// these buttons produce text, so a bare mouse binding is a chord on
/// its own, no modifier required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Middle,
    Back,
    Forward,
    /// Any further button the OS reports by index (thumb buttons past
    /// Back / Forward, tilt-wheel clicks, ...).
    Other(u16),
}

impl MouseButton {
    /// The bindable subset of iced's button set. `None` for Left and
    /// Right, the two the terminal keeps for itself.
    pub fn from_iced(button: iced::mouse::Button) -> Option<Self> {
        match button {
            iced::mouse::Button::Middle => Some(Self::Middle),
            iced::mouse::Button::Back => Some(Self::Back),
            iced::mouse::Button::Forward => Some(Self::Forward),
            iced::mouse::Button::Other(n) => Some(Self::Other(n)),
            iced::mouse::Button::Left | iced::mouse::Button::Right => None,
        }
    }

    /// Settings-table token. The `mouse_` prefix is what keeps these
    /// clear of every other primary: no `Named` name, punctuation token
    /// or single alphanumeric char can collide with it, so `parse` can
    /// try mouse buttons first without shadowing anything.
    pub fn token(self) -> String {
        match self {
            Self::Middle => "mouse_middle".into(),
            Self::Back => "mouse_back".into(),
            Self::Forward => "mouse_forward".into(),
            Self::Other(n) => format!("mouse_{n}"),
        }
    }

    /// Reverse of [`MouseButton::token`].
    pub fn parse_token(s: &str) -> Option<Self> {
        match s {
            "mouse_middle" => Some(Self::Middle),
            "mouse_back" => Some(Self::Back),
            "mouse_forward" => Some(Self::Forward),
            other => other
                .strip_prefix("mouse_")
                .and_then(|n| n.parse::<u16>().ok())
                .map(Self::Other),
        }
    }

    /// Whether this is a SIDE button (the thumb pair and anything past
    /// it), as opposed to the wheel click.
    ///
    /// The distinction decides where the button may fire. Nothing in
    /// the app reacts to a side button: iced's `button` / `scrollable`
    /// / `text_input` all act on the primary, and the terminal canvas
    /// claims primary / secondary / middle. So a side button is free
    /// window-wide and can carry any action.
    ///
    /// The wheel click is not free: the canvas spends it on mouse
    /// reports and the X11 paste, and a middle click over a list or a
    /// scrollbar is a gesture users expect elsewhere too. It stays
    /// terminal-scoped.
    pub fn is_side_button(self) -> bool {
        !matches!(self, Self::Middle)
    }

    /// User-facing badge label, translated. Deliberately short: it
    /// shares a chip with the modifier badges.
    pub fn label(self) -> String {
        match self {
            Self::Middle => crate::i18n::t("mouse_btn_middle").to_string(),
            Self::Back => crate::i18n::t("mouse_btn_back").to_string(),
            Self::Forward => crate::i18n::t("mouse_btn_forward").to_string(),
            Self::Other(n) => crate::i18n::t("mouse_btn_other").replace("{n}", &n.to_string()),
        }
    }
}

/// A wheel direction that can stand in for the primary key of a
/// binding.
///
/// The wheel is the one bindable input that ALSO has a meaning of its
/// own: a bare wheel scrolls, and Shift+wheel is the mouse-report
/// bypass and the horizontal-scroll idiom. So unlike a mouse button, a
/// wheel chord is only safe under Ctrl / Alt / Logo (see
/// [`HotkeyBinding::is_safe`]), and it only ever fires over the
/// terminal canvas, which is the one surface that reads the wheel
/// itself. Ctrl+wheel zoom is the factory case, and it is what Windows
/// synthesizes from a precision-touchpad pinch, which is why the
/// gesture that looked hard-wired is a chord like any other (#225).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelDirection {
    Up,
    Down,
}

impl WheelDirection {
    /// Settings-table token. The `wheel_` prefix keeps it clear of
    /// every other primary the way `mouse_` does for buttons.
    pub fn token(self) -> &'static str {
        match self {
            Self::Up => "wheel_up",
            Self::Down => "wheel_down",
        }
    }

    /// Reverse of [`WheelDirection::token`].
    pub fn parse_token(s: &str) -> Option<Self> {
        match s {
            "wheel_up" => Some(Self::Up),
            "wheel_down" => Some(Self::Down),
            _ => None,
        }
    }

    /// The direction of a scroll delta, `None` for a horizontal-only
    /// event (a tilt names no vertical direction).
    pub fn from_delta_y(y: f32) -> Option<Self> {
        if y > 0.0 {
            Some(Self::Up)
        } else if y < 0.0 {
            Some(Self::Down)
        } else {
            None
        }
    }

    /// User-facing badge label, translated. Short, like the button
    /// labels: it shares a chip with the modifier badges.
    pub fn label(self) -> String {
        match self {
            Self::Up => crate::i18n::t("mouse_wheel_up").to_string(),
            Self::Down => crate::i18n::t("mouse_wheel_down").to_string(),
        }
    }
}

/// The non-modifier half of a binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryKey {
    /// A printable character, ASCII case-insensitive (`'k'` matches
    /// both `"k"` and `"K"`).
    Char(char),
    /// A named key (F11, Escape, ',', '=', ...). Stored as
    /// `iced::keyboard::key::Named` plus an optional character
    /// fallback for punctuation.
    Named(Named),
    /// Single-char punctuation that iced reports as `Character` not
    /// `Named` (`,`, `=`, `-`, `+`). Kept as a distinct variant from
    /// `Char` because the editor needs to know it's punctuation when
    /// rendering the badge.
    Punct(&'static str),
    /// Family: any digit 1..9. Suffix isn't editable.
    Digit1to9,
    /// Family: ArrowLeft or ArrowRight. Suffix isn't editable.
    ArrowLeftRight,
    /// A mouse button, optionally with modifiers. Only fires inside the
    /// terminal canvas (that is the one surface where a click can't
    /// belong to a widget), so only `HotkeyAction::accepts_mouse`
    /// actions may hold one.
    Mouse(MouseButton),
    /// A wheel notch in one direction, always with a modifier. Only
    /// fires over the terminal canvas, so only
    /// `HotkeyAction::accepts_wheel` actions may hold one.
    Wheel(WheelDirection),
}

/// What `HotkeyBinding::match_event` returns: `None` if the event
/// didn't match this binding; `Some(FamilyMatch)` if it did, carrying
/// any extracted payload from the family variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FamilyMatch {
    /// Plain match, no payload.
    Plain,
    /// Digit family matched digit `n` (1..=9).
    Digit(u8),
    /// Arrow family matched left arrow.
    ArrowLeft,
    /// Arrow family matched right arrow.
    ArrowRight,
}

/// A modifier set + primary key. `Modifiers` from iced isn't stored
/// directly so we can `PartialEq` and serialize it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyBinding {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub logo: bool,
    pub primary: PrimaryKey,
}

impl HotkeyBinding {
    /// Returns `Some(FamilyMatch)` when this binding fires for the
    /// given event, otherwise `None`. Modifier match is exact (a
    /// binding with no Shift won't fire when Shift is held), this
    /// avoids the `Ctrl+1` / `Ctrl+!` confusion on US layouts.
    pub fn match_event(&self, key: &Key, modifiers: &Modifiers) -> Option<FamilyMatch> {
        if modifiers.control() != self.ctrl
            || modifiers.shift() != self.shift
            || modifiers.alt() != self.alt
            || modifiers.logo() != self.logo
        {
            return None;
        }
        match self.primary {
            PrimaryKey::Char(c) => match key {
                Key::Character(s) => {
                    let s = s.as_str();
                    if s.len() == 1 && s.eq_ignore_ascii_case(&c.to_string()) {
                        Some(FamilyMatch::Plain)
                    } else {
                        None
                    }
                }
                _ => None,
            },
            PrimaryKey::Named(n) => match key {
                Key::Named(actual) if *actual == n => Some(FamilyMatch::Plain),
                _ => None,
            },
            PrimaryKey::Punct(p) => match key {
                Key::Character(s) if s.as_str() == p => Some(FamilyMatch::Plain),
                _ => None,
            },
            PrimaryKey::Digit1to9 => match key {
                Key::Character(s) => s
                    .as_str()
                    .chars()
                    .next()
                    .and_then(|ch| ch.to_digit(10))
                    .filter(|d| (1..=9).contains(d))
                    .map(|d| FamilyMatch::Digit(d as u8)),
                _ => None,
            },
            PrimaryKey::ArrowLeftRight => match key {
                Key::Named(Named::ArrowLeft) => Some(FamilyMatch::ArrowLeft),
                Key::Named(Named::ArrowRight) => Some(FamilyMatch::ArrowRight),
                _ => None,
            },
            // No keystroke can ever produce a mouse or wheel binding.
            PrimaryKey::Mouse(_) | PrimaryKey::Wheel(_) => None,
        }
    }

    /// Mouse twin of [`HotkeyBinding::match_event`]. Modifier match is
    /// exact for the same reason: `Ctrl+Middle` and a bare middle click
    /// are different bindings, so one must not fire for the other.
    pub fn match_mouse(&self, button: MouseButton, modifiers: &Modifiers) -> bool {
        self.primary == PrimaryKey::Mouse(button)
            && modifiers.control() == self.ctrl
            && modifiers.shift() == self.shift
            && modifiers.alt() == self.alt
            && modifiers.logo() == self.logo
    }

    /// Wheel twin of [`HotkeyBinding::match_mouse`], modifier-exact for
    /// the same reason.
    pub fn match_wheel(&self, direction: WheelDirection, modifiers: &Modifiers) -> bool {
        self.primary == PrimaryKey::Wheel(direction)
            && modifiers.control() == self.ctrl
            && modifiers.shift() == self.shift
            && modifiers.alt() == self.alt
            && modifiers.logo() == self.logo
    }

    /// Whether one keypress can fire both bindings: the same modifiers,
    /// and the same key, or a family and one of its members (`ctrl+1`
    /// is a key of `ctrl+1...9`, `alt+left` one of `alt+←/→`). Equality
    /// alone cannot see that pair, and dispatch would let whichever
    /// comes first in the catalog win in silence.
    pub fn shares_keypress(&self, other: &HotkeyBinding) -> bool {
        if (self.ctrl, self.shift, self.alt, self.logo) != (other.ctrl, other.shift, other.alt, other.logo) {
            return false;
        }
        let member = |family: PrimaryKey, key: PrimaryKey| match (family, key) {
            (PrimaryKey::Digit1to9, PrimaryKey::Char(c)) => ('1'..='9').contains(&c),
            (PrimaryKey::ArrowLeftRight, PrimaryKey::Named(n)) => {
                matches!(n, Named::ArrowLeft | Named::ArrowRight)
            }
            _ => false,
        };
        let same = match (self.primary, other.primary) {
            (PrimaryKey::Char(a), PrimaryKey::Char(b)) => a.eq_ignore_ascii_case(&b),
            (a, b) => a == b,
        };
        same || member(self.primary, other.primary) || member(other.primary, self.primary)
    }

    /// Whether the primary is a mouse button.
    pub fn is_mouse(&self) -> bool {
        matches!(self.primary, PrimaryKey::Mouse(_))
    }

    /// Whether the primary is a wheel direction.
    pub fn is_wheel(&self) -> bool {
        matches!(self.primary, PrimaryKey::Wheel(_))
    }

    /// Whether the binding is valid for the editor: it must carry at
    /// least one of Ctrl / Alt / Logo, otherwise it would silently
    /// intercept the user's typing.
    ///
    /// Shift is not a modifier for this purpose on a primary that
    /// produces text (`Shift+a` is just an uppercase A), but it is on
    /// a primary that never can: `Shift+Insert` (paste) and
    /// `Shift+PageUp` (scrollback) are how every mainstream terminal
    /// spells those chords, and neither steals a keystroke the user
    /// could have typed.
    pub fn is_safe(&self) -> bool {
        // A mouse button never types anything, and the bindable set
        // already excludes the two buttons the terminal owns, so a bare
        // mouse binding steals nothing.
        if self.is_mouse() {
            return true;
        }
        // The wheel is the opposite case: bare, it scrolls, and under
        // Shift alone it is the mouse-report bypass. Only a modifier
        // that means nothing to a scroll makes it a chord.
        if self.is_wheel() {
            return self.ctrl || self.alt || self.logo;
        }
        if self.ctrl || self.alt || self.logo {
            return true;
        }
        // A function key is a chord on its own (F11 = fullscreen), and
        // so is the Menu key (the pane's context menu).
        if self.is_function_key_primary() {
            return true;
        }
        // Otherwise Shift is required, and only on a primary that can't
        // be typed. Modifier-free Insert / Delete / arrows stay
        // unbindable: the PTY wants them, and leaving them out keeps
        // Delete free as the capture editor's "remove this chord" key.
        self.shift && self.is_non_text_primary()
    }

    /// `true` when this binding looks like a sequence the terminal
    /// shell normally consumes itself: Ctrl + printable character with
    /// no other modifier. Examples: Ctrl+L = clear, Ctrl+P = history
    /// prev, Ctrl+K = readline kill, Ctrl+[ = Escape byte. Ctrl+Shift+X
    /// is NOT included because shells don't interpret it as a control
    /// byte. Used by the dispatcher to suppress app-level handling
    /// when the terminal view is focused.
    pub fn is_terminal_control_sequence(&self) -> bool {
        if !self.ctrl || self.alt || self.logo || self.shift {
            return false;
        }
        match self.primary {
            PrimaryKey::Char(c) => c.is_ascii_alphanumeric(),
            // Only the punctuation keys that genuinely produce control
            // bytes via the kernel's tty layer get suppressed. The
            // wider Punct set (`,`, `=`, `-`, `.`, `;`, `/`) doesn't
            // map to anything readline or the shell consumes, so the
            // default bindings on those (OpenSettings, FontZoomIn,
            // FontZoomOut) must continue to fire inside the terminal.
            // The accepted set mirrors the C0 escapes a US/QWERTY shell
            // actually generates: Ctrl+[ = ESC, Ctrl+\ = FS,
            // Ctrl+] = GS.
            PrimaryKey::Punct("[") => true,
            PrimaryKey::Punct("\\") => true,
            PrimaryKey::Punct("]") => true,
            _ => false,
        }
    }

    /// `true` when the primary is F1..F12 or the Menu key. The only
    /// primaries a modifier-free binding may target: none of them types
    /// anything, and the Menu key reaches no program (the encoder sends
    /// it nowhere).
    fn is_function_key_primary(&self) -> bool {
        matches!(
            self.primary,
            PrimaryKey::Named(
                Named::F1
                    | Named::F2
                    | Named::F3
                    | Named::F4
                    | Named::F5
                    | Named::F6
                    | Named::F7
                    | Named::F8
                    | Named::F9
                    | Named::F10
                    | Named::F11
                    | Named::F12
                    | Named::ContextMenu
            )
        )
    }

    /// `true` when the primary can never produce text on its own: the
    /// function keys plus the navigation / editing block. These are the
    /// only primaries where a bare Shift is a real chord rather than
    /// uppercase typing.
    ///
    /// Escape / Enter / Tab / Backspace / Space are deliberately out:
    /// they produce bytes the shell consumes, so binding them (even
    /// with Shift) would eat input the PTY needs.
    fn is_non_text_primary(&self) -> bool {
        matches!(
            self.primary,
            PrimaryKey::Named(
                Named::F1
                    | Named::F2
                    | Named::F3
                    | Named::F4
                    | Named::F5
                    | Named::F6
                    | Named::F7
                    | Named::F8
                    | Named::F9
                    | Named::F10
                    | Named::F11
                    | Named::F12
                    | Named::Insert
                    | Named::Delete
                    | Named::Home
                    | Named::End
                    | Named::PageUp
                    | Named::PageDown
                    | Named::ArrowUp
                    | Named::ArrowDown
                    | Named::ArrowLeft
                    | Named::ArrowRight
            )
        )
    }

    /// Serialize for the settings table: `"ctrl+shift+n"` /
    /// `"alt+arrows"` / `"f11"`. Lowercase, plus-separated, modifiers
    /// in canonical order so a round-trip never reformats.
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push_str("ctrl+");
        }
        if self.shift {
            out.push_str("shift+");
        }
        if self.alt {
            out.push_str("alt+");
        }
        if self.logo {
            out.push_str("logo+");
        }
        match self.primary {
            PrimaryKey::Char(c) => {
                let _ = write!(out, "{}", c.to_ascii_lowercase());
            }
            PrimaryKey::Named(n) => out.push_str(named_to_str(n)),
            PrimaryKey::Punct(p) => out.push_str(p),
            PrimaryKey::Digit1to9 => out.push_str("digit"),
            PrimaryKey::ArrowLeftRight => out.push_str("arrows"),
            PrimaryKey::Mouse(b) => out.push_str(&b.token()),
            PrimaryKey::Wheel(d) => out.push_str(d.token()),
        }
        out
    }

    /// Reverse of `serialize`. Returns `None` for malformed input or
    /// unknown tokens (the caller falls back to the default binding).
    pub fn parse(s: &str) -> Option<Self> {
        let mut ctrl = false;
        let mut shift = false;
        let mut alt = false;
        let mut logo = false;
        // The separator is also a bindable primary, so a trailing `++`
        // (`shift+alt++`) is "modifiers, then the `+` key"; a plain
        // split would leave an empty primary and drop the row.
        let (mods, primary_str) = if let Some(head) = s.strip_suffix("++") {
            (Some(head), "+")
        } else if s == "+" {
            (None, "+")
        } else {
            match s.rsplit_once('+') {
                Some((head, primary)) => (Some(head), primary),
                None => (None, s),
            }
        };
        for m in mods.into_iter().flat_map(|h| h.split('+')) {
            match m {
                "ctrl" => ctrl = true,
                "shift" => shift = true,
                "alt" => alt = true,
                "logo" => logo = true,
                _ => return None,
            }
        }
        let primary = match primary_str {
            "digit" => PrimaryKey::Digit1to9,
            "arrows" => PrimaryKey::ArrowLeftRight,
            "," | "." | ";" | "=" | "-" | "+" | "/" | "\\" | "[" | "]" => {
                // Static slice lookup keeps the &'static str alive.
                match primary_str {
                    "," => PrimaryKey::Punct(","),
                    "." => PrimaryKey::Punct("."),
                    ";" => PrimaryKey::Punct(";"),
                    "=" => PrimaryKey::Punct("="),
                    "-" => PrimaryKey::Punct("-"),
                    "+" => PrimaryKey::Punct("+"),
                    "/" => PrimaryKey::Punct("/"),
                    "\\" => PrimaryKey::Punct("\\"),
                    "[" => PrimaryKey::Punct("["),
                    "]" => PrimaryKey::Punct("]"),
                    _ => unreachable!(),
                }
            }
            other => {
                // Mouse tokens first: they are `mouse_`-prefixed, so
                // they can't shadow a named key or a single char, and
                // checking them here keeps the fallback chain honest.
                if let Some(button) = MouseButton::parse_token(other) {
                    PrimaryKey::Mouse(button)
                } else if let Some(direction) = WheelDirection::parse_token(other) {
                    PrimaryKey::Wheel(direction)
                } else if let Some(named) = str_to_named(other) {
                    PrimaryKey::Named(named)
                } else if other.len() == 1
                    && other
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphanumeric())
                {
                    // Digit chars (0..=9) round-trip as Char; the
                    // `digit` family token is reserved for the 1..9
                    // suffix variant of `SwitchToTabSlot`.
                    PrimaryKey::Char(other.chars().next().unwrap().to_ascii_lowercase())
                } else {
                    return None;
                }
            }
        };
        Some(HotkeyBinding {
            ctrl,
            shift,
            alt,
            logo,
            primary,
        })
    }

    /// Returns the user-facing badges for the binding (e.g.
    /// `["Ctrl", "Shift", "N"]`). Family suffixes render as their
    /// fixed glyph token (`"1...9"`, `"←/→"`).
    pub fn badges(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.ctrl {
            out.push("Ctrl".into());
        }
        if self.shift {
            out.push("Shift".into());
        }
        if self.alt {
            out.push("Alt".into());
        }
        if self.logo {
            // The key's own name on the platform the binary was built
            // for: a chord chip reading `Super+Shift+C` on a Mac names
            // a key its keyboard does not have.
            out.push(if cfg!(target_os = "macos") { "Cmd" } else { "Super" }.into());
        }
        let primary = match self.primary {
            PrimaryKey::Char(c) => c.to_ascii_uppercase().to_string(),
            PrimaryKey::Named(n) => named_to_str(n).to_uppercase(),
            PrimaryKey::Punct(p) => p.to_string(),
            PrimaryKey::Digit1to9 => "1...9".into(),
            PrimaryKey::ArrowLeftRight => "←/→".into(),
            PrimaryKey::Mouse(b) => b.label(),
            PrimaryKey::Wheel(d) => d.label(),
        };
        out.push(primary);
        out
    }
}

fn named_to_str(n: Named) -> &'static str {
    match n {
        Named::Escape => "esc",
        Named::Enter => "enter",
        Named::Tab => "tab",
        Named::Backspace => "backspace",
        Named::Delete => "del",
        Named::Insert => "ins",
        Named::Home => "home",
        Named::End => "end",
        Named::PageUp => "pgup",
        Named::PageDown => "pgdn",
        Named::ArrowUp => "up",
        Named::ArrowDown => "down",
        Named::ArrowLeft => "left",
        Named::ArrowRight => "right",
        Named::Space => "space",
        Named::F1 => "f1",
        Named::F2 => "f2",
        Named::F3 => "f3",
        Named::F4 => "f4",
        Named::F5 => "f5",
        Named::F6 => "f6",
        Named::F7 => "f7",
        Named::F8 => "f8",
        Named::F9 => "f9",
        Named::F10 => "f10",
        Named::F11 => "f11",
        Named::F12 => "f12",
        Named::ContextMenu => "menu",
        _ => "?",
    }
}

fn str_to_named(s: &str) -> Option<Named> {
    Some(match s {
        "esc" => Named::Escape,
        "enter" => Named::Enter,
        "tab" => Named::Tab,
        "backspace" => Named::Backspace,
        "del" => Named::Delete,
        "ins" => Named::Insert,
        "home" => Named::Home,
        "end" => Named::End,
        "pgup" => Named::PageUp,
        "pgdn" => Named::PageDown,
        "up" => Named::ArrowUp,
        "down" => Named::ArrowDown,
        "left" => Named::ArrowLeft,
        "right" => Named::ArrowRight,
        "space" => Named::Space,
        "f1" => Named::F1,
        "f2" => Named::F2,
        "f3" => Named::F3,
        "f4" => Named::F4,
        "f5" => Named::F5,
        "f6" => Named::F6,
        "f7" => Named::F7,
        "f8" => Named::F8,
        "f9" => Named::F9,
        "f10" => Named::F10,
        "f11" => Named::F11,
        "f12" => Named::F12,
        "menu" => Named::ContextMenu,
        _ => return None,
    })
}

/// A bare mouse chord: this button, no modifiers.
///
/// The factory table and the tests both need it, and spelling the
/// struct out at each site is how a field added later goes missing in
/// one of them.
impl HotkeyBinding {
    pub fn mouse(button: MouseButton) -> Self {
        Self { ctrl: false, shift: false, alt: false, logo: false, primary: PrimaryKey::Mouse(button) }
    }
}

/// Builds a `HotkeyBinding` from a captured iced KeyPressed event,
/// or `None` if the event can't be turned into a safe binding (no
/// modifier and not a function key). Used by capture mode in the
/// Settings → Shortcuts editor.
pub fn binding_from_event(
    key: &Key,
    modifiers: &Modifiers,
    primary_editable: bool,
) -> Option<HotkeyBinding> {
    // For family bindings (modifier-only edit) we ignore the primary
    // and just take the modifier set; the caller substitutes the
    // existing primary back in. The editor passes `primary_editable
    // = false` for those.
    let primary_opt: Option<PrimaryKey> = if primary_editable {
        match key {
            Key::Character(s) => {
                let txt = s.as_str();
                if txt.len() == 1 {
                    let ch = txt.chars().next().unwrap();
                    if ch.is_ascii_alphanumeric() {
                        Some(PrimaryKey::Char(ch.to_ascii_lowercase()))
                    } else {
                        // Single source of truth for the punctuation
                        // accept-list: the match returning Some(s) IS
                        // both the membership check and the
                        // &'static str mapping. Adding a new punct
                        // means one new arm, not two synced lists.
                        match ch {
                            ',' => Some(PrimaryKey::Punct(",")),
                            '.' => Some(PrimaryKey::Punct(".")),
                            ';' => Some(PrimaryKey::Punct(";")),
                            '=' => Some(PrimaryKey::Punct("=")),
                            '-' => Some(PrimaryKey::Punct("-")),
                            '+' => Some(PrimaryKey::Punct("+")),
                            '/' => Some(PrimaryKey::Punct("/")),
                            '\\' => Some(PrimaryKey::Punct("\\")),
                            '[' => Some(PrimaryKey::Punct("[")),
                            ']' => Some(PrimaryKey::Punct("]")),
                            _ => None,
                        }
                    }
                } else {
                    None
                }
            }
            Key::Named(n) => Some(PrimaryKey::Named(*n)),
            _ => None,
        }
    } else {
        None
    };

    if primary_editable {
        // Without a recognised primary there is nothing to bind. The
        // old fallback to `PrimaryKey::Char('?')` produced a row that
        // passed `is_safe()` but no real key event ever reproduced,
        // so the binding was silently dead. Returning `None` here
        // keeps the capture in "press a key" state.
        let primary = primary_opt?;
        let binding = HotkeyBinding {
            ctrl: modifiers.control(),
            shift: modifiers.shift(),
            alt: modifiers.alt(),
            logo: modifiers.logo(),
            primary,
        };
        if !binding.is_safe() {
            return None;
        }
        Some(binding)
    } else {
        // Family captures keep the existing primary (a digit, an
        // arrow, etc.) and only swap the modifiers. The user must
        // still pick at least one of Ctrl / Alt / Logo, otherwise
        // any bare digit press would hijack tab switching. The
        // primary isn't read from the event here, so a missing
        // `primary_opt` is fine, fall back to a placeholder that
        // the caller's existing `family` field overrides.
        let binding = HotkeyBinding {
            ctrl: modifiers.control(),
            shift: modifiers.shift(),
            alt: modifiers.alt(),
            logo: modifiers.logo(),
            primary: primary_opt.unwrap_or(PrimaryKey::Digit1to9),
        };
        if !binding.ctrl && !binding.alt && !binding.logo {
            return None;
        }
        Some(binding)
    }
}

/// The bare middle-click chord: `TerminalPasteSelection`'s second
/// factory input, and exactly what Settings > Terminal's "middle-click
/// paste" toggle adds to / removes from the binding table.
pub fn middle_click_chord() -> HotkeyBinding {
    HotkeyBinding {
        ctrl: false,
        shift: false,
        alt: false,
        logo: false,
        primary: PrimaryKey::Mouse(MouseButton::Middle),
    }
}

/// The Ctrl+wheel chord in one direction: the second factory input of
/// `FontZoomIn` (up) and `FontZoomOut` (down), and exactly what
/// Settings > Terminal's "zoom with Ctrl + wheel" toggle adds to / removes
/// from the binding table.
///
/// Ctrl on every platform, macOS included: the key chords swap Ctrl for
/// Cmd there, but this one is what Windows synthesizes from a
/// precision-touchpad pinch and what every Linux terminal binds, and
/// the macOS pinch never reaches the app as a wheel at all.
pub fn wheel_zoom_chord(direction: WheelDirection) -> HotkeyBinding {
    HotkeyBinding {
        ctrl: true,
        shift: false,
        alt: false,
        logo: false,
        primary: PrimaryKey::Wheel(direction),
    }
}

/// Wheel twin of [`binding_from_mouse`]: turns a captured wheel notch
/// into a binding, or `None` when the notch carries no modifier that
/// makes it a chord (a bare wheel scrolls the page the capture sits on,
/// and must go on doing so).
pub fn binding_from_wheel(direction: WheelDirection, modifiers: &Modifiers) -> Option<HotkeyBinding> {
    let binding = HotkeyBinding {
        ctrl: modifiers.control(),
        shift: modifiers.shift(),
        alt: modifiers.alt(),
        logo: modifiers.logo(),
        primary: PrimaryKey::Wheel(direction),
    };
    binding.is_safe().then_some(binding)
}

/// Mouse twin of [`binding_from_event`]: turns a captured button press
/// into a binding, or `None` when the button isn't bindable (Left and
/// Right, which the terminal canvas keeps).
///
/// No `is_safe` check is needed: every button that survives
/// `MouseButton::from_iced` is safe by construction (see
/// [`HotkeyBinding::is_safe`]).
pub fn binding_from_mouse(button: iced::mouse::Button, modifiers: &Modifiers) -> Option<HotkeyBinding> {
    Some(HotkeyBinding {
        ctrl: modifiers.control(),
        shift: modifiers.shift(),
        alt: modifiers.alt(),
        logo: modifiers.logo(),
        primary: PrimaryKey::Mouse(MouseButton::from_iced(button)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkeys::{default_bindings, HotkeyAction, HotkeyBindings};

    /// The chords #73 asked for. `Shift+Insert` (paste) and
    /// `Shift+PageUp` (scrollback) carry no Ctrl/Alt/Logo, so they only
    /// clear `is_safe` because Shift counts on a primary that can't be
    /// typed. If that regresses, the capture editor silently rejects the
    /// chord every other terminal ships.
    #[test]
    fn shift_plus_non_text_key_is_bindable() {
        let shift_ins = HotkeyBinding {
            ctrl: false,
            shift: true,
            alt: false,
            logo: false,
            primary: PrimaryKey::Named(Named::Insert),
        };
        assert!(shift_ins.is_safe());
        assert_eq!(shift_ins.serialize(), "shift+ins");
        assert_eq!(HotkeyBinding::parse("shift+ins"), Some(shift_ins));
        // Never suppressed as a shell control sequence (no Ctrl).
        assert!(!shift_ins.is_terminal_control_sequence());

        let shift_pgup = HotkeyBinding {
            primary: PrimaryKey::Named(Named::PageUp),
            ..shift_ins
        };
        assert!(shift_pgup.is_safe());

        // Shift alone on a typable primary is still just uppercase.
        let shift_a = HotkeyBinding {
            primary: PrimaryKey::Char('a'),
            ..shift_ins
        };
        assert!(!shift_a.is_safe());
        // Modifier-free navigation keys stay unbindable: the PTY wants
        // them, and bare Delete is the capture editor's remove gesture.
        let bare_del = HotkeyBinding {
            shift: false,
            primary: PrimaryKey::Named(Named::Delete),
            ..shift_ins
        };
        assert!(!bare_del.is_safe());
    }

    #[test]
    fn family_match_extracts_digit() {
        let b = HotkeyBinding {
            ctrl: true,
            shift: false,
            alt: false,
            logo: false,
            primary: PrimaryKey::Digit1to9,
        };
        let mods = Modifiers::CTRL;
        let key = Key::Character("3".into());
        assert_eq!(b.match_event(&key, &mods), Some(FamilyMatch::Digit(3)));
        let bad = Key::Character("0".into());
        assert_eq!(b.match_event(&bad, &mods), None);
    }

    #[test]
    fn family_match_extracts_arrow() {
        let b = HotkeyBinding {
            ctrl: false,
            shift: false,
            alt: true,
            logo: false,
            primary: PrimaryKey::ArrowLeftRight,
        };
        let mods = Modifiers::ALT;
        assert_eq!(
            b.match_event(&Key::Named(Named::ArrowRight), &mods),
            Some(FamilyMatch::ArrowRight),
        );
        assert_eq!(
            b.match_event(&Key::Named(Named::ArrowLeft), &mods),
            Some(FamilyMatch::ArrowLeft),
        );
        assert_eq!(b.match_event(&Key::Named(Named::ArrowUp), &mods), None);
    }

    #[test]
    fn shift_diff_blocks_match() {
        // Ctrl+K binding should NOT fire on Ctrl+Shift+K, the editor
        // exact-matches modifiers so the two combos can be bound to
        // different actions.
        let b = HotkeyBinding {
            ctrl: true,
            shift: false,
            alt: false,
            logo: false,
            primary: PrimaryKey::Char('k'),
        };
        assert_eq!(
            b.match_event(&Key::Character("k".into()), &(Modifiers::CTRL | Modifiers::SHIFT)),
            None
        );
        assert_eq!(
            b.match_event(&Key::Character("k".into()), &Modifiers::CTRL),
            Some(FamilyMatch::Plain)
        );
    }

    /// `+` is both the chord separator and a bindable primary, so a
    /// chord ending in it must still round-trip (Alt+Shift+Plus is the
    /// side-by-side split off macOS).
    #[test]
    fn plus_primary_round_trips() {
        for (ctrl, shift, alt) in [(false, false, false), (true, false, false), (false, true, true)] {
            let b = HotkeyBinding { ctrl, shift, alt, logo: false, primary: PrimaryKey::Punct("+") };
            let s = b.serialize();
            assert_eq!(HotkeyBinding::parse(&s), Some(b), "{s}");
        }
        assert_eq!(HotkeyBinding::parse("ctrl+"), None);
        assert_eq!(HotkeyBinding::parse("ctrl+shift+n").map(|b| b.primary), Some(PrimaryKey::Char('n')));
    }

    #[test]
    fn punct_keys_are_not_terminal_control_unless_c0() {
        // Regression: Ctrl+, / Ctrl+= / Ctrl+- used to be silently
        // suppressed inside the terminal view because the gate
        // accepted every Punct. They map to no control byte; the
        // default bindings (OpenSettings, FontZoomIn, FontZoomOut)
        // must fire even when the focus is on the embedded terminal.
        for &p in &[",", "=", "-", ".", ";", "/"] {
            let b = HotkeyBinding {
                ctrl: true,
                shift: false,
                alt: false,
                logo: false,
                primary: PrimaryKey::Punct(p),
            };
            assert!(
                !b.is_terminal_control_sequence(),
                "Ctrl+{p} should not be a terminal control sequence"
            );
        }
    }

    #[test]
    fn punct_keys_that_map_to_c0_are_terminal_control() {
        // Ctrl+[ = ESC, Ctrl+\ = FS, Ctrl+] = GS are real C0 escapes
        // a shell consumes via the tty layer, so the dispatcher should
        // continue to suppress them inside the terminal view.
        for &p in &["[", "\\", "]"] {
            let b = HotkeyBinding {
                ctrl: true,
                shift: false,
                alt: false,
                logo: false,
                primary: PrimaryKey::Punct(p),
            };
            assert!(
                b.is_terminal_control_sequence(),
                "Ctrl+{p} should be a terminal control sequence"
            );
        }
    }

    #[test]
    fn safe_requires_modifier_or_function_key() {
        let unsafe_binding = HotkeyBinding {
            ctrl: false,
            shift: false,
            alt: false,
            logo: false,
            primary: PrimaryKey::Char('a'),
        };
        assert!(!unsafe_binding.is_safe());

        let f_key = HotkeyBinding {
            ctrl: false,
            shift: false,
            alt: false,
            logo: false,
            primary: PrimaryKey::Named(Named::F11),
        };
        assert!(f_key.is_safe());

        let ctrl_a = HotkeyBinding {
            ctrl: true,
            shift: false,
            alt: false,
            logo: false,
            primary: PrimaryKey::Char('a'),
        };
        assert!(ctrl_a.is_safe());
    }

    /// Does the Insert chord actually FIRE? `is_safe` / `parse` passing
    /// only proves the editor would accept the chord, not that a real
    /// key event reaches the action. Since the Shift+Insert move to
    /// PasteSelection this also pins that plain paste does NOT match it,
    /// or one keystroke would fire both actions.
    #[test]
    fn insert_chords_match_a_real_key_event() {
        let defaults = default_bindings();
        let ins = Key::Named(Named::Insert);

        let mut m = Modifiers::default();
        m.set(Modifiers::SHIFT, true);
        let paste_sel = defaults
            .get(&HotkeyAction::TerminalPasteSelection)
            .expect("bound");
        assert_eq!(
            paste_sel.match_event(&ins, &m),
            Some(FamilyMatch::Plain),
            "Shift+Insert must fire the PRIMARY paste"
        );
        let paste = defaults.get(&HotkeyAction::TerminalPaste).expect("bound");
        assert_eq!(
            paste.match_event(&ins, &m),
            None,
            "plain paste must not shadow the PasteSelection chord"
        );

        // Modifier match is exact: a bare Insert fires nothing.
        let none = Modifiers::default();
        assert_eq!(paste_sel.match_event(&ins, &none), None);
    }

    /// Every bindable button round-trips through the settings table,
    /// modifiers included. A regression here silently drops a user's
    /// mouse binding on the next boot.
    #[test]
    fn mouse_bindings_round_trip() {
        for button in [
            MouseButton::Middle,
            MouseButton::Back,
            MouseButton::Forward,
            MouseButton::Other(8),
            MouseButton::Other(0),
        ] {
            let bare = HotkeyBinding::mouse(button);
            let s = bare.serialize();
            assert!(!s.contains(char::is_whitespace), "{s:?} would split the list");
            assert_eq!(HotkeyBinding::parse(&s), Some(bare), "bare {button:?}");

            let modded = HotkeyBinding { ctrl: true, shift: true, ..bare };
            let s = modded.serialize();
            assert_eq!(HotkeyBinding::parse(&s), Some(modded), "modified {button:?}");
        }
        // And as part of a multi-chord row, which is how paste-selection
        // actually stores it.
        let row = HotkeyBindings::many([
            HotkeyBinding {
                ctrl: false,
                shift: true,
                alt: false,
                logo: false,
                primary: PrimaryKey::Named(Named::Insert),
            },
            middle_click_chord(),
        ]);
        assert_eq!(row.serialize(), "shift+ins mouse_middle");
        assert_eq!(HotkeyBindings::parse("shift+ins mouse_middle"), Some(row));
    }

    /// The `mouse_` prefix is the whole reason `parse` can try buttons
    /// first. If a named key or punctuation token ever collided, one of
    /// the two would become unparseable.
    #[test]
    fn mouse_tokens_never_collide_with_key_primaries() {
        for token in ["mouse_middle", "mouse_back", "mouse_forward", "mouse_8"] {
            assert!(str_to_named(token).is_none(), "{token} shadows a named key");
            assert!(
                matches!(
                    HotkeyBinding::parse(token).map(|b| b.primary),
                    Some(PrimaryKey::Mouse(_))
                ),
                "{token} must parse as a mouse button"
            );
        }
        // Not every `mouse_*` string is a button: an unknown suffix is
        // malformed, not a silent Other(0).
        assert_eq!(MouseButton::parse_token("mouse_wat"), None);
        assert_eq!(MouseButton::parse_token("middle"), None);
    }

    /// Left and Right stay with the terminal canvas (select / the
    /// right-click scheme), so neither can ever become a binding.
    #[test]
    fn left_and_right_are_never_bindable() {
        let none = Modifiers::default();
        for button in [iced::mouse::Button::Left, iced::mouse::Button::Right] {
            assert_eq!(MouseButton::from_iced(button), None, "{button:?}");
            assert_eq!(binding_from_mouse(button, &none), None, "{button:?}");
        }
        assert_eq!(
            binding_from_mouse(iced::mouse::Button::Middle, &none),
            Some(middle_click_chord())
        );
    }

    /// A bare mouse binding needs no modifier (it can't be typed), and
    /// modifier matching is exact in both directions.
    #[test]
    fn mouse_matching_is_modifier_exact() {
        let bare = HotkeyBinding::mouse(MouseButton::Middle);
        assert!(bare.is_safe(), "a mouse button is a chord on its own");
        // Never suppressed as a shell control sequence, whatever the
        // modifiers: the PTY has no byte for a mouse binding.
        assert!(!bare.is_terminal_control_sequence());
        assert!(!HotkeyBinding { ctrl: true, ..bare }.is_terminal_control_sequence());

        let none = Modifiers::default();
        let mut ctrl = Modifiers::default();
        ctrl.set(Modifiers::CTRL, true);

        assert!(bare.match_mouse(MouseButton::Middle, &none));
        assert!(!bare.match_mouse(MouseButton::Middle, &ctrl), "Ctrl+Middle is a different binding");
        assert!(!bare.match_mouse(MouseButton::Back, &none), "wrong button");

        let ctrl_middle = HotkeyBinding { ctrl: true, ..bare };
        assert!(ctrl_middle.match_mouse(MouseButton::Middle, &ctrl));
        assert!(!ctrl_middle.match_mouse(MouseButton::Middle, &none));
    }

    /// The two halves never cross: no keystroke fires a mouse binding,
    /// and no button fires a chord.
    #[test]
    fn mouse_and_keyboard_bindings_never_cross() {
        let none = Modifiers::default();
        let bare = HotkeyBindings::single(HotkeyBinding::mouse(MouseButton::Middle));
        for key in [Key::Named(Named::Insert), Key::Character("v".into())] {
            assert_eq!(bare.match_event(&key, &none), None, "{key:?}");
        }
        let defaults = default_bindings();
        let copy = defaults.get(&HotkeyAction::TerminalCopy).expect("bound");
        assert!(!copy.match_mouse(MouseButton::Middle, &none));
    }

    /// A wheel chord round-trips through the settings table under its
    /// own prefix, and is only a chord under a modifier a scroll does
    /// not already mean something by: bare and Shift-only are refused,
    /// because one scrolls and the other is the mouse-report bypass.
    #[test]
    fn wheel_chords_need_a_real_modifier_and_round_trip() {
        let up = wheel_zoom_chord(WheelDirection::Up);
        assert!(up.is_wheel());
        assert!(!up.is_mouse());
        assert!(up.is_safe());
        assert!(!up.is_terminal_control_sequence());
        assert_eq!(up.serialize(), "ctrl+wheel_up");
        assert_eq!(HotkeyBinding::parse("ctrl+wheel_up"), Some(up));
        let down = wheel_zoom_chord(WheelDirection::Down);
        assert_eq!(down.serialize(), "ctrl+wheel_down");
        assert_eq!(HotkeyBinding::parse("ctrl+wheel_down"), Some(down));
        assert_eq!(HotkeyBinding::parse("alt+wheel_up").map(|b| b.alt), Some(true));

        let bare = HotkeyBinding { ctrl: false, ..up };
        assert!(!bare.is_safe(), "a bare wheel scrolls");
        let shift = HotkeyBinding { ctrl: false, shift: true, ..up };
        assert!(!shift.is_safe(), "Shift+wheel is the report bypass");
        assert!(HotkeyBinding { alt: true, ctrl: false, ..up }.is_safe());
        assert!(HotkeyBinding { logo: true, ctrl: false, ..up }.is_safe());

        // The capture helper applies the same rule, and answers None
        // rather than a toast-worthy error, so a bare wheel keeps
        // scrolling the page the capture sits on.
        let mut ctrl = Modifiers::default();
        ctrl.set(Modifiers::CTRL, true);
        assert_eq!(binding_from_wheel(WheelDirection::Up, &ctrl), Some(up));
        assert_eq!(binding_from_wheel(WheelDirection::Up, &Modifiers::default()), None);
        let mut shift_only = Modifiers::default();
        shift_only.set(Modifiers::SHIFT, true);
        assert_eq!(binding_from_wheel(WheelDirection::Down, &shift_only), None);
    }

    /// Wheel matching is modifier-exact and direction-exact, and never
    /// crosses with keys or buttons.
    #[test]
    fn wheel_matching_is_exact_and_never_crosses() {
        let up = wheel_zoom_chord(WheelDirection::Up);
        let none = Modifiers::default();
        let mut ctrl = Modifiers::default();
        ctrl.set(Modifiers::CTRL, true);
        let mut ctrl_shift = ctrl;
        ctrl_shift.set(Modifiers::SHIFT, true);

        assert!(up.match_wheel(WheelDirection::Up, &ctrl));
        assert!(!up.match_wheel(WheelDirection::Down, &ctrl), "wrong direction");
        assert!(!up.match_wheel(WheelDirection::Up, &none), "Ctrl is part of the chord");
        assert!(
            !up.match_wheel(WheelDirection::Up, &ctrl_shift),
            "Ctrl+Shift+wheel is another binding"
        );
        assert!(!up.match_mouse(MouseButton::Middle, &ctrl));
        assert_eq!(up.match_event(&Key::Character("=".into()), &ctrl), None);

        assert_eq!(WheelDirection::from_delta_y(0.25), Some(WheelDirection::Up));
        assert_eq!(WheelDirection::from_delta_y(-3.0), Some(WheelDirection::Down));
        assert_eq!(WheelDirection::from_delta_y(0.0), None, "a tilt names no direction");

        let defaults = default_bindings();
        let zoom_in = defaults.get(&HotkeyAction::FontZoomIn).expect("bound");
        assert!(zoom_in.match_wheel(WheelDirection::Up, &ctrl));
        assert!(!zoom_in.match_wheel(WheelDirection::Down, &ctrl));
        let zoom_out = defaults.get(&HotkeyAction::FontZoomOut).expect("bound");
        assert!(zoom_out.match_wheel(WheelDirection::Down, &ctrl));
    }
}
