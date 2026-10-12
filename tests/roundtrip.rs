//! Round trips of databases built with the sqlite3 command-line tool (skipped if it is
//! missing), of non-databases, and of corrupted containers.

use sqlite_recons::container::{Options, Stats, compress_with, decompress_threads};
use std::path::PathBuf;
use std::process::Command;

fn tmpdir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqlr-test-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Runs `sql` in a new database, returns its bytes (None without a sqlite3 binary).
fn build(name: &str, sql: &str) -> Option<Vec<u8>> {
    let path = tmpdir().join(format!("{name}.db"));
    let _ = std::fs::remove_file(&path);
    let out = Command::new("sqlite3").arg(&path).arg(sql).output().ok()?;
    assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
    let v = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    Some(v)
}

/// Compresses without the safety nets (panics surface, no verification), checks the round
/// trip, and that the file was coded rather than stored whole. Returns the coded size.
fn check(name: &str, db: &[u8]) -> usize {
    let opt = Options { verify: false, catch: false, ..Default::default() };
    let mut st = Stats::default();
    let c = compress_with(db, &opt, &mut st);
    assert!(!st.whole, "{name}: stored whole");
    assert_eq!(sqlite_recons::decompress(&c).unwrap(), db, "{name}");
    c.len()
}

const SCHEMA: &str = "
CREATE TABLE t(id INTEGER PRIMARY KEY, a INTEGER, b TEXT, c REAL, d BLOB, e TEXT);
CREATE INDEX t_a ON t(a);
CREATE INDEX t_be ON t(b COLLATE NOCASE, e DESC);
CREATE UNIQUE INDEX t_cid ON t(c, id);
WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 3000)
INSERT INTO t(a, b, c, d, e)
  SELECT i * 7 % 1000, 'Name' || (i * 13 % 97) || CASE i % 3 WHEN 0 THEN 'x' ELSE 'X' END,
         i / 8.0 + 0.125, CASE WHEN i % 5 = 0 THEN randomblob(i % 50) END,
         CASE WHEN i % 7 = 0 THEN NULL ELSE printf('%.3f', i * 1.5) END
  FROM n;
";

#[test]
fn page_sizes_and_indexes() {
    for ps in [512, 1024, 4096, 65536] {
        let Some(db) = build(&format!("ps{ps}"), &format!("PRAGMA page_size={ps};{SCHEMA}")) else {
            return;
        };
        let c = check(&format!("ps{ps}"), &db);
        assert!(c * 5 < db.len(), "page size {ps}: {c} of {}", db.len());
    }
}

#[test]
fn overflow_churn_freelist() {
    let sql = format!(
        "PRAGMA page_size=1024;{SCHEMA}
         UPDATE t SET d = randomblob(3000 + id % 5000) WHERE id % 11 = 0;
         UPDATE t SET b = b || hex(randomblob(400)) WHERE id % 13 = 0;
         DELETE FROM t WHERE id % 3 = 0;
         INSERT INTO t(a, b) SELECT a + 1, b FROM t WHERE id % 17 = 0;"
    );
    if let Some(db) = build("churn", &sql) {
        check("churn", &db);
    }
    if let Some(db) = build("secure", &format!("PRAGMA secure_delete=1;{sql}")) {
        check("secure", &db);
    }
}

#[test]
fn autovacuum_encodings_without_rowid() {
    let cases = [
        ("av_full", format!("PRAGMA auto_vacuum=FULL;{SCHEMA} DELETE FROM t WHERE id % 4 = 0;")),
        ("av_incr", format!("PRAGMA auto_vacuum=INCREMENTAL;{SCHEMA} DELETE FROM t WHERE id % 2 = 0;")),
        ("utf16le", format!("PRAGMA encoding='UTF-16le';{SCHEMA}")),
        ("utf16be", format!("PRAGMA encoding='UTF-16be';{SCHEMA}")),
        (
            "worowid",
            "CREATE TABLE w(k TEXT PRIMARY KEY, v INTEGER, x REAL) WITHOUT ROWID;
             CREATE INDEX w_v ON w(v);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000)
             INSERT INTO w SELECT 'key' || i, i % 37, i * 0.5 FROM n;"
                .to_string(),
        ),
        (
            "fts",
            "CREATE VIRTUAL TABLE f USING fts5(body);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000)
             INSERT INTO f SELECT 'word' || (i % 101) || ' other' || (i % 7) || ' text' FROM n;"
                .to_string(),
        ),
        ("empty", "CREATE TABLE e(x);".to_string()),
        ("vacuumed", format!("{SCHEMA} DELETE FROM t WHERE id % 2 = 0; VACUUM;")),
    ];
    for (name, sql) in cases {
        if let Some(db) = build(name, &sql) {
            check(name, &db);
        }
    }
}

#[test]
fn non_databases() {
    let mut cases: Vec<Vec<u8>> = vec![vec![], b"SQLite format 3\0".to_vec(), vec![0; 100], vec![7; 5000]];
    let mut x = 1u32;
    cases.push(
        (0..20000)
            .map(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                (x >> 24) as u8
            })
            .collect(),
    );
    // A header with nonsense after it, and a database cut short.
    let mut h = b"SQLite format 3\0".to_vec();
    h.extend_from_slice(&[0x10, 0, 1, 1, 0, 64, 32, 32]);
    h.resize(8192, 0xab);
    cases.push(h);
    if let Some(db) = build("cut", SCHEMA) {
        cases.push(db[..db.len() - 1000].to_vec());
        let mut odd = db.clone();
        odd.extend_from_slice(b"trailing junk");
        cases.push(odd);
    }
    for v in cases {
        let c = sqlite_recons::compress(&v);
        assert_eq!(sqlite_recons::decompress(&c).unwrap(), v);
    }
}

#[test]
fn corrupted_containers() {
    let Some(db) = build("corrupt", SCHEMA) else { return };
    let c = sqlite_recons::compress(&db);
    let mut x = 12345u64;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for i in 0..2000 {
        let mut cc = c.clone();
        if i % 4 == 0 {
            cc.truncate((next() % cc.len() as u64) as usize);
        } else {
            for _ in 0..1 + next() % 4 {
                let j = (next() % cc.len() as u64) as usize;
                cc[j] ^= 1 << (next() % 8);
            }
        }
        // Errors are fine, panics and wrong output are not.
        if let Ok(d) = sqlite_recons::decompress(&cc) {
            assert_eq!(d, db);
        }
    }
}

#[test]
fn verification_catches_differences() {
    use sqlite_recons::transform::{self, S_BYTES, S_FILL};
    let Some(db) = build("verify", SCHEMA) else { return };
    let enc = transform::encode(&db).unwrap();
    assert!(transform::verify(&enc.geom, &db, &enc.streams));
    for s in [S_FILL, S_BYTES] {
        let mut streams = enc.streams.clone();
        let k = streams[s].len() / 2;
        streams[s][k] ^= 1;
        assert!(!transform::verify(&enc.geom, &db, &streams), "stream {s}");
    }
    let mut other = db.clone();
    let k = other.len() - 7;
    other[k] ^= 0x40;
    assert!(!transform::verify(&enc.geom, &other, &enc.streams));
    assert!(!transform::verify(&enc.geom, &db[..db.len() - 1], &enc.streams));
}

/// The same bytes whatever the number of threads, and every thread count decodes them.
#[test]
fn threads_same_bytes() {
    let sql = format!(
        "PRAGMA page_size=1024;{SCHEMA}
         CREATE TABLE u(k TEXT PRIMARY KEY, v INTEGER) WITHOUT ROWID;
         CREATE INDEX u_v ON u(v);
         INSERT INTO u SELECT 'k' || id, a FROM t;
         UPDATE t SET d = randomblob(3000 + id % 5000) WHERE id % 11 = 0;
         DELETE FROM t WHERE id % 3 = 0;"
    );
    let mut dbs = Vec::new();
    for (name, sql) in [("thr", sql.as_str()), ("thr_small", "CREATE TABLE e(x);"), ("thr_s", SCHEMA)] {
        dbs.extend(build(name, sql).map(|d| (name, d)));
    }
    if dbs.is_empty() {
        return;
    }
    // Not databases, and databases cut short (stored whole).
    dbs.push(("zeros", vec![0; 5000]));
    dbs.push(("cut", dbs[0].1[..dbs[0].1.len() - 1000].to_vec()));
    for (name, db) in &dbs {
        for verify in [true, false] {
            let one = Options { verify, threads: 1, ..Default::default() };
            let c1 = compress_with(db, &one, &mut Stats::default());
            for threads in [2, 3, 8] {
                let c = compress_with(db, &Options { threads, ..one }, &mut Stats::default());
                assert!(c == c1, "{name}: compressed with {threads} threads differs");
            }
            for threads in [1, 2, 3, 8] {
                assert_eq!(&decompress_threads(&c1, threads).unwrap(), db, "{name}: {threads} threads");
            }
        }
    }
}

/// Corrupted containers: the same outcome whatever the number of threads.
#[test]
fn threads_corrupted_containers() {
    let Some(db) = build("thr_corrupt", SCHEMA) else { return };
    let c = sqlite_recons::compress(&db);
    let mut x = 777u64;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for i in 0..600 {
        let mut cc = c.clone();
        if i % 4 == 0 {
            cc.truncate((next() % cc.len() as u64) as usize);
        } else {
            for _ in 0..1 + next() % 4 {
                let j = (next() % cc.len() as u64) as usize;
                cc[j] ^= 1 << (next() % 8);
            }
        }
        let one = decompress_threads(&cc, 1);
        for threads in [2, 3, 8] {
            assert_eq!(decompress_threads(&cc, threads), one, "iteration {i}, {threads} threads");
        }
    }
}
