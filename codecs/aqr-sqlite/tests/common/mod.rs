//! What the round-trip tests share: databases built with the sqlite3 command-line tool, and the
//! evaluation of formulas by this crate's module.

use aqr_formula::{Apply, Error, Formula};
use aqr_sqlite::formula::{self, MODULE};
use std::path::PathBuf;
use std::process::Command;

fn tmpdir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("sqlr-test-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Runs `sql` in a new database, returns its bytes (None without a sqlite3 binary).
pub fn build(name: &str, sql: &str) -> Option<Vec<u8>> {
    let path = tmpdir().join(format!("{name}.db"));
    let _ = std::fs::remove_file(&path);
    let out = Command::new("sqlite3").arg(&path).arg(sql).output().ok()?;
    assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
    let v = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    Some(v)
}

pub fn eval(f: &Formula, threads: usize) -> Result<Vec<u8>, Error> {
    f.eval(&|a: &Apply, ins: &[&[u8]]| {
        assert_eq!(a.module, MODULE);
        formula::decode(&a.params, ins, a.len, threads).map_err(|e| Error::new(e.to_string()))
    })
}
