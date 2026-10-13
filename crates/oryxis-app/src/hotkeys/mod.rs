//! Editable keyboard binding model.
//!
//! Each `HotkeyAction` is something the user can trigger from the
//! keyboard (open settings, switch tab, close active tab, ...). A
//! `HotkeyBinding` pairs a modifier set with a `PrimaryKey`; the
//! `match_event` helper turns an incoming iced KeyPressed into an
//! optional `FamilyMatch` which the dispatcher inspects to build the
//! final `Message`.
//!
//! Families (`Digit1to9`, `ArrowLeftRight`) are bindings where the
//! suffix isn't editable, mirroring Termius's "Ctrl + [1...9]" row.
//! Only their modifier set can change.

//! The model is split three ways, re-exported here so callers keep
//! importing `crate::hotkeys::*` exactly as before:
//!
//! - [`action`]: the catalog of bindable actions and their rules.
//! - [`binding`]: keys, mouse buttons, modifiers, parsing, matching.
//! - [`defaults`]: the factory table and the per-action binding list.
//! - [`layout`]: a whole set at once (presets, the shortcuts file).
//! - [`presets`]: other terminals' keymaps, measured.

mod action;
mod binding;
mod defaults;
pub mod layout;
mod presets;

pub use action::{HotkeyAction, MouseBindingOwner};
pub use binding::{
    binding_from_event, binding_from_mouse, binding_from_wheel, middle_click_chord,
    wheel_zoom_chord, FamilyMatch, HotkeyBinding, MouseButton, PrimaryKey, WheelDirection,
};
pub use defaults::{default_bindings, HotkeyBindings, HotkeyMap, HotkeySlot};
pub use presets::ShortcutPreset;
