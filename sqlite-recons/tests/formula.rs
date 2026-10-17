//! Round trips through entry formulas: `formula::encode`, evaluation with this crate's module,
//! and the cases where there is no formula.

use recons_formula::{Apply, Error, Formula};
use sqlite_recons::container::Options;
use sqlite_recons::formula::{self, MODULE};
use std::path::PathBuf;
use std::process::Command;

fn tmpdir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqlr-formula-test-{}", std::process::id()));
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

fn eval(f: &Formula, threads: usize) -> Result<Vec<u8>, Error> {
    f.eval(&|a: &Apply, ins: &[&[u8]]| {
        assert_eq!(a.module, MODULE);
        formula::decode(&a.params, ins, a.len, threads).map_err(|e| Error::new(e.to_string()))
    })
}

fn check(name: &str, db: &[u8]) {
    for threads in [1, 3] {
        let f = formula::encode(db, threads).unwrap_or_else(|| panic!("{name}: no formula"));
        assert_eq!(f.len(), db.len() as u64, "{name}");
        assert!(f.applies().len() == 1 && f.streams.len() == sqlite_recons::transform::NSTREAMS, "{name}");
        assert!(eval(&f, 1).unwrap() == db, "{name}");
        assert!(eval(&f, threads).unwrap() == db, "{name}: {threads} threads");
    }
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
fn databases() {
    let churn = format!(
        "PRAGMA page_size=1024;{SCHEMA}
         UPDATE t SET d = randomblob(3000 + id % 5000) WHERE id % 11 = 0;
         UPDATE t SET b = b || hex(randomblob(400)) WHERE id % 13 = 0;
         DELETE FROM t WHERE id % 3 = 0;"
    );
    let cases = [
        ("ps512", format!("PRAGMA page_size=512;{SCHEMA}")),
        ("ps4096", format!("PRAGMA page_size=4096;{SCHEMA}")),
        ("ps65536", format!("PRAGMA page_size=65536;{SCHEMA}")),
        ("churn", churn),
        ("utf16le", format!("PRAGMA encoding='UTF-16le';{SCHEMA}")),
        ("av_full", format!("PRAGMA auto_vacuum=FULL;{SCHEMA} DELETE FROM t WHERE id % 4 = 0;")),
        ("empty", "CREATE TABLE e(x);".to_string()),
    ];
    for (name, sql) in cases {
        if let Some(db) = build(name, &sql) {
            check(name, &db);
        }
    }
}

#[test]
fn tail_and_formats() {
    let Some(db) = build("tail", SCHEMA) else { return };
    // Trailing bytes after the last page, and a header that claims an older schema format.
    let mut odd = db.clone();
    odd.extend_from_slice(b"trailing junk");
    check("tail", &odd);
    let mut old = db;
    old[44..48].copy_from_slice(&1u32.to_be_bytes());
    if let Some(f) = formula::encode(&old, 1) {
        assert!(eval(&f, 1).unwrap() == old);
    }
}

#[test]
fn not_coded() {
    let mut x = 1u32;
    let noise: Vec<u8> = (0..20000)
        .map(|_| {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            (x >> 24) as u8
        })
        .collect();
    let mut h = b"SQLite format 3\0".to_vec();
    h.extend_from_slice(&[0x10, 0, 1, 1, 0, 64, 32, 32]);
    h.resize(8192, 0xab);
    let mut cases = vec![vec![], b"SQLite format 3\0".to_vec(), vec![0; 100], vec![7; 5000], noise, h];
    if let Some(db) = build("cut", SCHEMA) {
        cases.push(db[..db.len() - 1000].to_vec());
    }
    for v in &cases {
        // Whatever the container would store, there is no formula; nothing panics.
        let stored = sqlite_recons::container::compress(v)[6] == 0;
        let f = formula::encode(v, 1);
        assert_eq!(f.is_none(), stored, "len {}", v.len());
        if let Some(f) = f {
            assert!(eval(&f, 1).unwrap() == *v);
        }
    }
    let opt = Options { verify: false, ..Default::default() };
    assert!(formula::encode_with(&[0; 700], &opt).is_none());
}

#[test]
fn corrupted_modules() {
    let Some(db) = build("corrupt", SCHEMA) else { return };
    let f = formula::encode(&db, 1).unwrap();
    let ins: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
    let params = match &f.root {
        recons_formula::Node::Apply(a) => a.params.clone(),
        _ => unreachable!(),
    };
    let len = db.len() as u64;
    assert!(formula::decode(&params, &ins, len, 1).unwrap() == db);
    assert!(formula::decode(&params, &ins[1..], len, 1).is_err());
    assert!(formula::decode(&params[..2], &ins, len, 1).is_err());
    assert!(formula::decode(&[3, 3, 0], &ins, len, 1).is_err());
    assert!(formula::decode(&params, &ins, len + 5000, 1).is_err());
    assert!(formula::decode(&params, &ins, 0, 1).is_err());
    let mut p = params.clone();
    p.push(0);
    assert!(formula::decode(&p, &ins, len, 1).is_err());
}
