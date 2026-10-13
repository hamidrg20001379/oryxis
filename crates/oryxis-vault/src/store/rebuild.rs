//! Rebuilding a damaged vault file (issue #250).
//!
//! A vault whose b-trees are damaged (`SQLITE_CORRUPT`, "database disk
//! image is malformed") refuses to open since 0.20, because the open
//! runs a migration that walks the recordings table. The repair copies
//! every table of the damaged file, row by row, into a fresh vault
//! made by `VaultStore::open` (current schema, every index and trigger
//! in place), skipping the rows the engine cannot read. Nothing is
//! decrypted: the ciphertext, the salt and the KDF parameters travel as
//! they are, so the rebuilt file opens with the same master password.
//!
//! The copy is driven by `rowid`, always upward, so a tree that points
//! at a page twice (the reporter's "2nd reference to page") cannot loop
//! it: a batch that fails keeps the rows read before the failure, and
//! the walk then probes forward with a doubling step until the engine
//! answers again, then narrows back by binary search so the rows right
//! after the damage are kept. A probe never accepts a rowid at or below
//! the last row copied. Rows read whole that the new schema refuses are
//! skipped and counted, and references to rows the damage took (a host
//! in a lost folder) are cleared after the copy. The rebuilt file replaces the damaged one by
//! rename, which stays beside it as `vault.db.damaged-<stamp>` with its
//! `-wal` / `-shm` siblings (a WAL left behind belongs to the file it
//! was written for).
//!
//! The bundled SQLite carries no `sqlite3_recover` (the CLI's
//! `.recover`), so this is the whole repair.

use std::path::{Path, PathBuf};
use std::time::Instant;

use rusqlite::types::Value;
use rusqlite::{params_from_iter, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::{VaultError, VaultStore};

/// Rows read per `SELECT`: small enough that a failure late in a batch
/// loses little progress, large enough that a 600 MB chunk table does
/// not pay a statement per row.
const BATCH: usize = 256;

/// Outcome of one table's copy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TableRebuild {
    pub name: String,
    /// Rows written to the new file.
    pub copied: u64,
    /// Stretches of the table the engine could not read. The rows lost
    /// in a stretch are unknown (they are what could not be read), so
    /// this counts stretches, not rows.
    pub unreadable_stretches: u64,
    /// Rows read whole that the new file refused (a constraint the
    /// damaged values break): skipped, like an unreadable stretch.
    pub rejected: u64,
    /// Rows kept whose reference pointed at a row the damage took (a
    /// host whose folder was lost): the reference is cleared, or the
    /// row dropped when the column cannot be empty.
    pub detached: u64,
    /// Columns of the damaged file this file could not take (a newer
    /// build's column whose definition the engine refused to add).
    pub lost_columns: u64,
}

impl TableRebuild {
    /// Whether anything of this table did not survive as it was.
    pub fn lost_anything(&self) -> bool {
        self.unreadable_stretches > 0 || self.rejected > 0 || self.detached > 0 || self.lost_columns > 0
    }

    fn lost(name: &str) -> Self {
        TableRebuild {
            name: name.to_string(),
            copied: 0,
            unreadable_stretches: 1,
            rejected: 0,
            detached: 0,
            lost_columns: 0,
        }
    }
}

/// What a table holds, as the user knows it: the rebuild report speaks
/// in these, never in table names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum DataKind {
    /// The salt, KDF parameters and password check: without them the
    /// master password cannot open anything.
    VaultData,
    Hosts,
    Folders,
    SessionGroups,
    Keys,
    Identities,
    Proxies,
    Snippets,
    PortForwards,
    KnownHosts,
    ApprovedProxyCommands,
    LoginScripts,
    CloudAccounts,
    Recordings,
    ConnectionLog,
    CommandHistory,
    AiChat,
    Themes,
    Sync,
    Settings,
    /// A table this build does not know: a newer Oryxis made it.
    Newer,
}

impl DataKind {
    /// The kind of `table`, and whether its rows are the ones the kind
    /// counts (a recording is a `session_logs` row; its chunks are its
    /// content). Every table this build creates maps to a kind other
    /// than `Newer` (`every_table_of_this_build_has_a_kind`).
    pub fn of_table(table: &str) -> (DataKind, bool) {
        use DataKind::*;
        match table {
            "vault_meta" => (VaultData, true),
            "connections" => (Hosts, true),
            "groups" => (Folders, true),
            "session_groups" => (SessionGroups, true),
            "keys" => (Keys, true),
            "identities" => (Identities, true),
            "proxy_identities" => (Proxies, true),
            "snippets" => (Snippets, true),
            "install_runs" => (Snippets, false),
            "port_forward_rules" => (PortForwards, true),
            "known_hosts" => (KnownHosts, true),
            "trusted_proxy_commands" => (ApprovedProxyCommands, true),
            "login_scripts" => (LoginScripts, true),
            "cloud_profiles" => (CloudAccounts, true),
            "session_logs" => (Recordings, true),
            "session_log_chunks" => (Recordings, false),
            "logs" => (ConnectionLog, true),
            "command_history" => (CommandHistory, true),
            "chat_conversations" => (AiChat, true),
            "chat_messages" => (AiChat, false),
            "custom_terminal_themes" | "custom_ui_themes" => (Themes, true),
            "sync_peers" => (Sync, true),
            "sync_metadata" => (Sync, false),
            "settings" => (Settings, true),
            _ => (Newer, true),
        }
    }
}

/// One kind's outcome, summed over its tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindRebuild {
    pub kind: DataKind,
    /// Items of the kind kept (hosts, recordings, conversations).
    pub kept: u64,
    /// Damaged stretches and refused rows among those items.
    pub skipped: u64,
    /// The items were kept but part of their content was not (a
    /// recording missing chunks, a conversation missing messages).
    pub content_gaps: bool,
    /// Items kept without a reference to something the damage took.
    pub detached: u64,
}

impl KindRebuild {
    pub fn lost_anything(&self) -> bool {
        self.skipped > 0 || self.content_gaps || self.detached > 0
    }
}

/// What a rebuild did, kept for the user: how many rows each table
/// kept, where the damage was, and where the damaged file went.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RebuildReport {
    pub tables: Vec<TableRebuild>,
    /// The damaged file's new name beside the rebuilt vault.
    pub damaged_path: PathBuf,
    pub elapsed_ms: u64,
}

impl RebuildReport {
    /// Tables that lost something, in copy order.
    pub fn damaged_tables(&self) -> Vec<&TableRebuild> {
        self.tables.iter().filter(|t| t.lost_anything()).collect()
    }

    pub fn copied_total(&self) -> u64 {
        self.tables.iter().map(|t| t.copied).sum()
    }

    /// The report by kind, in `DataKind` order.
    pub fn by_kind(&self) -> Vec<KindRebuild> {
        let mut kinds: std::collections::BTreeMap<DataKind, KindRebuild> = Default::default();
        for t in &self.tables {
            let (kind, counts) = DataKind::of_table(&t.name);
            let entry = kinds.entry(kind).or_insert(KindRebuild {
                kind,
                kept: 0,
                skipped: 0,
                content_gaps: false,
                detached: 0,
            });
            if counts {
                entry.kept += t.copied;
                entry.skipped += t.unreadable_stretches + t.rejected;
                entry.content_gaps |= t.lost_columns > 0;
            } else {
                entry.content_gaps |= t.unreadable_stretches + t.rejected + t.lost_columns > 0;
            }
            entry.detached += t.detached;
        }
        kinds.into_values().collect()
    }
}

/// A column of a table as both files know it.
struct CopyPlan {
    name: String,
    /// Columns present in BOTH files, quoted for SQL.
    columns: Vec<String>,
    /// Whether the table's primary key IS the rowid (an `INTEGER
    /// PRIMARY KEY` column): then the key column carries the rowid and
    /// naming `rowid` beside it would be the same value twice. Every
    /// other table gets its rowid copied explicitly, since readers
    /// order by it (`session_log_chunks`, `chat_messages`).
    rowid_is_key: bool,
}

/// Rebuild the vault at `path` in place. On success the rebuilt file
/// sits at `path` and the damaged one beside it (`report.damaged_path`).
/// A failure to WRITE the new file (disk full, the folder not writable)
/// leaves the damaged file untouched and the partial copy removed; a
/// failure to READ is what the rebuild is for and never fails it.
pub fn rebuild_vault(path: &Path) -> Result<RebuildReport, VaultError> {
    let started = Instant::now();
    let src = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    // A damaged file's WAL may be the damaged part; the engine recovers
    // what it can of it when the first statement runs.
    let tables = source_tables(&src)?;

    let staging = sibling(path, ".rebuilding");
    remove_with_siblings(&staging)?;
    // `VaultStore::open` makes the current schema, migrations included.
    let fresh = VaultStore::open(&staging)?;
    let dst = &fresh.db;
    // Bulk inserts: the file is private until the rename below, so a
    // crash mid-copy costs nothing durable. Foreign keys OFF while
    // copying: the bundled engine enforces them by default, and a child
    // whose parent the damage took (a host in a lost folder) would be
    // refused; the references are repaired after the copy instead.
    dst.execute_batch("PRAGMA synchronous=OFF; PRAGMA foreign_keys=OFF;")?;

    let mut report = Vec::with_capacity(tables.len());
    let result = (|| -> Result<(), VaultError> {
        for table in &tables {
            match copy_plan(&src, dst, table)? {
                Some((plan, lost_columns)) => {
                    let mut done = copy_table(&src, dst, &plan)?;
                    done.lost_columns = lost_columns;
                    report.push(done);
                }
                // Not even its definition could be carried over.
                None => report.push(TableRebuild::lost(table)),
            }
        }
        detach_dangling_references(dst, &mut report)?;
        dst.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA synchronous=NORMAL; PRAGMA wal_checkpoint(TRUNCATE);",
        )?;
        Ok(())
    })();
    drop(fresh);
    drop(src);
    if let Err(e) = result {
        let _ = remove_with_siblings(&staging);
        return Err(e);
    }

    let damaged_path = sibling(path, &format!(".damaged-{}", stamp()));
    rename_with_siblings(path, &damaged_path)?;
    if let Err(e) = rename_with_siblings(&staging, path) {
        // Put the damaged file back where the app looks for it rather
        // than leave no vault at all.
        let _ = rename_with_siblings(&damaged_path, path);
        return Err(e);
    }
    Ok(RebuildReport {
        tables: report,
        damaged_path,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// The damaged file's tables, in `sqlite_master` order, with the
/// recordings' chunks last: their triggers keep `session_logs.bytes`,
/// so the log rows have to exist before the chunks arrive.
fn source_tables(src: &Connection) -> Result<Vec<String>, VaultError> {
    let mut stmt = src.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )?;
    let mut names: Vec<String> =
        stmt.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    names.sort_by_key(|n| n == "session_log_chunks");
    Ok(names)
}

/// How `table` is copied, and how many of its columns could not be.
/// A table or a column the damaged file has and this build's schema
/// does not (a newer Oryxis made it, then the app was downgraded) is
/// recreated in the new file from the damaged file's own definition
/// rather than left behind. `None` when the table could not be carried
/// over at all.
fn copy_plan(src: &Connection, dst: &Connection, table: &str) -> Result<Option<(CopyPlan, u64)>, VaultError> {
    let mut dst_cols = table_columns(dst, table)?;
    if dst_cols.is_empty() {
        if !recreate_newer_table(src, dst, table)? {
            return Ok(None);
        }
        dst_cols = table_columns(dst, table)?;
    }
    let src_cols = table_columns(src, table)?;
    let mut lost_columns = 0;
    let mut added = false;
    for c in &src_cols {
        if dst_cols.iter().any(|d| d.name == c.name) {
            continue;
        }
        // Added as a plain column of the declared type: an ALTER cannot
        // add a NOT NULL column without a default, and the data matters
        // more than the constraint.
        let sql = format!(
            "ALTER TABLE {} ADD COLUMN {} {}",
            quote_ident(table),
            quote_ident(&c.name),
            c.declared
        );
        match dst.execute_batch(&sql) {
            Ok(()) => added = true,
            Err(_) => lost_columns += 1,
        }
    }
    if added {
        dst_cols = table_columns(dst, table)?;
    }
    // Only a lone `INTEGER PRIMARY KEY` aliases the rowid; a composite
    // key with an INTEGER part does not.
    let pk: Vec<&ColumnInfo> = dst_cols.iter().filter(|c| c.pk).collect();
    let rowid_is_key = pk.len() == 1 && pk[0].declared.eq_ignore_ascii_case("INTEGER");
    let columns: Vec<String> = src_cols
        .iter()
        .filter(|c| dst_cols.iter().any(|d| d.name == c.name))
        // Kept by the triggers on `session_log_chunks`: a value copied
        // from the old file would be added to, not replaced.
        .filter(|c| !(table == "session_logs" && c.name == "bytes"))
        .map(|c| quote_ident(&c.name))
        .collect();
    if columns.is_empty() {
        return Ok(None);
    }
    Ok(Some((CopyPlan { name: table.to_string(), columns, rowid_is_key }, lost_columns)))
}

/// Create in the new file a table this build does not know, from the
/// damaged file's own `CREATE TABLE`, with its indexes (an index the
/// engine refuses is left out: the rows matter, the index is rebuilt by
/// whoever needs it). Its triggers are not carried: they belong to the
/// code of the build that wrote them.
fn recreate_newer_table(src: &Connection, dst: &Connection, table: &str) -> Result<bool, VaultError> {
    let sql: Option<String> = src
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |r| r.get(0),
        )
        .optional()?;
    let Some(sql) = sql else {
        return Ok(false);
    };
    if dst.execute_batch(&sql).is_err() {
        return Ok(false);
    }
    let indexes: Vec<String> = {
        let mut stmt = src.prepare(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND tbl_name = ?1 AND sql IS NOT NULL",
        )?;
        stmt.query_map([table], |r| r.get(0))?.collect::<Result<_, _>>()?
    };
    for index in indexes {
        let _ = dst.execute_batch(&index);
    }
    Ok(true)
}

struct ColumnInfo {
    name: String,
    declared: String,
    not_null: bool,
    pk: bool,
}

fn table_columns(db: &Connection, table: &str) -> Result<Vec<ColumnInfo>, VaultError> {
    let mut stmt = db.prepare(&format!("PRAGMA table_info({})", quote_ident(table)))?;
    let cols = stmt
        .query_map([], |r| {
            Ok(ColumnInfo {
                name: r.get::<_, String>(1)?,
                declared: r.get::<_, String>(2)?,
                not_null: r.get::<_, i64>(3)? != 0,
                pk: r.get::<_, i64>(5)? > 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(cols)
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// One table, rowid upward, tolerant of the engine's refusals.
fn copy_table(src: &Connection, dst: &Connection, plan: &CopyPlan) -> Result<TableRebuild, VaultError> {
    let table = quote_ident(&plan.name);
    let cols = plan.columns.join(", ");
    let select = src.prepare(&format!(
        "SELECT rowid, {cols} FROM {table} WHERE rowid >= ?1 ORDER BY rowid LIMIT {BATCH}"
    ));
    let probe = src.prepare(&format!(
        "SELECT rowid FROM {table} WHERE rowid >= ?1 ORDER BY rowid LIMIT 1"
    ));
    let mut out = TableRebuild {
        name: plan.name.clone(),
        copied: 0,
        unreadable_stretches: 0,
        rejected: 0,
        detached: 0,
        lost_columns: 0,
    };
    // A statement that cannot even be prepared (the table's own schema
    // row is unreadable) is a table lost whole.
    let (Ok(mut select), Ok(mut probe)) = (select, probe) else {
        out.unreadable_stretches = 1;
        return Ok(out);
    };
    let insert_sql = if plan.rowid_is_key {
        let marks = vec!["?"; plan.columns.len()].join(", ");
        format!("INSERT OR IGNORE INTO {table} ({cols}) VALUES ({marks})")
    } else {
        let marks = vec!["?"; plan.columns.len() + 1].join(", ");
        format!("INSERT OR IGNORE INTO {table} (rowid, {cols}) VALUES ({marks})")
    };
    let mut insert = dst.prepare(&insert_sql)?;

    // Reads are INCLUSIVE from `start`: a `rowid > k` makes the engine
    // seek the key `k` itself, and when `k` is the last row before a
    // damaged leaf that seek lands in the damage again (measured: the
    // exclusive form lost the first readable row after every damaged
    // leaf, and more when the seek kept failing). `last` is the highest
    // rowid copied; nothing at or below it is accepted from a probe.
    let mut last: i64 = i64::MIN;
    let mut start: i64 = i64::MIN;
    loop {
        let before = last;
        let (rows, failed) = read_batch(&mut select, start, plan.columns.len());

        if !rows.is_empty() {
            dst.execute_batch("BEGIN")?;
            for (rowid, values) in &rows {
                let result = if plan.rowid_is_key {
                    insert.execute(params_from_iter(values.iter()))
                } else {
                    let mut all: Vec<Value> = Vec::with_capacity(values.len() + 1);
                    all.push(Value::Integer(*rowid));
                    all.extend(values.iter().cloned());
                    insert.execute(params_from_iter(all.iter()))
                };
                match result {
                    Ok(_) => out.copied += 1,
                    // Values read whole that the schema refuses: the
                    // row's own damage, skipped. Anything else (a full
                    // disk, an I/O error) is the WRITE side failing,
                    // which ends the rebuild.
                    Err(rusqlite::Error::SqliteFailure(e, _))
                        if matches!(
                            e.code,
                            rusqlite::ErrorCode::ConstraintViolation | rusqlite::ErrorCode::TypeMismatch
                        ) =>
                    {
                        out.rejected += 1
                    }
                    Err(e) => {
                        let _ = dst.execute_batch("ROLLBACK");
                        return Err(e.into());
                    }
                }
                // A row from an out-of-order tree below the cursor is one
                // already copied under its own rowid (OR IGNORE) or one
                // the tree lost its place for; never let it move the
                // cursor backwards.
                if *rowid > last {
                    last = *rowid;
                }
            }
            dst.execute_batch("COMMIT")?;
        }

        if !failed {
            if rows.len() < BATCH {
                break;
            }
            if last > before {
                start = last.saturating_add(1);
                continue;
            }
            // A whole batch that never got past the cursor (every row of
            // it out of order, below `last`): reading on from the same
            // place would return it again, forever. Step past it the way
            // a failed read is stepped past.
        }

        out.unreadable_stretches += 1;
        match next_readable(&mut probe, last) {
            Some(next) => start = next,
            None => break,
        }
    }
    Ok(out)
}

/// One batch from `start` on, keeping what came before a failure; the
/// flag says the engine stopped answering inside it.
fn read_batch(
    select: &mut rusqlite::Statement<'_>,
    start: i64,
    columns: usize,
) -> (Vec<(i64, Vec<Value>)>, bool) {
    let mut rows: Vec<(i64, Vec<Value>)> = Vec::with_capacity(BATCH);
    let Ok(mut cursor) = select.query([start]) else {
        return (rows, true);
    };
    loop {
        let row = match cursor.next() {
            Ok(Some(row)) => row,
            Ok(None) => return (rows, false),
            Err(_) => return (rows, true),
        };
        let Ok(rowid) = row.get::<_, i64>(0) else {
            return (rows, true);
        };
        let mut values = Vec::with_capacity(columns);
        for i in 0..columns {
            match row.get::<_, Value>(i + 1) {
                Ok(v) => values.push(v),
                Err(_) => return (rows, true),
            }
        }
        rows.push((rowid, values));
    }
}

/// The first rowid past `last` the engine can read its way to, after it
/// stopped answering there: a doubling step forward until a probe (the
/// first rowid at or after a key) answers, then a binary search back
/// between the last probe that failed and the first that answered, so
/// the first readable row after the damage is the one returned. `None` when nothing readable is
/// left. The floor is 0 while nothing was copied: this vault's rowids
/// are positive (implicit rowids and `INTEGER PRIMARY KEY` start at 1
/// and nothing writes a negative one), and doubling up from `i64::MIN`
/// would never land among them.
fn next_readable(probe: &mut rusqlite::Statement<'_>, last: i64) -> Option<i64> {
    let floor = if last == i64::MIN { 0 } else { last };
    // `Some(Some(n))` answered with a rowid past the cursor, `Some(None)`
    // nothing past `from`, `None` the engine refused (or answered with a
    // rowid it already walked past, which is the same as no answer).
    let mut ask = |from: i64| -> Option<Option<i64>> {
        match probe.query_row([from], |r| r.get::<_, i64>(0)) {
            Ok(n) if n > last => Some(Some(n)),
            Ok(_) => None,
            Err(rusqlite::Error::QueryReturnedNoRows) => Some(None),
            Err(_) => None,
        }
    };
    // `floor` itself was copied (or is the 0 floor, never a row): the
    // first key worth asking is the one after it.
    let mut failed_at = floor;
    let mut step: i64 = 1;
    let (mut hi, mut found) = loop {
        let from = floor.saturating_add(step);
        match ask(from) {
            Some(Some(n)) => break (from, n),
            // Nothing readable past `from`; a bigger step cannot find more.
            Some(None) => return None,
            None => {}
        }
        if from == i64::MAX {
            return None;
        }
        failed_at = from;
        step = step.saturating_mul(2);
    };
    // Narrow (failed_at, hi]: the smallest probe that still answers.
    while hi - failed_at > 1 {
        let mid = failed_at + (hi - failed_at) / 2;
        match ask(mid) {
            Some(Some(n)) => {
                hi = mid;
                found = n;
            }
            // A probe in the middle that finds NOTHING past it while a
            // larger one found a row is the tree contradicting itself;
            // keep the row already found.
            Some(None) => break,
            None => failed_at = mid,
        }
    }
    Some(found)
}

/// After the copy, rows whose foreign key points at a row the damage
/// took: the reference is cleared, or the row removed when the column
/// cannot be empty. The engine's own check lists them.
fn detach_dangling_references(dst: &Connection, report: &mut [TableRebuild]) -> Result<(), VaultError> {
    let violations: Vec<(String, i64, i64)> = {
        let mut stmt = dst.prepare("PRAGMA foreign_key_check")?;
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(3)?)))?
            .collect::<Result<_, _>>()?
    };
    for (table, rowid, fk_id) in violations {
        let column: Option<(String, bool)> = {
            let mut stmt = dst.prepare(&format!("PRAGMA foreign_key_list({})", quote_ident(&table)))?;
            let from = stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(3)?)))?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .find(|(id, _)| *id == fk_id)
                .map(|(_, col)| col);
            match from {
                Some(col) => {
                    let not_null = table_columns(dst, &table)?
                        .iter()
                        .any(|c| c.name == col && c.not_null);
                    Some((col, not_null))
                }
                None => None,
            }
        };
        let t = quote_ident(&table);
        match column {
            Some((col, false)) => {
                dst.execute(&format!("UPDATE {t} SET {} = NULL WHERE rowid = ?1", quote_ident(&col)), [rowid])?;
            }
            _ => {
                dst.execute(&format!("DELETE FROM {t} WHERE rowid = ?1"), [rowid])?;
            }
        }
        if let Some(entry) = report.iter_mut().find(|e| e.name == table) {
            entry.detached += 1;
        }
    }
    Ok(())
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(suffix);
    path.with_file_name(name)
}

/// The three files SQLite keeps for one database.
fn with_siblings(path: &Path) -> [PathBuf; 3] {
    [path.to_path_buf(), sibling(path, "-wal"), sibling(path, "-shm")]
}

fn remove_with_siblings(path: &Path) -> Result<(), VaultError> {
    for p in with_siblings(path) {
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(VaultError::Io(e)),
        }
    }
    Ok(())
}

fn rename_with_siblings(from: &Path, to: &Path) -> Result<(), VaultError> {
    let from = with_siblings(from);
    let to = with_siblings(to);
    for (f, t) in from.iter().zip(to.iter()) {
        if f.exists() {
            std::fs::rename(f, t)?;
        }
    }
    Ok(())
}

fn stamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}
