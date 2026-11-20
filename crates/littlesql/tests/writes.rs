//! Write-path tests: random operations checked against a simple model,
//! with the integrity check run along the way.

use littlesql::{Database, Value};
use std::collections::BTreeMap;

/// A small deterministic PRNG (xorshift), so failures reproduce.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn check(db: &Database) {
    let rs = db.query("PRAGMA integrity_check").unwrap();
    let msgs: Vec<String> = rs.rows.iter().map(|r| r[0].to_text()).collect();
    assert_eq!(msgs, ["ok"], "integrity check failed");
}

fn text_of_len(rng: &mut Rng, n: usize) -> String {
    (0..n)
        .map(|_| (b'a' + rng.below(26) as u8) as char)
        .collect()
}

#[test]
fn random_rowid_table_with_indexes() {
    let db = Database::memory();
    db.execute(
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT COLLATE NOCASE, n INT, body TEXT);
         CREATE INDEX t_name ON t(name DESC, n);
         CREATE INDEX t_n ON t(n);",
    )
    .unwrap();
    let mut model: BTreeMap<i64, (String, i64, String)> = BTreeMap::new();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for step in 0..3000 {
        let id = rng.below(800) as i64 + 1;
        match rng.below(10) {
            0..=5 => {
                let len = 1 + rng.below(12) as usize;
                let name = text_of_len(&mut rng, len);
                let n = rng.below(50) as i64;
                // Some bodies are long enough to need overflow pages.
                let len = if rng.below(8) == 0 {
                    2000 + rng.below(9000)
                } else {
                    rng.below(200)
                };
                let body = text_of_len(&mut rng, len as usize);
                db.execute(&format!(
                    "INSERT OR REPLACE INTO t VALUES ({id}, '{name}', {n}, '{body}')"
                ))
                .unwrap();
                model.insert(id, (name, n, body));
            }
            6 | 7 => {
                db.execute(&format!("DELETE FROM t WHERE id = {id}"))
                    .unwrap();
                model.remove(&id);
            }
            _ => {
                let n = rng.below(50) as i64;
                db.execute(&format!("UPDATE t SET n = {n} WHERE id = {id}"))
                    .unwrap();
                if let Some(r) = model.get_mut(&id) {
                    r.1 = n;
                }
            }
        }
        if step % 500 == 499 {
            check(&db);
            let rs = db.query("SELECT id, name, n, body FROM t").unwrap();
            assert_eq!(rs.rows.len(), model.len());
            for (row, (id, (name, n, body))) in rs.rows.iter().zip(&model) {
                assert_eq!(row[0], Value::Int(*id));
                assert_eq!(row[1].to_text(), *name);
                assert_eq!(row[2], Value::Int(*n));
                assert_eq!(row[3].to_text(), *body);
            }
            // Index-driven queries agree with the model too.
            for probe in [0i64, 7, 25, 49] {
                let rs = db
                    .query(&format!("SELECT count(*) FROM t WHERE n = {probe}"))
                    .unwrap();
                let want = model.values().filter(|r| r.1 == probe).count() as i64;
                assert_eq!(rs.rows[0][0], Value::Int(want));
            }
        }
    }
    // Delete everything through the index path, then check emptiness.
    db.execute("DELETE FROM t WHERE n >= 0").unwrap();
    check(&db);
    assert_eq!(
        db.query("SELECT count(*) FROM t").unwrap().rows[0][0],
        Value::Int(0)
    );
}

#[test]
fn random_without_rowid_table() {
    let db = Database::memory();
    db.execute(
        "CREATE TABLE w(a TEXT, b INT, c, PRIMARY KEY(a, b DESC)) WITHOUT ROWID;
         CREATE INDEX w_c ON w(c);",
    )
    .unwrap();
    let mut model: BTreeMap<(String, i64), i64> = BTreeMap::new();
    let mut rng = Rng(42);
    for _ in 0..2000 {
        let a = format!("k{}", rng.below(30));
        let b = rng.below(40) as i64;
        if rng.below(3) == 0 {
            db.execute(&format!("DELETE FROM w WHERE a = '{a}' AND b = {b}"))
                .unwrap();
            model.remove(&(a, b));
        } else {
            let c = rng.below(1000) as i64;
            db.execute(&format!(
                "INSERT OR REPLACE INTO w VALUES ('{a}', {b}, {c})"
            ))
            .unwrap();
            model.insert((a, b), c);
        }
    }
    check(&db);
    let rs = db.query("SELECT a, b, c FROM w ORDER BY a, b").unwrap();
    let want: Vec<(String, i64, i64)> = model
        .iter()
        .map(|((a, b), c)| (a.clone(), *b, *c))
        .collect();
    let got: Vec<(String, i64, i64)> = rs
        .rows
        .iter()
        .map(|r| (r[0].to_text(), r[1].to_int(), r[2].to_int()))
        .collect();
    assert_eq!(got, want);
    let rs = db.query("SELECT count(*) FROM w WHERE c < 500").unwrap();
    let n = model.values().filter(|c| **c < 500).count() as i64;
    assert_eq!(rs.rows[0][0], Value::Int(n));
}

#[test]
fn constraints_and_conflicts() {
    let db = Database::memory();
    db.execute(
        "CREATE TABLE u(id INTEGER PRIMARY KEY, email TEXT UNIQUE NOT NULL, v INT DEFAULT 7)",
    )
    .unwrap();
    db.execute("INSERT INTO u(email) VALUES ('a'), ('b')")
        .unwrap();
    let err = db.execute("INSERT INTO u(email) VALUES ('a')").unwrap_err();
    assert_eq!(err.to_string(), "UNIQUE constraint failed: u.email");
    let err = db
        .execute("INSERT INTO u(email) VALUES (NULL)")
        .unwrap_err();
    assert_eq!(err.to_string(), "NOT NULL constraint failed: u.email");
    let err = db
        .execute("INSERT INTO u(id, email) VALUES (1, 'c')")
        .unwrap_err();
    assert_eq!(err.to_string(), "UNIQUE constraint failed: u.id");
    let err = db
        .execute("INSERT INTO u(id, email) VALUES ('x', 'c')")
        .unwrap_err();
    assert_eq!(err.to_string(), "datatype mismatch");
    db.execute("INSERT OR IGNORE INTO u(email) VALUES ('a'), ('c')")
        .unwrap();
    db.execute("INSERT OR REPLACE INTO u(id, email) VALUES (9, 'b')")
        .unwrap();
    let rs = db.query("SELECT id, email, v FROM u ORDER BY id").unwrap();
    let got: Vec<String> = rs
        .rows
        .iter()
        .map(|r| format!("{}|{}|{}", r[0].to_text(), r[1].to_text(), r[2].to_text()))
        .collect();
    assert_eq!(got, ["1|a|7", "3|c|7", "9|b|7"]);
    // CHECK constraints, named and unnamed; NULL passes.
    db.execute(
        "CREATE TABLE c(a INT CHECK (a > 0), b TEXT, CONSTRAINT short CHECK (length(b) < 5))",
    )
    .unwrap();
    let err = db.execute("INSERT INTO c VALUES (0, 'x')").unwrap_err();
    assert_eq!(err.to_string(), "CHECK constraint failed: a > 0");
    let err = db
        .execute("INSERT INTO c VALUES (1, 'toolong')")
        .unwrap_err();
    assert_eq!(err.to_string(), "CHECK constraint failed: short");
    db.execute(
        "INSERT INTO c VALUES (NULL, NULL); INSERT OR IGNORE INTO c VALUES (-1, 'a'), (2, 'b')",
    )
    .unwrap();
    assert!(db.execute("UPDATE c SET a = -a").is_err());
    assert_eq!(
        db.query("SELECT count(*) FROM c").unwrap().rows[0][0],
        Value::Int(2)
    );
    // A failing multi-row statement leaves nothing behind.
    let err = db.execute("INSERT INTO u(email) VALUES ('d'), ('a')");
    assert!(err.is_err());
    assert_eq!(
        db.query("SELECT count(*) FROM u").unwrap().rows[0][0],
        Value::Int(3)
    );
    check(&db);
}

#[test]
fn transactions() {
    let db = Database::memory();
    db.execute("CREATE TABLE t(x INTEGER PRIMARY KEY, y TEXT UNIQUE)")
        .unwrap();
    db.execute("BEGIN; INSERT INTO t VALUES (1, 'a'); INSERT INTO t VALUES (2, 'b')")
        .unwrap();
    // A failing statement undoes only itself.
    assert!(db.execute("INSERT INTO t VALUES (3, 'a')").is_err());
    db.execute("INSERT INTO t VALUES (4, 'd')").unwrap();
    db.execute("ROLLBACK").unwrap();
    assert_eq!(
        db.query("SELECT count(*) FROM t").unwrap().rows[0][0],
        Value::Int(0)
    );
    db.execute("BEGIN; INSERT INTO t VALUES (1, 'a')").unwrap();
    assert!(db.execute("INSERT INTO t VALUES (3, 'a')").is_err());
    db.execute("INSERT INTO t VALUES (4, 'd'); COMMIT").unwrap();
    let rs = db.query("SELECT group_concat(x) FROM t").unwrap();
    assert_eq!(rs.rows[0][0].to_text(), "1,4");
    // Schema changes roll back too.
    db.execute("BEGIN; CREATE TABLE gone(a); ROLLBACK").unwrap();
    assert!(db.query("SELECT * FROM gone").is_err());
    assert!(db.execute("COMMIT").is_err());
    check(&db);
}

#[test]
fn file_round_trip_through_the_wal() {
    let dir = std::env::temp_dir().join(format!("littlesql-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.db");
    let _ = std::fs::remove_file(&path);
    {
        let db = Database::open(&path).unwrap();
        db.execute("CREATE TABLE t(a INTEGER PRIMARY KEY, b TEXT); CREATE INDEX tb ON t(b)")
            .unwrap();
        for i in 0..2000 {
            db.execute(&format!("INSERT INTO t(b) VALUES ('row {i}')"))
                .unwrap();
        }
        db.execute("DELETE FROM t WHERE a % 3 = 0").unwrap();
        // Dropping the handle checkpoints the WAL into the file.
    }
    let wal = dir.join("t.db-wal");
    assert!(!wal.exists() || std::fs::metadata(&wal).unwrap().len() == 0);
    let db = Database::open_readonly(&path).unwrap();
    check(&db);
    let rs = db.query("SELECT count(*), max(a) FROM t").unwrap();
    assert_eq!(rs.rows[0][0], Value::Int(1334));
    assert_eq!(rs.rows[0][1], Value::Int(2000));
    let rs = db.query("PRAGMA journal_mode").unwrap();
    assert_eq!(rs.rows[0][0].to_text(), "wal");
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn drop_frees_pages_for_reuse() {
    let db = Database::memory();
    db.execute("CREATE TABLE big(x)").unwrap();
    db.execute("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 3000) INSERT INTO big SELECT printf('%0200d', i) FROM n")
        .unwrap();
    let pages = db.query("PRAGMA page_count").unwrap().rows[0][0].to_int();
    db.execute("DROP TABLE big").unwrap();
    check(&db);
    db.execute("CREATE TABLE again(x)").unwrap();
    db.execute("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 3000) INSERT INTO again SELECT printf('%0200d', i) FROM n")
        .unwrap();
    let after = db.query("PRAGMA page_count").unwrap().rows[0][0].to_int();
    assert!(
        after <= pages + 2,
        "freed pages were not reused: {pages} -> {after}"
    );
    check(&db);
}

#[test]
fn deep_tree_survives_range_deletes() {
    // Enough rows for a three-level table b-tree and index, then deletes
    // that empty whole interior pages in several places.
    let db = Database::memory();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, pad TEXT); CREATE INDEX tp ON t(pad)")
        .unwrap();
    db.execute("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 20000) INSERT INTO t SELECT i, printf('%0300d', i) FROM n")
        .unwrap();
    check(&db);
    for (lo, hi) in [(1, 6000), (9000, 15000), (16000, 19990)] {
        db.execute(&format!("DELETE FROM t WHERE id BETWEEN {lo} AND {hi}"))
            .unwrap();
        check(&db);
    }
    let rs = db
        .query("SELECT count(*), min(id), max(id) FROM t")
        .unwrap();
    assert_eq!(rs.rows[0][0], Value::Int(20000 - 6000 - 6001 - 3991));
    db.execute("DELETE FROM t WHERE id > 0").unwrap();
    check(&db);
}

#[test]
fn empty_database_reads_as_empty() {
    let db = Database::memory();
    let rs = db.query("SELECT count(*) FROM sqlite_schema").unwrap();
    assert_eq!(rs.rows[0][0], Value::Int(0));
    check(&db);
    assert!(db.query("SELECT * FROM nothing").is_err());
}

#[test]
fn savepoints_and_alter_table() {
    let db = Database::memory();
    db.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, a TEXT, b INT); CREATE INDEX tb ON t(b)")
        .unwrap();
    db.execute("SAVEPOINT s; INSERT INTO t(a, b) VALUES ('x', 1); SAVEPOINT s2; INSERT INTO t(a, b) VALUES ('y', 2); ROLLBACK TO s2; RELEASE s")
        .unwrap();
    assert_eq!(
        db.query("SELECT count(*) FROM t").unwrap().rows[0][0],
        Value::Int(1)
    );
    db.execute("ALTER TABLE t ADD COLUMN c TEXT DEFAULT 'd'; ALTER TABLE t RENAME COLUMN b TO bb; ALTER TABLE t RENAME TO u")
        .unwrap();
    let rs = db.query("SELECT id, a, bb, c FROM u WHERE bb = 1").unwrap();
    assert_eq!(rs.rows[0][3].to_text(), "d");
    let sql = db
        .query("SELECT sql FROM sqlite_schema WHERE name = 'tb'")
        .unwrap();
    assert_eq!(sql.rows[0][0].to_text(), "CREATE INDEX tb ON \"u\"(bb)");
    assert!(db.execute("ALTER TABLE u DROP COLUMN bb").is_err());
    db.execute("ALTER TABLE u DROP COLUMN a").unwrap();
    let rs = db.query("SELECT * FROM u").unwrap();
    assert_eq!(rs.columns, ["id", "bb", "c"]);
    check(&db);
}

#[test]
fn raise_in_triggers() {
    let db = Database::memory();
    db.execute(
        "CREATE TABLE a(id INTEGER PRIMARY KEY, bal INT);
         CREATE TRIGGER neg BEFORE UPDATE ON a WHEN NEW.bal < 0 BEGIN SELECT RAISE(ABORT, 'negative'); END;
         CREATE TRIGGER zero BEFORE INSERT ON a WHEN NEW.bal = 0 BEGIN SELECT RAISE(IGNORE); END;
         CREATE TRIGGER big BEFORE INSERT ON a WHEN NEW.bal > 99 BEGIN SELECT RAISE(FAIL, 'big'); END;",
    )
    .unwrap();
    db.execute("INSERT INTO a(bal) VALUES (5), (0), (7)")
        .unwrap();
    let sum =
        |db: &Database| db.query("SELECT group_concat(bal) FROM a").unwrap().rows[0][0].to_text();
    assert_eq!(sum(&db), "5,7");
    assert_eq!(
        db.execute("UPDATE a SET bal = bal - 6")
            .unwrap_err()
            .to_string(),
        "negative"
    );
    assert_eq!(sum(&db), "5,7");
    // FAIL keeps the rows the statement had already inserted.
    assert!(db
        .execute("INSERT INTO a(bal) VALUES (8), (100), (9)")
        .is_err());
    assert_eq!(sum(&db), "5,7,8");
    assert!(db.query("SELECT RAISE(ABORT, 'x')").is_err());
    check(&db);
}
