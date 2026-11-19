//! A database over 1 GiB: the page at 0x40000000 is the lock-byte page, which SQLite never uses
//! (the pages around it are consecutive in the file, the page numbers skip one). Built by the
//! sqlite3 tool under the scratch directory; ignored for its time and disk.

#![allow(dead_code)]

mod common;

use aqr_sqlite::formula::{self, Options};
use common::eval;
use std::process::Command;

fn lock_page_database(page_size: u32) {
    let dir = std::path::PathBuf::from(r"F:\Scratch").join(format!("aqr-sqlite-large-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("big.db");
    let sql = format!(
        "PRAGMA page_size={page_size};
         CREATE TABLE t(id INTEGER PRIMARY KEY, a INTEGER, s TEXT);
         CREATE INDEX t_a ON t(a);
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1400000)
         INSERT INTO t(a, s) SELECT i * 7 % 100003, printf('%0800d', i) FROM n;"
    );
    let out = Command::new("sqlite3").arg(&path).arg(sql).output().expect("the sqlite3 tool");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let db = std::fs::read(&path).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(db.len() as u64 > 0x4000_0000 + u64::from(page_size), "{} bytes: no page past the lock byte", db.len());
    let opt = Options { verify: false, ..Default::default() };
    let f = formula::encode_with(&db, &opt).expect("a formula");
    assert!(eval(&f, 4).unwrap() == db, "the round trip differs");
}

#[test]
#[ignore = "builds a 1.2 GiB database: about a minute"]
fn past_the_lock_byte_page() {
    lock_page_database(4096);
}
