//! `VaultStore::open`: the error names the step it failed at (issue
//! #250 arrived as "Failed to open vault database" and nothing else).

use super::*;

#[test]
fn a_file_that_is_not_a_database_fails_at_the_first_read_and_says_so() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), b"this is not an SQLite file, by a long way\n").unwrap();
    let Err(err) = VaultStore::open(tmp.path()) else {
        panic!("garbage must not open");
    };
    match &err {
        VaultError::Open { step, source } => {
            assert_eq!(*step, "pragmas");
            assert!(
                matches!(**source, VaultError::Database(_)),
                "the cause is the engine's, got {source:?}"
            );
        }
        other => panic!("expected VaultError::Open, got {other:?}"),
    }
    let text = err.to_string();
    assert!(text.contains("pragmas"), "{text}");
    assert!(text.contains("not a database"), "{text}");
}

#[test]
fn the_session_log_size_migration_refuses_a_caller_transaction() {
    let vault = temp_vault();
    vault.db.execute_batch("BEGIN").unwrap();
    let err = vault.ensure_session_log_bytes().expect_err("must refuse");
    assert!(err.to_string().contains("session log sizes"), "{err}");
    assert!(err.to_string().contains("already open"), "{err}");
    vault.db.execute_batch("ROLLBACK").unwrap();
    // Outside a transaction it is idempotent.
    vault.ensure_session_log_bytes().unwrap();
    vault.ensure_session_log_bytes().unwrap();
}

#[test]
fn open_error_display_carries_the_step_and_the_cause() {
    let err = VaultError::Open {
        step: "schema",
        source: Box::new(VaultError::Database("session log sizes, begin: database is locked".into())),
    };
    assert_eq!(
        err.to_string(),
        "Could not open the vault at step \"schema\": Database error: session log sizes, begin: database is locked"
    );
}

/// An owner's file at 0444 (a restore from read-only media) is made
/// writable by the open itself, before SQLite would fall back to a
/// read-only handle: the pre-0.20 opens healed it the same way, so the
/// read-only refusal must not reach it.
#[cfg(unix)]
#[test]
fn an_owner_file_without_write_bits_is_healed_before_the_read_only_check() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    drop(VaultStore::open(&path).unwrap());
    for ext in ["", "-wal", "-shm"] {
        let p = std::path::PathBuf::from(format!("{}{ext}", path.display()));
        if p.exists() {
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o444)).unwrap();
        }
    }
    let vault = VaultStore::open(&path).expect("the open heals the mode");
    vault.set_setting("probe", "1").unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "{mode:o}");
}

/// A vault the open CREATES is 0600 from its first session, siblings
/// included: the chmod that heals an existing file runs before the
/// SQLite open, when a new file does not exist yet.
#[cfg(unix)]
#[test]
fn a_new_vault_is_private_from_the_start() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vault.db");
    let vault = VaultStore::open(&path).unwrap();
    vault.set_setting("probe", "1").unwrap();
    for ext in ["", "-wal", "-shm"] {
        let p = std::path::PathBuf::from(format!("{}{ext}", path.display()));
        if p.exists() {
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{}: {mode:o}", p.display());
        }
    }
}
