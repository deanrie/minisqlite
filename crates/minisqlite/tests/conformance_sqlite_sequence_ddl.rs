//! Conformance battery: `DROP TABLE` and `ALTER TABLE ... RENAME TO` keep the internal
//! `sqlite_sequence(name, seq)` table in step with the AUTOINCREMENT tables it tracks
//! (autoinc.html §3, lang_droptable.html, lang_altertable.html).
//!
//! sqlite3 runs `DELETE FROM sqlite_sequence WHERE name=<table>` when it drops an
//! AUTOINCREMENT table, so a table re-created under the same name starts again from
//! rowid 1, and `UPDATE sqlite_sequence SET name=<new> WHERE name=<old>` when it renames
//! any table, so the high-water follows the table. Both match `name` exactly against the
//! table's stored name, and both live in the store the table lives in (`main`, `temp`, or
//! an attached database). Every expected value below is what sqlite3 3.51.3 returns for
//! the same statements.

mod conformance;
use conformance::*;

use minisqlite::Connection;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const AUTOINC_T: &str = "CREATE TABLE t(a INTEGER PRIMARY KEY AUTOINCREMENT, v)";
const AUTOINC_U: &str = "CREATE TABLE u(a INTEGER PRIMARY KEY AUTOINCREMENT, v)";

/// A unique on-disk path, removed (with its sidecars) on drop.
struct TempDb {
    path: PathBuf,
}

impl TempDb {
    fn new() -> TempDb {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let mut path = std::env::temp_dir();
        path.push(format!("minisqlite_seqddl_{}_{n}_{nanos}.db", std::process::id()));
        let db = TempDb { path };
        db.remove_all();
        db
    }

    fn open(&self) -> Connection {
        Connection::open(&self.path)
            .unwrap_or_else(|e| panic!("Connection::open failed for {:?}: {e:?}", self.path))
    }

    fn remove_all(&self) {
        let _ = std::fs::remove_file(&self.path);
        for suffix in ["-journal", "-wal", "-shm"] {
            let mut s = self.path.as_os_str().to_os_string();
            s.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(s));
        }
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        self.remove_all();
    }
}

// ---------------------------------------------------------------------------
// DROP TABLE
// ---------------------------------------------------------------------------

#[test]
fn drop_table_deletes_only_its_own_sequence_row() {
    let mut db = mem();
    exec(&mut db, AUTOINC_T);
    exec(&mut db, AUTOINC_U);
    exec(&mut db, "INSERT INTO t(v) VALUES (1), (2)");
    exec(&mut db, "INSERT INTO u(v) VALUES (1)");
    exec(&mut db, "DROP TABLE t");
    assert_rows(&mut db, "SELECT name, seq FROM sqlite_sequence", &[vec![text("u"), int(1)]]);
}

#[test]
fn table_recreated_after_drop_restarts_at_rowid_one() {
    let mut db = mem();
    exec(&mut db, AUTOINC_T);
    exec(&mut db, "INSERT INTO t(v) VALUES (1), (2)");
    exec(&mut db, "DROP TABLE t");
    exec(&mut db, AUTOINC_T);
    exec(&mut db, "INSERT INTO t(v) VALUES (9)");
    assert_rows(&mut db, "SELECT a, v FROM t", &[vec![int(1), int(9)]]);
    assert_rows(&mut db, "SELECT name, seq FROM sqlite_sequence", &[vec![text("t"), int(1)]]);
}

#[test]
fn drop_table_matches_the_stored_name_not_the_spelling_used() {
    // The row holds the declared spelling `Tab`; `DROP TABLE TAB` still removes it.
    let mut db = mem();
    exec(&mut db, "CREATE TABLE Tab(a INTEGER PRIMARY KEY AUTOINCREMENT, v)");
    exec(&mut db, "INSERT INTO tab(v) VALUES (1), (2)");
    assert_rows(&mut db, "SELECT name, seq FROM sqlite_sequence", &[vec![text("Tab"), int(2)]]);
    exec(&mut db, "DROP TABLE TAB");
    assert_scalar(&mut db, "SELECT count(*) FROM sqlite_sequence", int(0));
}

#[test]
fn drop_table_in_rolled_back_transaction_keeps_the_sequence_row() {
    let mut db = mem();
    exec(&mut db, AUTOINC_T);
    exec(&mut db, "INSERT INTO t(v) VALUES (1), (2)");
    exec(&mut db, "BEGIN");
    exec(&mut db, "DROP TABLE t");
    assert_scalar(&mut db, "SELECT count(*) FROM sqlite_sequence", int(0));
    exec(&mut db, "ROLLBACK");
    assert_rows(&mut db, "SELECT name, seq FROM sqlite_sequence", &[vec![text("t"), int(2)]]);
    exec(&mut db, "DELETE FROM t");
    exec(&mut db, "INSERT INTO t(v) VALUES (3)");
    assert_scalar(&mut db, "SELECT a FROM t", int(3));
}

#[test]
fn drop_temp_table_deletes_only_the_temp_sequence_row() {
    let mut db = mem();
    exec(&mut db, AUTOINC_T);
    exec(&mut db, "INSERT INTO t(v) VALUES (1)");
    exec(&mut db, "CREATE TEMP TABLE t(a INTEGER PRIMARY KEY AUTOINCREMENT, v)");
    exec(&mut db, "INSERT INTO temp.t(v) VALUES (1), (2), (3)");
    exec(&mut db, "DROP TABLE temp.t");
    assert_rows(&mut db, "SELECT name, seq FROM main.sqlite_sequence", &[vec![text("t"), int(1)]]);
    assert_scalar(&mut db, "SELECT count(*) FROM temp.sqlite_sequence", int(0));
    exec(&mut db, "CREATE TEMP TABLE t(a INTEGER PRIMARY KEY AUTOINCREMENT, v)");
    exec(&mut db, "INSERT INTO temp.t(v) VALUES (9)");
    assert_scalar(&mut db, "SELECT a FROM temp.t", int(1));
}

#[test]
fn drop_table_in_attached_database_deletes_only_that_sequence_row() {
    let main_file = TempDb::new();
    let aux_file = TempDb::new();
    let mut db = main_file.open();
    exec(&mut db, &format!("ATTACH '{}' AS aux", aux_file.path.display()));
    exec(&mut db, AUTOINC_T);
    exec(&mut db, "INSERT INTO t(v) VALUES (1)");
    exec(&mut db, "CREATE TABLE aux.t(a INTEGER PRIMARY KEY AUTOINCREMENT, v)");
    exec(&mut db, "INSERT INTO aux.t(v) VALUES (1), (2)");
    exec(&mut db, "DROP TABLE aux.t");
    assert_scalar(&mut db, "SELECT count(*) FROM aux.sqlite_sequence", int(0));
    assert_rows(&mut db, "SELECT name, seq FROM main.sqlite_sequence", &[vec![text("t"), int(1)]]);
}

#[test]
fn drop_table_removal_is_durable_across_reopen() {
    let file = TempDb::new();
    {
        let mut db = file.open();
        exec(&mut db, AUTOINC_T);
        exec(&mut db, AUTOINC_U);
        exec(&mut db, "INSERT INTO t(v) VALUES (1), (2)");
        exec(&mut db, "INSERT INTO u(v) VALUES (1)");
        exec(&mut db, "DROP TABLE t");
    }
    let mut db = file.open();
    assert_rows(&mut db, "SELECT name, seq FROM sqlite_sequence", &[vec![text("u"), int(1)]]);
    exec(&mut db, AUTOINC_T);
    exec(&mut db, "INSERT INTO t(v) VALUES (9)");
    assert_scalar(&mut db, "SELECT a FROM t", int(1));
}

#[test]
fn drop_table_without_autoincrement_keeps_a_same_named_sequence_row() {
    // sqlite3 only clears sqlite_sequence for a table declared AUTOINCREMENT.
    let mut db = mem();
    exec(&mut db, "CREATE TABLE p(a INTEGER PRIMARY KEY, v)");
    exec(&mut db, AUTOINC_T);
    exec(&mut db, "INSERT INTO sqlite_sequence VALUES ('p', 7)");
    exec(&mut db, "DROP TABLE p");
    assert_rows(&mut db, "SELECT name, seq FROM sqlite_sequence", &[vec![text("p"), int(7)]]);
}

// ---------------------------------------------------------------------------
// ALTER TABLE ... RENAME TO
// ---------------------------------------------------------------------------

#[test]
fn rename_table_renames_its_sequence_row() {
    let mut db = mem();
    exec(&mut db, AUTOINC_T);
    exec(&mut db, "INSERT INTO t(v) VALUES (1), (2)");
    exec(&mut db, "ALTER TABLE t RENAME TO u");
    assert_rows(&mut db, "SELECT name, seq FROM sqlite_sequence", &[vec![text("u"), int(2)]]);
    exec(&mut db, "INSERT INTO u(v) VALUES (3)");
    assert_rows(&mut db, "SELECT name, seq FROM sqlite_sequence", &[vec![text("u"), int(3)]]);
}

#[test]
fn renamed_table_keeps_its_high_water_after_emptying() {
    // Without the renamed row, `u` has no high-water and an emptied table reuses rowid 1.
    let mut db = mem();
    exec(&mut db, AUTOINC_T);
    exec(&mut db, "INSERT INTO t(v) VALUES (1), (2)");
    exec(&mut db, "ALTER TABLE t RENAME TO u");
    exec(&mut db, "DELETE FROM u");
    exec(&mut db, "INSERT INTO u(v) VALUES (3)");
    assert_scalar(&mut db, "SELECT a FROM u", int(3));
}

#[test]
fn rename_then_drop_leaves_no_sequence_row() {
    let mut db = mem();
    exec(&mut db, "CREATE TABLE Tab(a INTEGER PRIMARY KEY AUTOINCREMENT, v)");
    exec(&mut db, "INSERT INTO Tab(v) VALUES (1), (2)");
    exec(&mut db, "ALTER TABLE tab RENAME TO \"New\"");
    assert_rows(&mut db, "SELECT name, seq FROM sqlite_sequence", &[vec![text("New"), int(2)]]);
    exec(&mut db, "DROP TABLE NEW");
    assert_scalar(&mut db, "SELECT count(*) FROM sqlite_sequence", int(0));
}

#[test]
fn rename_in_rolled_back_transaction_keeps_the_old_name() {
    let mut db = mem();
    exec(&mut db, AUTOINC_T);
    exec(&mut db, "INSERT INTO t(v) VALUES (1), (2)");
    exec(&mut db, "BEGIN");
    exec(&mut db, "ALTER TABLE t RENAME TO u");
    assert_rows(&mut db, "SELECT name, seq FROM sqlite_sequence", &[vec![text("u"), int(2)]]);
    exec(&mut db, "ROLLBACK");
    assert_rows(&mut db, "SELECT name, seq FROM sqlite_sequence", &[vec![text("t"), int(2)]]);
}

#[test]
fn rename_updates_every_exactly_matching_row_of_any_table() {
    // sqlite3 renames sqlite_sequence rows for any table, AUTOINCREMENT or not, and
    // compares `name` case-sensitively, so `P` is left alone.
    let mut db = mem();
    exec(&mut db, "CREATE TABLE p(a INTEGER PRIMARY KEY, v)");
    exec(&mut db, AUTOINC_T);
    exec(&mut db, "INSERT INTO sqlite_sequence VALUES ('p', 7), ('P', 8)");
    exec(&mut db, "ALTER TABLE p RENAME TO p2");
    assert_rows(
        &mut db,
        "SELECT name, seq FROM sqlite_sequence ORDER BY rowid",
        &[vec![text("p2"), int(7)], vec![text("P"), int(8)]],
    );
}

#[test]
fn rename_and_drop_on_a_utf16_database() {
    // sqlite_sequence.name is stored UTF-16 here, so the match and the rewrite must use
    // the database's text encoding.
    let file = TempDb::new();
    {
        let mut db = file.open();
        let _ = db.query("PRAGMA encoding = 'UTF-16le'");
        exec(&mut db, AUTOINC_T);
        exec(&mut db, AUTOINC_U);
        exec(&mut db, "INSERT INTO t(v) VALUES (1), (2)");
        exec(&mut db, "INSERT INTO u(v) VALUES (1)");
        exec(&mut db, "ALTER TABLE t RENAME TO w");
        exec(&mut db, "DROP TABLE u");
    }
    let mut db = file.open();
    assert_rows(&mut db, "SELECT name, seq FROM sqlite_sequence", &[vec![text("w"), int(2)]]);
    exec(&mut db, "DELETE FROM w");
    exec(&mut db, "INSERT INTO w(v) VALUES (3)");
    assert_scalar(&mut db, "SELECT a FROM w", int(3));
}
