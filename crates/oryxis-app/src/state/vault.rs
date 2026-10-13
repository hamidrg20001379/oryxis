//! Vault access UI state: the lock screen, master-password setup, and the
//! destroy-confirm flag. Grouped off the `Oryxis` god-struct as part of the
//! modules-by-feature direction (field grouping only).
//!
//! Note: the live `VaultStore` handle stays at `Oryxis::vault`; only the
//! transient unlock/setup UI lives here. The struct is named `VaultUi` to
//! avoid colliding with that `vault` field.

use super::VaultState;

/// Which set / change-password operation is pending behind an off-thread
/// Argon2id calibration (E1). Carried through `VaultKdfCalibrated` so the
/// second phase applies the right vault mutation with the tuned params.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VaultPwOp {
    /// First-ever master password (onboarding / Settings set-password
    /// form): `VaultStore::set_master_password_with_params`.
    FirstSetup,
    /// Add a password to a passwordless vault (Settings set-password
    /// form once a vault exists): `set_user_password_with_params`.
    SetUser,
    /// Change an existing master password (current already verified):
    /// `set_user_password_with_params`.
    Change,
}

/// The boot error screen's repair of a damaged vault file (issue #250).
#[derive(Debug, Clone, Default)]
pub(crate) enum VaultRebuild {
    #[default]
    Idle,
    /// The copy runs on a blocking worker; the button is disabled.
    Running,
    /// The rebuilt file is in place. `restart_failed` is set when
    /// `relaunch_self` could not spawn the replacement process.
    Done { report: oryxis_vault::RebuildReport, restart_failed: bool },
    /// The new file could not be written; the damaged one is as it was.
    Failed(String),
}

/// Lock screen + master-password setup + destroy-confirm UI state.
#[derive(Debug, Clone, Default)]
pub(crate) struct VaultUi {
    /// Loading / NeedSetup / Locked / Unlocked.
    pub(crate) state: VaultState,
    /// Why the boot left `state` at `Loading`: the text of the error
    /// `VaultStore::open_default` returned, shown on the error screen
    /// beside the vault path and the debug-log path. `None` once the
    /// vault opened (the state moved on) and on a boot that never
    /// tried.
    pub(crate) open_error: Option<String>,
    /// The open failed because SQLite could only open the file
    /// read-only (`VaultError::ReadOnly`): the error screen swaps its
    /// generic hint for the one that says what to check. The owner's
    /// call (2026-10-12): a vault this user cannot write to is REFUSED,
    /// with the reason, never opened read-only (nothing changed would
    /// be saved, and the pre-0.20 opens did exactly that in silence).
    pub(crate) open_read_only: bool,
    /// The open failed on a damaged file (`VaultError::Corrupt`): the
    /// error screen offers the rebuild (`oryxis_vault::rebuild_vault`).
    pub(crate) open_corrupt: bool,
    /// Where that rebuild stands.
    pub(crate) rebuild: VaultRebuild,
    /// Password typed on the lock / setup screen.
    pub(crate) password_input: String,
    /// Whether the lock-screen password is shown as plain text.
    pub(crate) password_visible: bool,
    /// Error shown on the lock / setup screen.
    pub(crate) error: Option<String>,
    /// Whether a master password is set (vs the empty-password vault).
    pub(crate) has_user_password: bool,
    /// When no master password is set yet, whether the inline set-password
    /// form is revealed. Flipped by the header switch so the toggle has a
    /// visible effect before a password exists; ignored once one is set.
    pub(crate) show_password_form: bool,
    /// Two-step confirm latch for removing the master password. Toggling
    /// the switch off (or the removal path) arms this; the destructive
    /// action only runs once the user confirms, so an accidental flip
    /// doesn't silently drop encryption.
    pub(crate) confirm_remove_password: bool,
    /// Two-step confirm latch for the manual lock. Lock Vault tears down
    /// every live SSH session and tab, so the button asks first; an
    /// accidental click must not sever all open connections. Armed by
    /// `LockVaultConfirm`, cleared by `CancelLockVaultConfirm` and by
    /// either lock path.
    pub(crate) lock_confirm: bool,
    /// The confirm dialog's "always use the selected option" opt-in.
    /// Reset every time the dialog arms (a stale check would turn a
    /// one-off choice into a standing one); when checked, the chosen
    /// button persists itself as the `manual_lock_action` setting.
    pub(crate) lock_confirm_remember: bool,
    /// Whether the "change master password" form is open (only meaningful
    /// once a password is set). Reuses `new_password` / `confirm_password`
    /// for the new value and adds `current_password` for verification.
    pub(crate) change_password_open: bool,
    /// Current master password typed in the change-password form, checked
    /// against the vault before the rotation runs.
    pub(crate) current_password: String,
    /// New master password (Settings > Security).
    pub(crate) new_password: String,
    /// Confirm new master password (Settings > Security).
    pub(crate) confirm_password: String,
    /// Inline error on the master-password change form.
    pub(crate) password_error: Option<String>,
    /// Two-step confirm latch for the destroy-vault action.
    pub(crate) destroy_confirm: bool,
    /// Lock screen: the user asked for the typed-password form even though
    /// biometric unlock is offered (biometric-first layout). Also flipped
    /// on automatically when the OS prompt fails or is cancelled, so the
    /// user is never stuck without an input. Reset on every lock and on a
    /// successful unlock, so the next lock screen leads with biometrics.
    pub(crate) password_fallback: bool,
    /// Set-password forms (Settings and the onboarding final slide):
    /// whether the "also enable biometric unlock" opt-in is checked.
    /// Seeded to platform availability when the form opens / at boot, so
    /// the convenience layer is offered at password-creation time (the
    /// market-standard moment) without another trip to Settings.
    pub(crate) setup_enable_biometric: bool,
    /// E1: a set / change-password flow is calibrating the Argon2id KDF
    /// off the UI thread. Drives the "Calibrating encryption..." spinner
    /// and disables the submit button so the user can't fire it twice
    /// while the worker runs.
    pub(crate) calibrating: bool,
    /// E1: the new password snapshotted at the moment the user confirmed,
    /// applied when `VaultKdfCalibrated` lands. The apply phase must never
    /// read the live form buffers: they stay editable during the ~1s
    /// calibration, and Cancel clears them, so reading them late would
    /// rotate the vault key to whatever the buffer holds by then (a
    /// cleared buffer would rotate to the empty-string key). A cancel path
    /// drops this snapshot instead, which aborts the pending apply.
    pub(crate) pending_kdf_pw: Option<String>,
}
