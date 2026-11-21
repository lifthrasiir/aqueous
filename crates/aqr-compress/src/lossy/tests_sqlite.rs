use super::sqlite::slack;
use super::*;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A temporary directory of its own, removed at the end.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Dir {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("aqr-lossy-sqlite-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        Dir(p)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The output of the `sqlite3` command line tool on `db` given `sql`; None where there is no such tool.
fn sqlite3(db: &Path, sql: &str) -> Option<String> {
    let mut c = match Command::new("sqlite3").arg(db).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no sqlite3 on PATH: skipped");
            return None;
        }
    };
    c.stdin.take().unwrap().write_all(sql.as_bytes()).unwrap();
    let out = c.wait_with_output().unwrap();
    assert!(out.status.success(), "sqlite3 failed on {sql}");
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A database made with `pragmas` first: rows of text with an index and large blobs, a third of
/// them deleted and some updated.
fn make(dir: &Dir, name: &str, pragmas: &str) -> Option<Vec<u8>> {
    let path = dir.0.join(name);
    sqlite3(
        &path,
        &format!(
            "{pragmas}
CREATE TABLE t(id INTEGER PRIMARY KEY, a TEXT, b BLOB);
CREATE INDEX ia ON t(a);
WITH RECURSIVE c(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM c WHERE i<3000)
INSERT INTO t SELECT i, 'text row '||i||' '||hex(randomblob(20)), CASE WHEN i%10=0 THEN randomblob(1000+i*7) ELSE NULL END FROM c;
DELETE FROM t WHERE id%3=0;
UPDATE t SET a=a||'xx', b=randomblob(300) WHERE id%7=0;
INSERT INTO t VALUES (100000, 'last', randomblob(50000));
"
        ),
    )?;
    Some(std::fs::read(path).unwrap())
}

fn zeros(d: &[u8]) -> usize {
    d.iter().filter(|&&b| b == 0).count()
}

fn rewritten(x: &[u8]) -> Option<(Rewrite, Option<Vec<u8>>)> {
    rewrite(x, &Lossy::parse("slack").unwrap(), &Options::default())
}

/// R of `x`, checked as `sqlite3` reads them the same.
fn roundtrip(dir: &Dir, name: &str, pragmas: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    let x = make(dir, name, pragmas)?;
    let (w, r) = rewritten(&x).expect("a rewriter takes it");
    assert!(matches!(w, Rewrite::Made { .. }), "{w:?}");
    let r = r.unwrap();
    assert_eq!(r.len(), x.len());
    check::sqlite(&x, &r).unwrap();
    assert!(zeros(&r) > zeros(&x), "zeros {} -> {}", zeros(&x), zeros(&r));
    let rp = dir.0.join(format!("r-{name}"));
    std::fs::write(&rp, &r).unwrap();
    assert_eq!(sqlite3(&rp, "PRAGMA integrity_check;").unwrap().trim(), "ok");
    assert_eq!(sqlite3(&dir.0.join(name), ".dump").unwrap(), sqlite3(&rp, ".dump").unwrap());
    Some((x, r))
}

#[test]
fn slack_of_a_database() {
    let dir = Dir::new();
    roundtrip(&dir, "a.db", "PRAGMA secure_delete=OFF;");
}

#[test]
fn slack_of_small_pages() {
    let dir = Dir::new();
    roundtrip(&dir, "b.db", "PRAGMA page_size=1024; PRAGMA secure_delete=OFF;");
}

#[test]
fn slack_of_an_auto_vacuum_database() {
    let dir = Dir::new();
    // (freed pages are truncated away: what is left of the slack is the b-trees' and the overflow's)
    roundtrip(&dir, "c.db", "PRAGMA page_size=1024; PRAGMA auto_vacuum=FULL; PRAGMA secure_delete=OFF;");
    roundtrip(&dir, "d.db", "PRAGMA page_size=4096; PRAGMA auto_vacuum=INCREMENTAL; PRAGMA secure_delete=OFF;");
}

#[test]
fn slack_of_a_database_with_reserved_bytes() {
    let dir = Dir::new();
    let Some(x) = make(&dir, "e.db", ".filectrl reserve_bytes 8\nPRAGMA secure_delete=OFF;") else { return };
    if x[20] != 8 {
        eprintln!("reserve_bytes not taken: skipped");
        return;
    }
    let (w, r) = rewritten(&x).unwrap();
    assert!(matches!(w, Rewrite::Made { .. }));
    let r = r.unwrap();
    check::sqlite(&x, &r).unwrap();
    for p in 0..x.len() / 4096 {
        assert_eq!(x[p * 4096 + 4088..(p + 1) * 4096], r[p * 4096 + 4088..(p + 1) * 4096]);
    }
}

#[test]
fn nothing_to_zero() {
    let dir = Dir::new();
    let Some(x) = make(&dir, "f.db", "PRAGMA secure_delete=ON;") else { return };
    match rewritten(&x) {
        None => {}
        Some((w, r)) => {
            // (a stale page here and there the pragma does not clean: still checked)
            assert!(matches!(w, Rewrite::Made { .. }), "{w:?}");
            check::sqlite(&x, &r.unwrap()).unwrap();
        }
    }
    let Some(y) = make(&dir, "g.db", "PRAGMA secure_delete=OFF;") else { return };
    let (_, r) = rewritten(&y).unwrap();
    // (zeroed already: nothing more)
    assert!(rewritten(&r.unwrap()).is_none());
}

/// The pages of `d` (of `ps` bytes) of a b-tree leaf with a freeblock: where its first is.
fn freeblock(d: &[u8], ps: usize) -> usize {
    (1..d.len() / ps)
        .find_map(|p| {
            let pg = &d[p * ps..(p + 1) * ps];
            let f = u16::from_be_bytes([pg[1], pg[2]]) as usize;
            (pg[0] == 13 && f != 0).then_some(p * ps + f)
        })
        .expect("a freeblock")
}

#[test]
fn tampered() {
    let dir = Dir::new();
    let Some((x, r)) = roundtrip(&dir, "h.db", "PRAGMA secure_delete=OFF;") else { return };
    let ps = 4096;
    let fails = |f: &dyn Fn(&mut Vec<u8>)| {
        let mut t = r.clone();
        f(&mut t);
        assert_ne!(t, r);
        check::sqlite(&x, &t).expect_err("tampering found");
    };
    // the header
    fails(&|t| t[40] ^= 1);
    // a cell (the last non-zero byte of page 1 is of a schema cell), changed and zeroed
    let cell = (0..ps).rev().find(|&i| x[i] != 0).unwrap();
    fails(&|t| t[cell] ^= 1);
    fails(&|t| t[cell] = 0);
    // a freeblock's header (its size)
    let fb = freeblock(&x, ps);
    fails(&|t| t[fb + 3] ^= 0x10);
    fails(&|t| t[fb] ^= 1);
    // the freelist's first trunk: a leaf number (and its count)
    let trunk = u32::from_be_bytes(x[32..36].try_into().unwrap()) as usize;
    assert!(trunk != 0);
    fails(&|t| t[(trunk - 1) * ps + 8] ^= 1);
    fails(&|t| t[(trunk - 1) * ps + 7] ^= 1);
    // a freed byte made non-zero, where it was not zero in X nor now
    let at = (0..x.len()).find(|&i| x[i] != 0 && r[i] == 0).unwrap();
    fails(&|t| t[at] = 7);
    // the first byte of the cell content area of a leaf of the table
    let other = (1..x.len() / ps)
        .find_map(|p| {
            let c = u16::from_be_bytes([x[p * ps + 5], x[p * ps + 6]]) as usize;
            (x[p * ps] == 13 && c != 0 && x[p * ps + c] != 0).then_some(p * ps + c)
        })
        .unwrap();
    fails(&|t| t[other] ^= 1);
    // another length
    assert!(check::sqlite(&x, &r[..r.len() - ps]).is_err());
}

#[test]
fn not_a_database() {
    let o = Options::default();
    assert!(slack(b"hello", &o).is_err());
    let mut d = vec![0u8; 8192];
    d[..16].copy_from_slice(b"SQLite format 3\0");
    assert!(slack(&d, &o).is_err());
    let dir = Dir::new();
    let Some(x) = make(&dir, "i.db", "PRAGMA secure_delete=OFF;") else { return };
    // truncated: no panic, and refused
    for n in [99, 100, 4000, 4096 + 17, x.len() - 1, x.len() - 4096] {
        let _ = std::panic::catch_unwind(|| slack(&x[..n], &o)).expect("no panic");
    }
    assert!(slack(&x[..x.len() - 1], &o).is_err());
    // a page claimed twice: the first trunk's first leaf is the trunk itself
    let trunk = u32::from_be_bytes(x[32..36].try_into().unwrap()) as usize;
    let mut y = x.clone();
    y[(trunk - 1) * 4096 + 8..(trunk - 1) * 4096 + 12].copy_from_slice(&(trunk as u32).to_be_bytes());
    assert!(slack(&y, &o).is_err());
    // the freelist's count wrong
    let mut y = x.clone();
    y[36..40].copy_from_slice(&1u32.to_be_bytes());
    assert!(slack(&y, &o).is_err());
    // the size in the header wrong, valid for the change counter
    let mut y = x.clone();
    let n = u32::from_be_bytes(y[28..32].try_into().unwrap());
    y[28..32].copy_from_slice(&(n + 1).to_be_bytes());
    let cc: [u8; 4] = y[24..28].try_into().unwrap();
    y[92..96].copy_from_slice(&cc);
    assert!(slack(&y, &o).is_err());
}
