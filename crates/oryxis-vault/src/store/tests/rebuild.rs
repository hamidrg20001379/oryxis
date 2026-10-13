//! `store::rebuild`: a vault with damaged pages in the recordings table
//! (issue #250's file) refuses to open and is rebuilt around the damage.

use super::*;
use crate::store::rebuild::{rebuild_vault, DataKind};
use std::io::{Seek, SeekFrom, Write};

/// A vault in the 0.19 shape (no `session_logs.bytes`, no triggers) so
/// the open has to run the backfill that walks the chunks, as it did
/// for the reporter; the schema change needs the triggers gone first.
fn vault_in_0_19_shape(path: &std::path::Path) {
    let mut v = VaultStore::open(path).unwrap();
    v.set_master_password("pw").unwrap();
    for i in 0..3 {
        let conn = Connection::new(format!("host {i}"), format!("h{i}.example.com"));
        v.save_connection(&conn, Some("secret")).unwrap();
    }
    v.set_setting("probe", "kept").unwrap();
    let conn_id = Uuid::new_v4();
    for n in 0..6u32 {
        let id = Uuid::new_v4();
        v.create_session_log(&id, &conn_id, &format!("log {n}")).unwrap();
        for k in 0..80u32 {
            let data: Vec<u8> = (0..3000).map(|i| ((i * 31 + (n * 80 + k) as usize) % 251) as u8).collect();
            v.append_session_data(&id, &data, Some(k as i64 * 10), false).unwrap();
        }
        v.end_session_log(&id).unwrap();
    }
    v.db.execute_batch(
        "DROP TRIGGER session_log_bytes_insert;
         DROP TRIGGER session_log_bytes_delete;
         DROP TRIGGER session_log_bytes_update;
         ALTER TABLE session_logs DROP COLUMN bytes;",
    )
    .unwrap();
}

/// The leaf pages of `table` in TREE order (dbstat's `path`), with the
/// number of rows on each.
fn leaves(path: &std::path::Path, table: &str) -> (u64, Vec<(u64, u64)>) {
    let db = rusqlite::Connection::open(path).unwrap();
    let page_size = db.query_row("PRAGMA page_size", [], |r| r.get::<_, i64>(0)).unwrap() as u64;
    let mut stmt = db
        .prepare("SELECT pageno, ncell FROM dbstat WHERE name = ?1 AND pagetype = 'leaf' ORDER BY path")
        .unwrap();
    let pages = stmt
        .query_map([table], |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    (page_size, pages)
}

/// Overwrite the given pages with junk whose first byte is no b-tree
/// page type, in the main file (the close above checkpointed the WAL).
fn junk_pages(path: &std::path::Path, page_size: u64, pages: &[u64]) {
    let junk: Vec<u8> = (0..page_size).map(|i| ((i * 7919) % 253) as u8 + 1).collect();
    let mut f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    for &p in pages {
        f.seek(SeekFrom::Start((p - 1) * page_size)).unwrap();
        f.write_all(&junk).unwrap();
    }
    f.sync_all().unwrap();
}

/// Every third leaf of the chunks tree from its middle on, so every
/// recording loses a chunk and the per-log backfill cannot miss it.
/// Returns how many rows those leaves held.
fn damage_chunk_pages(path: &std::path::Path) -> u64 {
    let (page_size, pages) = leaves(path, "session_log_chunks");
    assert!(pages.len() > 20, "{} leaf pages", pages.len());
    let hit: Vec<(u64, u64)> = pages.iter().skip(pages.len() / 3).step_by(3).copied().collect();
    junk_pages(path, page_size, &hit.iter().map(|(p, _)| *p).collect::<Vec<_>>());
    hit.iter().map(|(_, n)| n).sum()
}

#[test]
fn a_damaged_recordings_table_refuses_the_open_and_is_rebuilt_around_the_damage() {
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    vault_in_0_19_shape(&path);
    let lost = damage_chunk_pages(&path);

    // The reporter's open: the backfill walks the damaged tree.
    let Err(err) = VaultStore::open(&path) else {
        panic!("the damaged vault must not open");
    };
    match &err {
        VaultError::Open { step, source } => {
            assert_eq!(*step, "schema");
            assert!(matches!(**source, VaultError::Corrupt(_)), "{source:?}");
        }
        other => panic!("{other:?}"),
    }
    assert!(err.to_string().contains("backfill"), "{err}");
    assert!(err.to_string().contains("malformed"), "{err}");

    let report = rebuild_vault(&path).unwrap();
    assert!(report.damaged_path.exists(), "{}", report.damaged_path.display());
    assert!(report.damaged_path.to_string_lossy().contains(".damaged-"));
    let chunks = report.tables.iter().find(|t| t.name == "session_log_chunks").unwrap();
    assert!(chunks.unreadable_stretches >= 1, "{chunks:?}");
    // Exactly the rows on the damaged leaves are gone: the probe narrows
    // back to the first readable row after each one.
    assert_eq!(chunks.copied, 480 - lost, "{chunks:?}");
    for t in &report.tables {
        if t.name != "session_log_chunks" {
            assert!(!t.lost_anything(), "{t:?}");
        }
    }
    assert_eq!(report.damaged_tables().len(), 1);

    // The rebuilt file opens, unlocks with the same password and keeps
    // everything outside the damage; the recordings' sizes come from
    // the triggers over what was copied.
    let mut v = VaultStore::open(&path).expect("the rebuilt vault opens");
    v.unlock("pw").unwrap();
    assert_eq!(v.list_connections().unwrap().len(), 3);
    assert_eq!(v.get_connection_password(&v.list_connections().unwrap()[0].id).unwrap().as_deref(), Some("secret"));
    assert_eq!(v.get_setting("probe").unwrap().as_deref(), Some("kept"));
    assert_eq!(v.list_session_logs().unwrap().len(), 6);
    let total = v.session_logs_total_bytes().unwrap();
    assert!(total > 0, "{total}");
    let copied: i64 = v.db.query_row("SELECT count(*) FROM session_log_chunks", [], |r| r.get(0)).unwrap();
    assert_eq!(copied as u64, chunks.copied);
    let ok: String = v.db.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap();
    assert_eq!(ok, "ok");
}

#[test]
fn an_intact_vault_rebuilds_whole() {
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    vault_in_0_19_shape(&path);
    let report = rebuild_vault(&path).unwrap();
    assert!(report.damaged_tables().is_empty(), "{report:?}");
    let chunks = report.tables.iter().find(|t| t.name == "session_log_chunks").unwrap();
    assert_eq!(chunks.copied, 480);
    let mut v = VaultStore::open(&path).unwrap();
    v.unlock("pw").unwrap();
    assert_eq!(v.list_session_logs().unwrap().len(), 6);
}

#[test]
fn a_corrupt_answer_from_the_engine_is_its_own_error() {
    let err: VaultError = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
        Some("database disk image is malformed".into()),
    )
    .into();
    assert!(matches!(err, VaultError::Corrupt(_)), "{err:?}");
    assert_eq!(err.to_string(), "The vault file is damaged: database disk image is malformed");
}

#[test]
fn damage_at_the_very_start_of_a_table_keeps_everything_after_it() {
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    vault_in_0_19_shape(&path);
    let (page_size, pages) = leaves(&path, "session_log_chunks");
    let first: Vec<(u64, u64)> = pages.iter().take(2).copied().collect();
    junk_pages(&path, page_size, &first.iter().map(|(p, _)| *p).collect::<Vec<_>>());
    let lost: u64 = first.iter().map(|(_, n)| n).sum();

    let report = rebuild_vault(&path).unwrap();
    let chunks = report.tables.iter().find(|t| t.name == "session_log_chunks").unwrap();
    assert_eq!(chunks.copied, 480 - lost, "{chunks:?}");
    assert_eq!(chunks.unreadable_stretches, 1, "{chunks:?}");
}

#[test]
fn hosts_in_a_folder_the_damage_took_are_kept_at_the_top_level() {
    use oryxis_core::models::group::Group;
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    {
        let mut v = VaultStore::open(&path).unwrap();
        v.set_master_password("pw").unwrap();
        let folder = Group::new("prod");
        v.save_group(&folder).unwrap();
        for i in 0..3 {
            let mut conn = Connection::new(format!("in folder {i}"), format!("p{i}.example.com"));
            conn.group_id = Some(folder.id);
            v.save_connection(&conn, None).unwrap();
        }
        v.save_connection(&Connection::new("loose", "l.example.com"), None).unwrap();
    }
    let (page_size, pages) = leaves(&path, "groups");
    junk_pages(&path, page_size, &pages.iter().map(|(p, _)| *p).collect::<Vec<_>>());

    let report = rebuild_vault(&path).unwrap();
    let groups = report.tables.iter().find(|t| t.name == "groups").unwrap();
    assert_eq!(groups.copied, 0, "{groups:?}");
    let conns = report.tables.iter().find(|t| t.name == "connections").unwrap();
    assert_eq!(conns.copied, 4, "{conns:?}");
    assert_eq!(conns.detached, 3, "{conns:?}");

    let mut v = VaultStore::open(&path).unwrap();
    v.unlock("pw").unwrap();
    let hosts = v.list_connections().unwrap();
    assert_eq!(hosts.len(), 4);
    assert!(hosts.iter().all(|c| c.group_id.is_none()), "{:?}", hosts.iter().map(|c| c.group_id).collect::<Vec<_>>());
    let fk: Vec<String> = v
        .db
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(fk.is_empty(), "{fk:?}");
}

#[test]
fn every_table_of_this_build_has_a_kind() {
    let vault = temp_vault();
    let tables: Vec<String> = vault
        .db
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(tables.len() > 20, "{tables:?}");
    for t in &tables {
        assert_ne!(DataKind::of_table(t).0, DataKind::Newer, "{t} has no kind in DataKind::of_table");
    }
}

/// A downgrade leaves a newer build's table and column in the file:
/// the rebuild carries both instead of dropping them in silence.
#[test]
fn a_newer_builds_table_and_column_survive_the_rebuild() {
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    {
        let mut v = VaultStore::open(&path).unwrap();
        v.set_master_password("pw").unwrap();
        v.save_connection(&Connection::new("h", "h.example.com"), None).unwrap();
        v.db.execute_batch(
            "CREATE TABLE future_things (id TEXT PRIMARY KEY, payload TEXT NOT NULL);
             CREATE INDEX idx_future_payload ON future_things(payload);
             INSERT INTO future_things VALUES ('a', 'one'), ('b', 'two'), ('c', 'three');
             ALTER TABLE connections ADD COLUMN future_flag TEXT;
             UPDATE connections SET future_flag = 'kept';",
        )
        .unwrap();
    }
    let report = rebuild_vault(&path).unwrap();
    let newer = report.by_kind().into_iter().find(|k| k.kind == DataKind::Newer).unwrap();
    assert_eq!(newer.kept, 3, "{newer:?}");
    assert!(!newer.lost_anything(), "{newer:?}");
    assert!(report.damaged_tables().is_empty(), "{report:?}");

    let v = VaultStore::open(&path).unwrap();
    let rows: i64 = v.db.query_row("SELECT count(*) FROM future_things", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 3);
    let index: i64 = v
        .db
        .query_row("SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_future_payload'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(index, 1);
    let flag: String = v.db.query_row("SELECT future_flag FROM connections", [], |r| r.get(0)).unwrap();
    assert_eq!(flag, "kept");
}

#[test]
fn the_report_speaks_in_kinds() {
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    vault_in_0_19_shape(&path);
    damage_chunk_pages(&path);
    let report = rebuild_vault(&path).unwrap();
    let kinds = report.by_kind();
    let lost: Vec<_> = kinds.iter().filter(|k| k.lost_anything()).collect();
    assert_eq!(lost.len(), 1, "{lost:?}");
    // Every recording survives; part of their content does not.
    assert_eq!(lost[0].kind, DataKind::Recordings);
    assert_eq!(lost[0].kept, 6);
    assert_eq!(lost[0].skipped, 0);
    assert!(lost[0].content_gaps);
    let hosts = kinds.iter().find(|k| k.kind == DataKind::Hosts).unwrap();
    assert_eq!(hosts.kept, 3);
}
