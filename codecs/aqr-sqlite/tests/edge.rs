//! Crafted corruption of databases (built with the sqlite3 tool, then damaged by hand): cycles
//! in the freelist and in overflow chains, page counts and sizes the header lies about, cells
//! that overlap, WAL-mode headers. Whatever `encode_with` takes evaluates back to the input, in
//! time and without an allocation the data cannot hold.

mod common;

use aqr_sqlite::formula::{self, Options};
use aqr_testkit::Refusing;
use common::{build, eval};
use std::time::Duration;

#[global_allocator]
static A: Refusing = Refusing(256 << 20);

const TIME: Duration = Duration::from_secs(20);

/// A database of a table with overflow payloads, an index, and a freelist (rows deleted).
fn base(ps: usize) -> Vec<u8> {
    // (built once: the tests run side by side, and the tool writes to a file named by the case)
    static MADE: std::sync::Mutex<Vec<(usize, Vec<u8>)>> = std::sync::Mutex::new(Vec::new());
    let mut made = MADE.lock().unwrap();
    if let Some((_, d)) = made.iter().find(|m| m.0 == ps) {
        return d.clone();
    }
    let d = made_base(ps);
    made.push((ps, d.clone()));
    d
}

fn made_base(ps: usize) -> Vec<u8> {
    build(
        &format!("edge{ps}"),
        &format!(
            "PRAGMA page_size={ps};
             CREATE TABLE t(id INTEGER PRIMARY KEY, a INTEGER, b TEXT, d BLOB);
             CREATE INDEX t_a ON t(a);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 400)
             INSERT INTO t(a, b, d) SELECT i * 7 % 100, 'name' || i, randomblob(CASE WHEN i % 9 = 0 THEN 3000 ELSE 20 END) FROM n;
             DELETE FROM t WHERE id % 4 = 1;"
        ),
    )
}

fn be32(d: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(d[at..at + 4].try_into().unwrap())
}

fn set32(d: &mut [u8], at: usize, v: u32) {
    d[at..at + 4].copy_from_slice(&v.to_be_bytes());
}

/// `db` run through both options: if there is a formula it gives `db` back.
fn run(db: &[u8], name: &str) {
    for verify in [true, false] {
        let r = aqr_testkit::within(TIME, || {
            aqr_testkit::case(name);
            let f = formula::encode_with(db, &Options { verify, threads: 1 })?;
            Some(eval(&f, 1).map(|o| o == db))
        });
        match r {
            Ok(Some(Ok(same))) => assert!(same, "{name}, verify {verify}: the formula gives other bytes"),
            // (a formula that does not evaluate is only right where it was not verified)
            Ok(Some(Err(e))) => assert!(!verify, "{name}: a verified formula fails: {e}"),
            Ok(None) => {}
            Err(e) => panic!("{name}, verify {verify}: {e}"),
        }
    }
}

#[test]
fn the_base_is_taken() {
    for ps in [512, 1024, 4096] {
        let db = base(ps);
        assert!(be32(&db, 36) > 0, "a freelist");
        let f = formula::encode(&db, 1).expect("a formula");
        assert!(eval(&f, 1).unwrap() == db);
    }
}

/// The first four bytes of each page (a trunk's next, an overflow page's next, a b-tree page's
/// type and flags) made to point at itself, the first page, the last and past it.
#[test]
fn pages_pointing_around() {
    let db = base(512);
    let n = db.len() / 512;
    for p in 2..=n {
        for (what, v) in [
            ("itself", p as u32),
            ("page 1", 1),
            ("the next", p as u32 + 1),
            ("past", n as u32 + 1),
            ("all ones", u32::MAX),
            ("zero", 0),
        ] {
            let mut d = db.clone();
            set32(&mut d, (p - 1) * 512, v);
            run(&d, &format!("page {p} points to {what}"));
        }
    }
}

/// The freelist: its head, its length, a trunk's count of leaves, trunks in a ring.
#[test]
fn freelist_lies() {
    let db = base(512);
    let n = db.len() / 512;
    let trunk = be32(&db, 32) as usize;
    assert!(trunk >= 2 && trunk <= n);
    let at = (trunk - 1) * 512;
    for v in [0, 1, trunk as u32, n as u32, n as u32 + 1, u32::MAX] {
        let mut d = db.clone();
        set32(&mut d, 32, v);
        run(&d, &format!("freelist head {v}"));
        let mut d = db.clone();
        set32(&mut d, 36, v);
        run(&d, &format!("freelist count {v}"));
        let mut d = db.clone();
        set32(&mut d, at, v);
        run(&d, &format!("trunk's next {v}"));
        let mut d = db.clone();
        set32(&mut d, at + 4, v);
        run(&d, &format!("trunk's leaves {v}"));
        let mut d = db.clone();
        set32(&mut d, at + 8, v);
        run(&d, &format!("trunk's first leaf {v}"));
    }
    // two trunks of one another
    let mut d = db.clone();
    let other = if trunk < n { trunk + 1 } else { trunk - 1 };
    set32(&mut d, at, other as u32);
    set32(&mut d, (other - 1) * 512, trunk as u32);
    run(&d, "trunks in a ring");
}

/// The overflow chains: each cell's first page and every page's next made a cycle.
#[test]
fn overflow_cycles() {
    let db = base(512);
    let n = db.len() / 512;
    // the overflow pages: those the encoder takes, found as the pages of no b-tree type whose
    // first four bytes are a page number or zero and that no freelist page is
    let free = be32(&db, 36);
    assert!(free > 0);
    let mut found = 0;
    for p in 2..=n {
        let t = db[(p - 1) * 512];
        if matches!(t, 2 | 5 | 10 | 13) {
            continue;
        }
        found += 1;
        for v in [p as u32, 2, n as u32, (p as u32).saturating_sub(1)] {
            let mut d = db.clone();
            set32(&mut d, (p - 1) * 512, v);
            run(&d, &format!("chain of page {p} goes to {v}"));
        }
    }
    assert!(found > 5);
}

/// The header's sizes: page size (all powers of two and others), reserved bytes, the count of
/// pages, the format numbers, the encoding, a file that is not a whole number of pages.
#[test]
fn header_lies() {
    let db = base(1024);
    for ps in [0u16, 1, 2, 3, 256, 511, 512, 513, 1024, 2048, 4096, 8192, 16384, 32768, 32769, 40000, u16::MAX] {
        let mut d = db.clone();
        d[16..18].copy_from_slice(&ps.to_be_bytes());
        run(&d, &format!("page size {ps}"));
    }
    for r in [0u8, 1, 8, 32, 255, 200, 100] {
        let mut d = db.clone();
        d[20] = r;
        run(&d, &format!("reserved {r}"));
    }
    for v in [0, 1, 2, 3, 100, u32::MAX, (db.len() / 1024) as u32 + 1, 1 << 31] {
        let mut d = db.clone();
        set32(&mut d, 28, v);
        run(&d, &format!("page count {v}"));
    }
    for at in [44, 56, 60, 64, 68, 96] {
        for v in [0, 1, 2, 3, 4, 5, 255, u32::MAX] {
            let mut d = db.clone();
            set32(&mut d, at, v);
            run(&d, &format!("{v} at {at}"));
        }
    }
    // the file's length: a page short, a byte over, 512 bytes over, the first page alone
    for len in [db.len() - 1024, db.len() - 1, db.len() + 1, db.len() + 512, 1024, 512, 511, 100] {
        let mut d = db.clone();
        if len <= d.len() {
            d.truncate(len);
        } else {
            d.resize(len, 0xa5);
        }
        run(&d, &format!("length {len}"));
    }
}

/// A database of WAL mode (versions 2 at 18 and 19), with a header of a WAL after it, and the
/// WAL's own bytes in place of the database.
#[test]
fn wal_headers() {
    let db = base(1024);
    for (w, r) in [(2u8, 2u8), (2, 1), (1, 2), (3, 3), (255, 255), (0, 0)] {
        let mut d = db.clone();
        d[18] = w;
        d[19] = r;
        run(&d, &format!("versions {w} {r}"));
    }
    // a WAL file's header, then frames: not a database
    let mut wal = Vec::new();
    wal.extend(0x377f0682u32.to_be_bytes());
    wal.extend(3007000u32.to_be_bytes());
    wal.extend(1024u32.to_be_bytes());
    wal.extend([0; 20]);
    wal.extend(&db[100..4096]);
    run(&wal, "a WAL file");
    // the WAL's magic and header over the start of a database
    let mut d = db.clone();
    d[..4].copy_from_slice(&0x377f0683u32.to_be_bytes());
    run(&d, "a WAL magic");
    // a WAL after the database: trailing bytes
    let mut d = db.clone();
    d.extend(&wal);
    run(&d, "a WAL after");
}

/// Cells that overlap or point out of the page, counts of cells past the page, the content
/// start before the pointers, on every b-tree page.
#[test]
fn cells_overlap() {
    let db = base(512);
    let n = db.len() / 512;
    for p in 1..=n {
        let h = if p == 1 { 100 } else { 0 };
        let at = (p - 1) * 512 + h;
        let t = db[at];
        if !matches!(t, 2 | 5 | 10 | 13) {
            continue;
        }
        let hl = if t == 2 || t == 5 { 12 } else { 8 };
        let cells = u16::from_be_bytes([db[at + 3], db[at + 4]]) as usize;
        if cells < 2 {
            continue;
        }
        let ptr = at + hl;
        // the first pointer at the second's cell, the last at the first's
        let mut d = db.clone();
        let (a, b) = (d[ptr + 2], d[ptr + 3]);
        d[ptr] = a;
        d[ptr + 1] = b;
        run(&d, &format!("page {p}: two cells of one place"));
        // a cell shifted by a byte
        let mut d = db.clone();
        d[ptr + 1] = d[ptr + 1].wrapping_add(1);
        run(&d, &format!("page {p}: a cell one byte on"));
        // a pointer into the pointers, to the page's end, past it
        for v in [hl as u16, (ptr - (p - 1) * 512) as u16, 511, 512, 513, 0xffff, 0] {
            let mut d = db.clone();
            d[ptr..ptr + 2].copy_from_slice(&v.to_be_bytes());
            run(&d, &format!("page {p}: pointer {v}"));
        }
        for v in [0u16, 1, 2, 100, 0x7fff, 0xffff] {
            let mut d = db.clone();
            d[at + 3..at + 5].copy_from_slice(&v.to_be_bytes());
            run(&d, &format!("page {p}: {v} cells"));
            let mut d = db.clone();
            d[at + 5..at + 7].copy_from_slice(&v.to_be_bytes());
            run(&d, &format!("page {p}: content start {v}"));
            let mut d = db.clone();
            d[at + 1..at + 3].copy_from_slice(&v.to_be_bytes());
            run(&d, &format!("page {p}: freeblock {v}"));
        }
        // an interior page's right-most child and cells' children
        if hl == 12 {
            for v in [0, 1, p as u32, n as u32 + 1, u32::MAX] {
                let mut d = db.clone();
                set32(&mut d, at + 8, v);
                run(&d, &format!("page {p}: right-most child {v}"));
                let mut d = db.clone();
                let off = u16::from_be_bytes([d[ptr], d[ptr + 1]]) as usize;
                set32(&mut d, (p - 1) * 512 + off, v);
                run(&d, &format!("page {p}: first child {v}"));
            }
        }
    }
}

/// Damaged at many places: every one taken evaluates to its input.
#[test]
fn damaged_database() {
    let db = base(512);
    aqr_testkit::damaged(&db, 120, 150, TIME, |v| {
        if let Some(f) = formula::encode_with(v, &Options { verify: true, threads: 1 }) {
            assert!(eval(&f, 1).as_deref() == Ok(v), "does not round trip");
        }
    });
}
