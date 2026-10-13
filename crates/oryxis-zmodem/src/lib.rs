//! In-terminal ZMODEM file transfer for Oryxis.
//!
//! Two pieces of terminal-side glue around the `zmodem2` protocol state
//! machine (which this crate re-exports):
//!
//! - [`ZmodemDetector`]: watches a terminal output stream for a `sz` /
//!   `rz` initiation header and reports the split point where the wire
//!   stream begins (the `OscSniffer` pattern).
//! - the async driver (added alongside) maps `zmodem2`'s poll/submit
//!   [`Action`]s onto the pane's transport (`WriteWire`) and the local
//!   filesystem (`ReadFile` / `WriteFile`), surfacing progress.
//!
//! The engine lives in the core binary, not a plugin: it must sit in
//! the live byte path of the terminal, which a subprocess cannot reach,
//! and `zmodem2` is small enough (no_std / heapless) to bundle freely.
//!
//! # Protocol coverage: deliberate exclusions
//!
//! Interop with lrzsz's `sz` / `rz` is the contract (enforced by the
//! real-binary tests in `tests/lrzsz_interop.rs`). The following ZMODEM
//! features are absent ON PURPOSE; do not "complete" them without
//! revisiting the reasons:
//!
//! - **ZCOMMAND** (the remote executes commands on this machine): a
//!   security hole with no redeeming use in an SSH client; lrzsz ships
//!   with it disabled too. Never implement.
//! - **ZCNL / ASCII conversion** (sender-flagged line-ending mangling):
//!   misdetection corrupts data silently. Transfers are binary-exact,
//!   always, matching `sz -b` and every modern client's default.
//! - **Sender-driven file management** (replace-if-newer, append,
//!   ZSKIP negotiation): downloads are strictly no-clobber, taking a
//!   browser-style ` (N)` rename on collision, so a remote-controlled
//!   file name can never truncate or overwrite local data.
//! - **Files over 4 GiB**: wire positions are 32-bit in ZMODEM itself;
//!   refused up front with a clear error instead of wrapping.
//! - **XMODEM / YMODEM fallbacks**: strictly worse protocols with no
//!   modern demand; the detector only arms on ZMODEM initiation.

pub mod detector;
pub mod driver;

pub use detector::{Direction, Scan, ZmodemDetector};
pub use driver::{
    DEFAULT_STREAMING_WINDOW, PART_SUFFIX, Progress, SERIAL_STREAMING_WINDOW, TransferIo,
    TransferSpec, place_file, resume_owner_path, resume_owner_path_for_part, run,
};

/// The canonical ZMODEM cancel sequence: ten `CAN` (ZDLE) bytes then ten
/// backspaces, lrzsz's `canit()`, which the other end takes as an abort
/// and which also erases the CANs from the remote's line. `zmodem2`'s
/// `abort()` queues these same bytes for a session it is running; this
/// copy is for the ends it does not see: the user declining an upload's
/// file picker before any session exists, and a local failure that ends
/// the driver mid-session. Either way the remote `rz` / `sz` exits at once
/// instead of waiting out its timeout.
pub const CANCEL: &[u8] = &[
    0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08,
    0x08, 0x08, 0x08, 0x08,
];

// Re-export the protocol primitives so the app drives one dependency.
pub use zmodem2::{Action, Event, FileInfo, Position, Receiver, Sender};
