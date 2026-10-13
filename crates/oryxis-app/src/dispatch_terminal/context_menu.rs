//! Where the pane's context menu opens when a KEY asks for it.
//!
//! A right-click carries its own point. The Menu key carries none, and the
//! mouse may be resting anywhere (another pane, another monitor), so the
//! menu opens where the keyboard user is looking: under the text cursor,
//! the spot the IME candidate window is anchored to.

use uuid::Uuid;

use super::*;

impl Oryxis {
    /// Window-absolute point under `pane_id`'s text cursor, kept inside the
    /// pane's drawn rect. `None` while the pane has not been drawn yet (no
    /// rect to anchor to); the caller falls back to the mouse.
    ///
    /// The cursor cell ignores a scrolled-back viewport (see
    /// `TerminalState::cursor_cell`), so with the view in history the
    /// caret can sit below the pane; the clamp keeps the menu on the pane
    /// the key was pressed over instead of off its bottom edge.
    pub(crate) fn terminal_cursor_menu_anchor(&self, pane_id: Uuid) -> Option<(f32, f32)> {
        let pane = self.pane_by_id(pane_id)?;
        let bounds = pane.bounds.get();
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return None;
        }
        let cell = pane.terminal.lock().ok()?.cursor_cell();
        let caret = oryxis_terminal::ime_caret_rect(
            bounds,
            self.terminal_font_px(),
            Some(self.terminal_font_name.as_str()),
            self.terminal_font_weight.font_weight(),
            cell,
        );
        Some(clamp_to(bounds, caret.x, caret.y + caret.height))
    }
}

/// `(x, y)` pulled inside `bounds`.
fn clamp_to(bounds: iced::Rectangle, x: f32, y: f32) -> (f32, f32) {
    (
        x.clamp(bounds.x, bounds.x + bounds.width),
        y.clamp(bounds.y, bounds.y + bounds.height),
    )
}

#[cfg(test)]
mod tests {
    use super::clamp_to;

    #[test]
    fn a_caret_below_the_pane_anchors_on_its_bottom_edge() {
        let pane = iced::Rectangle { x: 10.0, y: 40.0, width: 600.0, height: 300.0 };
        assert_eq!(clamp_to(pane, 120.0, 900.0), (120.0, 340.0));
    }

    #[test]
    fn a_caret_inside_the_pane_stays_where_it_is() {
        let pane = iced::Rectangle { x: 10.0, y: 40.0, width: 600.0, height: 300.0 };
        assert_eq!(clamp_to(pane, 120.0, 90.0), (120.0, 90.0));
    }
}
