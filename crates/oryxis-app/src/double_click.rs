//! Double-click detection for row lists (the dual-pane SFTP browser and
//! the sidebar Files tab), on the operating system's own terms.
//!
//! A native list (FileZilla's `wxListCtrl`, Explorer, Finder) never times
//! clicks itself: the toolkit reports a double click when the second
//! PRESS lands within the system double-click interval of the first and
//! within a few pixels of it, on the same item. Our rows are iced buttons,
//! which publish on RELEASE, so the two halves are split: the press is
//! judged here (`ClickTracker::press`, fed by the row's press reporter
//! with the press position) and the row's own release message carries the
//! verdict out. Judging the release instead, as the panes once did with a
//! private 500 ms clock, counted the time the button was held and ignored
//! how far the cursor had moved, which is what made a double click land
//! or miss depending on the hand.
//!
//! The interval is the system's where the system has one: Windows reads
//! `GetDoubleClickTime` (the Control Panel slider, 500 ms by default) and
//! the `SM_CXDOUBLECLK` / `SM_CYDOUBLECLK` box; macOS reads
//! `NSEvent.doubleClickInterval`. Linux has no toolkit to ask (the app
//! draws its own widgets), so it takes the GTK and KDE default of 400 ms.
//! The distance box outside Windows is 5 px, GTK's
//! `gtk-double-click-distance`; a CHOSEN value, not a measured one.

use std::time::{Duration, Instant};

use iced::Point;

/// The system's double-click rule: how soon and how close the second
/// press has to be.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DoubleClickRule {
    /// Longest gap between the two presses.
    pub interval: Duration,
    /// Half the width and height of the box, centred on the first press,
    /// the second press has to land in.
    pub half_box: (f32, f32),
}

impl DoubleClickRule {
    /// The rule the platform's own lists use. Read per press: it is a
    /// cheap call everywhere, and the user can move the slider while the
    /// app runs.
    pub fn os() -> DoubleClickRule {
        DoubleClickRule {
            interval: os_interval(),
            half_box: os_half_box(),
        }
    }
}

/// Bounds any platform answer is clamped to. Windows documents its
/// slider as 100..5000 ms; a broken or zero reading outside that lands on
/// the documented default instead of making every click a double one.
const MIN_INTERVAL: Duration = Duration::from_millis(100);
const MAX_INTERVAL: Duration = Duration::from_millis(5_000);
const DEFAULT_INTERVAL: Duration = Duration::from_millis(500);

fn clamp_interval(interval: Option<Duration>) -> Duration {
    match interval {
        Some(d) if d >= MIN_INTERVAL && d <= MAX_INTERVAL => d,
        _ => DEFAULT_INTERVAL,
    }
}

#[cfg(windows)]
fn os_interval() -> Duration {
    // Never fails per the docs; the clamp covers a value edited into the
    // registry past the slider's range.
    let ms = unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() };
    clamp_interval(Some(Duration::from_millis(u64::from(ms))))
}

#[cfg(target_os = "macos")]
fn os_interval() -> Duration {
    // Seconds, from the Mouse / Trackpad "Double-click speed" slider.
    let secs = objc2_app_kit::NSEvent::doubleClickInterval();
    let interval = (secs.is_finite() && secs > 0.0).then(|| Duration::from_secs_f64(secs));
    clamp_interval(interval)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn os_interval() -> Duration {
    // GTK's `gtk-double-click-time` and KDE's `DoubleClickInterval`
    // default. Nothing reads the running desktop's value: the app has no
    // GTK or Qt in it to ask, and the X resource / gsettings routes each
    // cover one desktop only.
    clamp_interval(Some(Duration::from_millis(400)))
}

#[cfg(windows)]
fn os_half_box() -> (f32, f32) {
    // The metrics are the FULL width and height of the box, centred on
    // the first click (4 px by default), in system-DPI pixels; iced
    // reports logical positions, so a scaled display gets a box that is
    // a little tighter than Explorer's.
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXDOUBLECLK, SM_CYDOUBLECLK,
    };
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXDOUBLECLK), GetSystemMetrics(SM_CYDOUBLECLK)) };
    let half = |v: i32| if v > 0 { v as f32 / 2.0 } else { 2.0 };
    (half(w), half(h))
}

#[cfg(not(windows))]
fn os_half_box() -> (f32, f32) {
    // GTK's `gtk-double-click-distance` default; macOS documents no
    // figure for its own threshold.
    (5.0, 5.0)
}

/// What a press turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClickReport {
    /// The second press of a double click on the same row.
    pub double: bool,
    /// Time since the previous press, when that press was on the SAME
    /// row and this one is not its double. The slow second click that
    /// arms an inline rename reads it: a gap under the interval but
    /// outside the box is neither a double click nor a rename.
    pub gap: Option<Duration>,
}

#[derive(Debug, Clone)]
struct Press<K> {
    key: K,
    at: Point,
    time: Instant,
}

/// Judges each left press against the previous one. One per list, keyed
/// by whatever names a row in that list (a path, or the `..` row).
#[derive(Debug, Clone)]
pub(crate) struct ClickTracker<K> {
    last: Option<Press<K>>,
}

impl<K> Default for ClickTracker<K> {
    fn default() -> Self {
        ClickTracker { last: None }
    }
}

impl<K: PartialEq + Clone> ClickTracker<K> {
    /// Record a plain left press on `key` at `at` and say what it was.
    /// A double click consumes both presses: the press after it starts a
    /// fresh count, the way a native list treats a third click as the
    /// first of a new pair.
    pub fn press(&mut self, key: K, at: Point, now: Instant, rule: &DoubleClickRule) -> ClickReport {
        let previous = self.last.take();
        let same = previous.as_ref().filter(|p| p.key == key);
        let gap = same.map(|p| now.saturating_duration_since(p.time));
        let double = same.is_some_and(|p| {
            // `>=`: a press stamped in the same instant as the previous
            // one (the harness replays both halves of a double click
            // without a frame between) is as consecutive as it gets. A
            // clock that went backwards is no double click.
            now >= p.time
                && gap.is_some_and(|g| g <= rule.interval)
                && (at.x - p.at.x).abs() <= rule.half_box.0
                && (at.y - p.at.y).abs() <= rule.half_box.1
        });
        if double {
            return ClickReport { double: true, gap: None };
        }
        self.last = Some(Press { key, at, time: now });
        ClickReport { double: false, gap }
    }

    /// Forget the previous press: a press that is selection-building
    /// (Ctrl / Shift) is never half of a double click and must not leave
    /// a stamp that turns the NEXT plain press into one, and a listing
    /// that was replaced underneath the cursor has no "same row" to speak
    /// of.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: DoubleClickRule = DoubleClickRule {
        interval: Duration::from_millis(400),
        half_box: (5.0, 5.0),
    };

    fn at(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn second_press_within_interval_and_box_is_a_double() {
        let mut t = ClickTracker::default();
        let t0 = Instant::now();
        let first = t.press("a", at(10.0, 10.0), t0, &RULE);
        assert_eq!(first, ClickReport { double: false, gap: None });
        let second = t.press("a", at(12.0, 8.0), t0 + Duration::from_millis(150), &RULE);
        assert!(second.double);
        assert_eq!(second.gap, None);
    }

    #[test]
    fn a_double_click_consumes_both_presses() {
        let mut t = ClickTracker::default();
        let t0 = Instant::now();
        t.press("a", at(0.0, 0.0), t0, &RULE);
        assert!(t.press("a", at(0.0, 0.0), t0 + Duration::from_millis(100), &RULE).double);
        // The third press starts over: no stamp to pair with.
        let third = t.press("a", at(0.0, 0.0), t0 + Duration::from_millis(200), &RULE);
        assert_eq!(third, ClickReport { double: false, gap: None });
        // ... and the fourth pairs with the third.
        assert!(t.press("a", at(0.0, 0.0), t0 + Duration::from_millis(300), &RULE).double);
    }

    #[test]
    fn a_press_past_the_interval_is_a_single_with_its_gap() {
        let mut t = ClickTracker::default();
        let t0 = Instant::now();
        t.press("a", at(0.0, 0.0), t0, &RULE);
        let late = t.press("a", at(0.0, 0.0), t0 + Duration::from_millis(401), &RULE);
        assert!(!late.double);
        assert_eq!(late.gap, Some(Duration::from_millis(401)));
    }

    #[test]
    fn a_press_outside_the_box_is_a_single_with_its_gap() {
        let mut t = ClickTracker::default();
        let t0 = Instant::now();
        t.press("a", at(0.0, 0.0), t0, &RULE);
        let moved = t.press("a", at(5.5, 0.0), t0 + Duration::from_millis(100), &RULE);
        assert_eq!(moved, ClickReport { double: false, gap: Some(Duration::from_millis(100)) });
        // The moved press is the new reference: pairing with it works.
        assert!(t.press("a", at(5.0, 0.0), t0 + Duration::from_millis(200), &RULE).double);
    }

    #[test]
    fn a_press_on_another_row_is_a_single_without_a_gap() {
        let mut t = ClickTracker::default();
        let t0 = Instant::now();
        t.press("a", at(0.0, 0.0), t0, &RULE);
        let other = t.press("b", at(0.0, 0.0), t0 + Duration::from_millis(100), &RULE);
        assert_eq!(other, ClickReport { double: false, gap: None });
        assert!(t.press("b", at(0.0, 0.0), t0 + Duration::from_millis(200), &RULE).double);
    }

    #[test]
    fn the_same_instant_counts_as_consecutive() {
        let mut t = ClickTracker::default();
        let t0 = Instant::now();
        t.press("a", at(0.0, 0.0), t0, &RULE);
        assert!(t.press("a", at(0.0, 0.0), t0, &RULE).double);
    }

    #[test]
    fn reset_drops_the_stamp() {
        let mut t = ClickTracker::default();
        let t0 = Instant::now();
        t.press("a", at(0.0, 0.0), t0, &RULE);
        t.reset();
        let next = t.press("a", at(0.0, 0.0), t0 + Duration::from_millis(50), &RULE);
        assert_eq!(next, ClickReport { double: false, gap: None });
    }

    #[test]
    fn the_box_is_inclusive_on_both_axes() {
        let mut t = ClickTracker::default();
        let t0 = Instant::now();
        t.press("a", at(0.0, 0.0), t0, &RULE);
        assert!(t.press("a", at(5.0, -5.0), t0 + Duration::from_millis(10), &RULE).double);
    }

    #[test]
    fn platform_intervals_are_clamped_to_the_documented_range() {
        assert_eq!(clamp_interval(None), DEFAULT_INTERVAL);
        assert_eq!(clamp_interval(Some(Duration::ZERO)), DEFAULT_INTERVAL);
        assert_eq!(clamp_interval(Some(Duration::from_secs(60))), DEFAULT_INTERVAL);
        assert_eq!(
            clamp_interval(Some(Duration::from_millis(400))),
            Duration::from_millis(400)
        );
        let rule = DoubleClickRule::os();
        assert!(rule.interval >= MIN_INTERVAL && rule.interval <= MAX_INTERVAL);
        assert!(rule.half_box.0 > 0.0 && rule.half_box.1 > 0.0);
    }
}
