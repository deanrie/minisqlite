//! Conformance battery: `DROP TABLE` / `DROP INDEX` return every page of the dropped
//! b-tree(s) to the freelist (fileformat2 §1.5 freelist, §1.6 b-tree pages, §1.7
//! overflow pages; lang_droptable.html, lang_dropindex.html).
//!
//! Real sqlite frees the dropped object's root, interior, leaf and overflow pages in
//! the DROP statement itself, and its `PRAGMA integrity_check` reports any page that is
//! neither part of a b-tree nor on the freelist as "Page N: never used". So the
//! property checked here is the one integrity_check enforces: after the DROP, every
//! page of the file (other than page 1 and pointer-map pages) is reached EXACTLY once,
//! either from a live b-tree (`sqlite_schema` roots, their children and overflow
//! chains) or from the freelist. That walk is done over the raw file bytes by
//! [`account_pages`], transcribed from the spec rather than imported from the engine,
//! so it cannot agree with the code under test by construction.
//!
//! The page counts asserted are the ones sqlite3 3.51.3 produces when it runs the same
//! DROP on the same file: dropping a database's only table (and its index) leaves
//! `freelist_count == page_count - 1`; dropping an index frees exactly the index
//! b-tree's pages; the file never shrinks (auto_vacuum NONE); and a later CREATE +
//! INSERT reuses the freed pages before the file grows.

mod conformance;
use conformance::*;

use minisqlite::{Connection, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Temp-file harness (same shape as the sibling on-disk test files).
// ---------------------------------------------------------------------------

struct TempDb {
    path: PathBuf,
}

impl TempDb {
    fn new() -> TempDb {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let mut path = std::env::temp_dir();
        path.push(format!("minisqlite_dropfl_{pid}_{n}_{nanos}.db"));
        let db = TempDb { path };
        db.remove_all();
        db
    }

    fn open(&self) -> Connection {
        Connection::open(&self.path)
            .unwrap_or_else(|e| panic!("Connection::open failed for {:?}: {e:?}", self.path))
    }

    fn read(&self) -> Vec<u8> {
        std::fs::read(&self.path).expect("read database file")
    }

    fn sidecar(&self, suffix: &str) -> PathBuf {
        let mut s = self.path.as_os_str().to_os_string();
        s.push(suffix);
        PathBuf::from(s)
    }

    fn remove_all(&self) {
        let _ = std::fs::remove_file(&self.path);
        for suffix in ["-journal", "-wal", "-shm"] {
            let _ = std::fs::remove_file(self.sidecar(suffix));
        }
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        self.remove_all();
    }
}

fn scalar_int(db: &mut Connection, sql: &str) -> i64 {
    match &query(db, sql).rows[0][0] {
        Value::Integer(i) => *i,
        other => panic!("{sql}: expected an integer, got {other:?}"),
    }
}

fn page_count(db: &mut Connection) -> i64 {
    scalar_int(db, "PRAGMA page_count")
}

fn freelist_count(db: &mut Connection) -> i64 {
    scalar_int(db, "PRAGMA freelist_count")
}

/// The `rootpage` of every b-tree object in `sqlite_schema`, by name.
fn schema_roots(db: &mut Connection) -> BTreeMap<String, u32> {
    query(
        db,
        "SELECT name, rootpage FROM sqlite_master WHERE rootpage > 0",
    )
    .rows
    .iter()
    .map(|r| match (&r[0], &r[1]) {
        (Value::Text(n), Value::Integer(p)) => (n.clone(), *p as u32),
        other => panic!("unexpected sqlite_master row {other:?}"),
    })
    .collect()
}

// ---------------------------------------------------------------------------
// Raw-file page accounting (fileformat2 §1.3 header, §1.5 freelist, §1.6 b-tree
// pages, §1.7 overflow, §1.8 ptrmap positions). Every multibyte field is big-endian.
// ---------------------------------------------------------------------------

fn be16(b: &[u8], off: usize) -> usize {
    u16::from_be_bytes([b[off], b[off + 1]]) as usize
}

fn be32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn varint(b: &[u8], off: usize) -> (u64, usize) {
    let mut v: u64 = 0;
    for i in 0..8 {
        let byte = b[off + i];
        v = (v << 7) | u64::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return (v, i + 1);
        }
    }
    ((v << 8) | u64::from(b[off + 8]), 9)
}

struct RawFile {
    bytes: Vec<u8>,
    page_size: usize,
    usable: usize,
    page_count: u32,
}

impl RawFile {
    fn parse(bytes: Vec<u8>) -> RawFile {
        let ps_field = be16(&bytes, 16);
        let page_size = if ps_field == 1 { 65_536 } else { ps_field };
        let usable = page_size - bytes[20] as usize;
        let page_count = (bytes.len() / page_size) as u32;
        RawFile {
            bytes,
            page_size,
            usable,
            page_count,
        }
    }

    fn page(&self, id: u32) -> &[u8] {
        let start = (id as usize - 1) * self.page_size;
        &self.bytes[start..start + self.page_size]
    }

    fn freelist_count(&self) -> u32 {
        be32(&self.bytes, 36)
    }

    fn is_ptrmap_page(&self, id: u32) -> bool {
        if be32(&self.bytes, 52) == 0 || id < 2 {
            return false;
        }
        let stride = (self.usable / 5 + 1) as u32;
        (id - 2).is_multiple_of(stride)
    }

    /// Local (on-page) payload bytes for a cell with total payload `p` (§1.6).
    fn local_len(&self, p: usize, table_leaf: bool) -> usize {
        let u = self.usable;
        let x = if table_leaf {
            u - 35
        } else {
            ((u - 12) * 64 / 255) - 23
        };
        if p <= x {
            return p;
        }
        let m = ((u - 12) * 32 / 255) - 23;
        let k = m + ((p - m) % (u - 4));
        if k <= x {
            k
        } else {
            m
        }
    }
}

/// Walk every live b-tree (page 1 plus each schema root) and the freelist, and return
/// how many times each page was reached. A correct file reaches every page in
/// `2..=page_count` (bar ptrmap pages) exactly once: that is integrity_check's
/// "never used" / "2nd reference" accounting.
fn page_refs(raw: &RawFile, roots: &[u32]) -> BTreeMap<u32, u32> {
    let mut refs: BTreeMap<u32, u32> = BTreeMap::new();
    let hit = |refs: &mut BTreeMap<u32, u32>, p: u32| *refs.entry(p).or_insert(0) += 1;

    let mut stack: Vec<u32> = std::iter::once(1).chain(roots.iter().copied()).collect();
    while let Some(id) = stack.pop() {
        hit(&mut refs, id);
        let page = raw.page(id);
        let h = if id == 1 { 100 } else { 0 };
        let kind = page[h];
        let interior = kind == 0x02 || kind == 0x05;
        let cells = be16(page, h + 3);
        let ptr_base = h + if interior { 12 } else { 8 };
        if interior {
            stack.push(be32(page, h + 8));
        }
        for i in 0..cells {
            let mut off = be16(page, ptr_base + 2 * i);
            if interior {
                stack.push(be32(page, off));
                off += 4;
                if kind == 0x05 {
                    continue; // table interior: (child, rowid), no payload
                }
            }
            let (p, n) = varint(page, off);
            off += n;
            if kind == 0x0d {
                off += varint(page, off).1; // table leaf rowid
            }
            let p = p as usize;
            let local = raw.local_len(p, kind == 0x0d);
            if local < p {
                let mut ovfl = be32(page, off + local);
                while ovfl != 0 {
                    hit(&mut refs, ovfl);
                    ovfl = be32(raw.page(ovfl), 0);
                }
            }
        }
    }

    for p in freelist_pages(raw) {
        hit(&mut refs, p);
    }
    refs
}

/// Every freelist page (trunks and their leaves), walked from the header (§1.5).
fn freelist_pages(raw: &RawFile) -> Vec<u32> {
    let mut out = Vec::new();
    let mut trunk = be32(&raw.bytes, 32);
    while trunk != 0 {
        out.push(trunk);
        let page = raw.page(trunk);
        let leaves = be32(page, 4) as usize;
        for i in 0..leaves {
            out.push(be32(page, 8 + 4 * i));
        }
        trunk = be32(page, 0);
    }
    out
}

/// Assert the integrity_check page accounting holds for the file at `tmp`: every page
/// is reached exactly once, and the header freelist count equals the freelist walk.
fn assert_every_page_accounted(tmp: &TempDb) {
    let mut db = tmp.open();
    let roots: Vec<u32> = schema_roots(&mut db).into_values().collect();
    drop(db);
    let raw = RawFile::parse(tmp.read());
    let refs = page_refs(&raw, &roots);
    let mut never_used = Vec::new();
    let mut twice = Vec::new();
    for p in 1..=raw.page_count {
        if raw.is_ptrmap_page(p) {
            continue;
        }
        match refs.get(&p).copied().unwrap_or(0) {
            0 => never_used.push(p),
            1 => {}
            _ => twice.push(p),
        }
    }
    assert!(
        never_used.is_empty(),
        "pages neither in a b-tree nor on the freelist (sqlite3: \"Page N: never used\"): {never_used:?}"
    );
    assert!(twice.is_empty(), "pages reached more than once: {twice:?}");
    assert_eq!(
        raw.freelist_count() as usize,
        freelist_pages(&raw).len(),
        "header freelist count (offset 36) matches the freelist walk"
    );
}

/// Pages of the b-tree rooted at `root` (root, children, overflow), from the raw file.
fn tree_page_count(tmp: &TempDb, root: u32) -> i64 {
    let raw = RawFile::parse(tmp.read());
    let mut refs = page_refs(&raw, &[root]);
    // `page_refs` always walks page 1 (the schema tree) and the freelist too; take
    // them back out so only `root`'s own tree is counted.
    let base = page_refs(&raw, &[]);
    for (p, n) in base {
        if let Some(v) = refs.get_mut(&p) {
            *v -= n;
        }
    }
    refs.values().filter(|&&n| n > 0).count() as i64
}

const ROWS_500: &str = "WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM c WHERE i<500) \
                        INSERT INTO t SELECT i, hex(zeroblob(200)) FROM c";

/// The repro shape: one table with an index over 500 ~400-byte rows (multi-level
/// table and index b-trees).
fn build_table_with_index(tmp: &TempDb) {
    let mut db = tmp.open();
    let _ = db.query("PRAGMA journal_mode=DELETE");
    exec(&mut db, "CREATE TABLE t(a, b)");
    exec(&mut db, "CREATE INDEX i ON t(b)");
    exec(&mut db, ROWS_500);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// DROP TABLE frees the table's b-tree AND its index's b-tree. With no other object
/// left, sqlite3 leaves every page but page 1 on the freelist (on a 120-page sqlite3
/// file: page_count 120, freelist_count 119).
#[test]
fn drop_table_frees_table_and_index_pages() {
    let tmp = TempDb::new();
    build_table_with_index(&tmp);
    let mut db = tmp.open();
    let before = page_count(&mut db);
    assert!(
        before > 50,
        "expected multi-level trees, got {before} pages"
    );
    assert_eq!(freelist_count(&mut db), 0);

    exec(&mut db, "DROP TABLE t");
    assert_eq!(
        page_count(&mut db),
        before,
        "auto_vacuum NONE never shrinks the file"
    );
    assert_eq!(
        freelist_count(&mut db),
        before - 1,
        "every page but page 1 is free"
    );
    drop(db);
    assert_every_page_accounted(&tmp);
}

/// DROP INDEX frees exactly the index b-tree's pages and leaves the table intact.
#[test]
fn drop_index_frees_exactly_the_index_pages() {
    let tmp = TempDb::new();
    build_table_with_index(&tmp);
    let mut db = tmp.open();
    let before = page_count(&mut db);
    let index_root = schema_roots(&mut db)["i"];
    drop(db);
    let index_pages = tree_page_count(&tmp, index_root);
    assert!(
        index_pages > 10,
        "expected a multi-level index, got {index_pages} pages"
    );

    let mut db = tmp.open();
    exec(&mut db, "DROP INDEX i");
    assert_eq!(page_count(&mut db), before);
    assert_eq!(
        freelist_count(&mut db),
        index_pages,
        "the index's pages, and only those, are free"
    );
    assert_eq!(scalar_int(&mut db, "SELECT count(*) FROM t"), 500);
    assert_eq!(scalar_int(&mut db, "SELECT sum(a) FROM t"), 500 * 501 / 2);
    drop(db);
    assert_every_page_accounted(&tmp);
}

/// A CREATE + INSERT after the DROP reuses the freed pages before the file grows
/// (sqlite3: 120 pages before and after, freelist 119 -> 84 for 300 rows).
#[test]
fn pages_freed_by_drop_are_reused_by_a_later_insert() {
    let tmp = TempDb::new();
    build_table_with_index(&tmp);
    let mut db = tmp.open();
    let before = page_count(&mut db);
    exec(&mut db, "DROP TABLE t");
    exec(&mut db, "CREATE TABLE u(x, y)");
    exec(
        &mut db,
        "WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM c WHERE i<300) \
         INSERT INTO u SELECT i, hex(zeroblob(200)) FROM c",
    );
    assert_eq!(
        page_count(&mut db),
        before,
        "the new table fits in the freed pages"
    );
    let u_root = schema_roots(&mut db)["u"];
    let free = freelist_count(&mut db);
    drop(db);
    let u_pages = tree_page_count(&tmp, u_root);
    assert_eq!(
        free,
        before - 1 - u_pages,
        "the new table's pages came off the freelist"
    );
    assert_every_page_accounted(&tmp);
}

/// ROLLBACK of a DROP restores the trees and the empty freelist.
#[test]
fn rollback_restores_dropped_pages() {
    let tmp = TempDb::new();
    build_table_with_index(&tmp);
    let mut db = tmp.open();
    let before = page_count(&mut db);
    exec(&mut db, "BEGIN");
    exec(&mut db, "DROP TABLE t");
    assert_eq!(
        freelist_count(&mut db),
        before - 1,
        "freed inside the transaction"
    );
    exec(&mut db, "ROLLBACK");
    assert_eq!(freelist_count(&mut db), 0);
    assert_eq!(page_count(&mut db), before);
    assert_eq!(scalar_int(&mut db, "SELECT count(*) FROM t"), 500);
    assert_eq!(
        scalar_int(
            &mut db,
            "SELECT count(*) FROM t WHERE b = hex(zeroblob(200))"
        ),
        500
    );

    // A rolled-back DROP INDEX inside a savepoint, too.
    exec(&mut db, "BEGIN");
    exec(&mut db, "SAVEPOINT sp");
    exec(&mut db, "DROP INDEX i");
    exec(&mut db, "ROLLBACK TO sp");
    exec(&mut db, "COMMIT");
    assert_eq!(freelist_count(&mut db), 0);
    drop(db);
    assert_every_page_accounted(&tmp);
}

/// The freelist is durable: a fresh connection sees it, and allocates from it.
#[test]
fn freed_pages_survive_reopen_and_are_reused() {
    let tmp = TempDb::new();
    build_table_with_index(&tmp);
    let before = {
        let mut db = tmp.open();
        let n = page_count(&mut db);
        exec(&mut db, "DROP TABLE t");
        n
    };
    {
        let mut db = tmp.open();
        assert_eq!(freelist_count(&mut db), before - 1);
        exec(&mut db, "CREATE TABLE again(x)");
        exec(&mut db, "INSERT INTO again VALUES (1)");
        assert_eq!(page_count(&mut db), before);
        assert_eq!(freelist_count(&mut db), before - 2);
    }
    assert_every_page_accounted(&tmp);
}

/// DROP TABLE of a WITHOUT ROWID table (its rows live in the PRIMARY KEY index
/// b-tree) with a UNIQUE auto-index, rows that spill to overflow pages, and a trigger.
/// sqlite3 frees every page of both index b-trees and the overflow chains.
#[test]
fn drop_without_rowid_table_with_autoindex_overflow_and_trigger() {
    let tmp = TempDb::new();
    let (before, keep_pages) = {
        let mut db = tmp.open();
        let _ = db.query("PRAGMA journal_mode=DELETE");
        exec(&mut db, "CREATE TABLE keep(x)");
        exec(&mut db, "INSERT INTO keep VALUES (42)");
        exec(
            &mut db,
            "CREATE TABLE w(k TEXT PRIMARY KEY, v, u UNIQUE) WITHOUT ROWID",
        );
        exec(
            &mut db,
            "CREATE TRIGGER tr AFTER DELETE ON w BEGIN INSERT INTO keep VALUES (old.u); END",
        );
        exec(
            &mut db,
            "WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM c WHERE i<300) \
             INSERT INTO w SELECT printf('k%05d', i), \
             CASE WHEN i % 20 = 0 THEN hex(zeroblob(3000)) ELSE hex(zeroblob(100)) END, i FROM c",
        );
        let n = page_count(&mut db);
        let keep_root = schema_roots(&mut db)["keep"];
        drop(db);
        (n, tree_page_count(&tmp, keep_root))
    };
    let mut db = tmp.open();
    let names = schema_roots(&mut db);
    assert!(
        names.keys().any(|n| n.starts_with("sqlite_autoindex_w")),
        "w has an auto-index: {names:?}"
    );
    exec(&mut db, "DROP TABLE w");
    assert_eq!(page_count(&mut db), before);
    assert_eq!(
        freelist_count(&mut db),
        before - 1 - keep_pages,
        "everything but page 1 and keep is free"
    );
    assert_eq!(
        scalar_int(
            &mut db,
            "SELECT count(*) FROM sqlite_master WHERE name = 'tr'"
        ),
        0
    );
    assert_eq!(scalar_int(&mut db, "SELECT x FROM keep"), 42);
    drop(db);
    assert_every_page_accounted(&tmp);
}

/// WAL mode: the frees go through the WAL like any other write, and the checkpointed
/// file is fully accounted.
#[test]
fn drop_in_wal_mode_frees_pages() {
    let tmp = TempDb::new();
    let before = {
        let mut db = tmp.open();
        let _ = db.query("PRAGMA journal_mode=WAL");
        exec(&mut db, "CREATE TABLE t(a, b)");
        exec(&mut db, "CREATE INDEX i ON t(b)");
        exec(&mut db, ROWS_500);
        let n = page_count(&mut db);
        exec(&mut db, "DROP TABLE t");
        assert_eq!(freelist_count(&mut db), n - 1);
        let _ = db.query("PRAGMA wal_checkpoint(TRUNCATE)");
        n
    };
    let mut db = tmp.open();
    assert_eq!(page_count(&mut db), before);
    assert_eq!(freelist_count(&mut db), before - 1);
    drop(db);
    assert_every_page_accounted(&tmp);
}

/// A TEMP table's pages go back to the temp store's freelist (sqlite3: temp
/// page_count 58, freelist_count 57 for this table).
#[test]
fn drop_temp_table_frees_temp_pages() {
    let mut db = mem();
    exec(&mut db, "CREATE TEMP TABLE t(a, b)");
    exec(&mut db, ROWS_500);
    let before = scalar_int(&mut db, "PRAGMA temp.page_count");
    assert!(before > 20);
    exec(&mut db, "DROP TABLE t");
    assert_eq!(scalar_int(&mut db, "PRAGMA temp.page_count"), before);
    assert_eq!(
        scalar_int(&mut db, "PRAGMA temp.freelist_count"),
        before - 1
    );
}

/// auto_vacuum INCREMENTAL keeps freed pages on the freelist until
/// `incremental_vacuum`; FULL truncates them at commit. Both match sqlite3 (INCREMENTAL
/// file of 122 pages: freelist 119 after DROP TABLE; FULL: page_count 3, freelist 0).
#[test]
fn drop_under_auto_vacuum() {
    for (mode, full) in [(2, false), (1, true)] {
        let tmp = TempDb::new();
        let before = {
            let mut db = tmp.open();
            let _ = db.query("PRAGMA journal_mode=DELETE");
            let _ = db.query(&format!("PRAGMA auto_vacuum = {mode}"));
            exec(&mut db, "CREATE TABLE keep(x)");
            exec(&mut db, "CREATE TABLE t(a, b)");
            exec(&mut db, "CREATE INDEX i ON t(b)");
            exec(&mut db, ROWS_500);
            exec(&mut db, "INSERT INTO keep VALUES (1)");
            page_count(&mut db)
        };
        let mut db = tmp.open();
        exec(&mut db, "DROP TABLE t");
        if full {
            // page 1, the ptrmap page, and keep's root.
            assert_eq!(page_count(&mut db), 3, "FULL truncates the freed pages");
            assert_eq!(freelist_count(&mut db), 0);
        } else {
            assert_eq!(page_count(&mut db), before);
            // page 1, the ptrmap page, keep's root stay; every other page is free.
            assert_eq!(freelist_count(&mut db), before - 3);
        }
        assert_eq!(scalar_int(&mut db, "SELECT x FROM keep"), 1);
        drop(db);
        assert_every_page_accounted(&tmp);
    }
}
