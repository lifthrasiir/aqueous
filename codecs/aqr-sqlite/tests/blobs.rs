//! Where the blobs of a database's records are: inline, over overflow pages, in tables and in
//! indexes, as pieces of the file that give the blob's bytes back.

#[allow(dead_code)]
mod common;

use aqr_sqlite::blobs::{self, KEY};
use common::build;

const N: usize = 400;

/// A table of `N` records of three integers and a blob of `2 + 2 n` bytes (`MG`, then hex digits;
/// `n` up to 1500, so it is over overflow pages on 1024-byte pages), some of other prefixes or
/// too short; an index of the first integer and the blob.
fn database() -> Vec<u8> {
    static DB: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    DB.get_or_init(made).clone()
}

fn made() -> Vec<u8> {
    build(
        "blobs",
        &format!(
            "PRAGMA page_size=1024;
             CREATE TABLE t(k INTEGER, j INTEGER, v INTEGER, d BLOB, s TEXT);
             CREATE INDEX ti ON t(k, d);
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {N})
             INSERT INTO t SELECT i, -i * 1000, i % 2, CASE
                 WHEN i % 10 = 3 THEN CAST('XX' || hex(randomblob(40)) AS BLOB)
                 WHEN i % 10 = 7 THEN CAST('MG1' AS BLOB)
                 ELSE CAST('MG' || hex(randomblob(i * 7 % 1500)) AS BLOB) END, 'text' || i
             FROM n;"
        ),
    )
}

#[test]
fn finds_blobs_in_tables_and_indexes() {
    let db = database();
    let want = |h: &[u8]| h.starts_with(b"MG");
    let found = blobs::blobs(&db, 8, &want).expect("a database");
    // the table's and the index's: every record but those of other prefixes and the short ones
    // (and `i * 7 % 1500` of 0 or 1 hex digits makes a blob of under 8 bytes)
    let expected: Vec<usize> = (1..=N).filter(|i| i % 10 != 3 && i % 10 != 7 && (i * 7 % 1500) * 2 + 2 >= 8).collect();
    assert_eq!(found.len(), 2 * expected.len());
    let root_of_table = found.iter().find(|b| b.column == 3).expect("the table's blobs").root;
    let (mut table, mut index): (Vec<_>, Vec<_>) = found.iter().partition(|b| b.root == root_of_table);
    assert_eq!((table.len(), index.len()), (expected.len(), expected.len()));
    assert!(index.iter().all(|b| b.root != root_of_table && b.root == index[0].root));
    // the table's: each blob's key the record's integers
    table.sort_by_key(|b| b.key[0]);
    for (b, &i) in table.iter().zip(&expected) {
        assert_eq!(b.column, 3);
        assert_eq!(b.key, [i as i64, -(i as i64) * 1000, (i % 2) as i64]);
        let bytes = b.bytes(&db);
        assert_eq!(bytes.len(), b.len());
        assert_eq!(bytes.len(), 2 + 2 * (i * 7 % 1500));
        assert!(bytes.starts_with(b"MG") && bytes[2..].iter().all(u8::is_ascii_hexdigit), "row {i}");
        assert!(!b.is_empty());
        // (blobs of over 1 KiB lie over several pieces of overflow pages)
        if bytes.len() > 2000 {
            assert!(b.pieces.len() > 1, "row {i}");
        }
    }
    // the index's: its second column the blob, the same bytes as the table's
    index.sort_by_key(|b| b.key[0]);
    assert!(index.iter().zip(&expected).all(|(b, &i)| b.column == 1 && b.key[0] == i as i64));
    let a: Vec<Vec<u8>> = table.iter().map(|b| b.bytes(&db)).collect();
    let b: Vec<Vec<u8>> = index.iter().map(|b| b.bytes(&db)).collect();
    assert_eq!(a, b);
    // no blob of the other prefix; a larger minimum drops the smaller
    assert!(blobs::blobs(&db, 8, &|h: &[u8]| h.starts_with(b"XX")).unwrap().len() >= 2 * (N / 10));
    let big = blobs::blobs(&db, 1500, &want).unwrap();
    assert!(!big.is_empty() && big.len() < found.len() && big.iter().all(|b| b.len() >= 1500));
    assert_eq!(KEY, 3);
}

#[test]
fn samples_some_pages() {
    let db = database();
    let want = |h: &[u8]| h.starts_with(b"MG");
    let all = blobs::blobs(&db, 8, &want).unwrap();
    let (few, walked) = blobs::sample(&db, 8, &want, 12).unwrap();
    assert_eq!(walked, 12);
    assert!(!few.is_empty() && few.len() < all.len());
    assert_eq!(few[..], all[..few.len()]);
    let (every, walked) = blobs::sample(&db, 8, &want, usize::MAX).unwrap();
    assert_eq!(every, all);
    assert!(walked > 12);
}

#[test]
fn not_a_database_or_damaged() {
    let want = |_: &[u8]| true;
    assert!(blobs::blobs(b"not a database at all", 1, &want).is_none());
    assert!(blobs::blobs(&[], 1, &want).is_none());
    let db = database();
    // pages damaged: whatever cannot be read is left out, nothing panics
    for seed in 0..40usize {
        let mut d = db.clone();
        for k in 0..30 {
            let at = 1024 + (seed * 7919 + k * 104_729) % (d.len() - 1024);
            d[at] ^= (seed + k) as u8 | 1;
        }
        let _ = blobs::blobs(&d, 1, &want);
    }
    // the lengths of the records cut by the file's end
    let cut = &db[..db.len() - 1024 * 3];
    let _ = blobs::blobs(cut, 1, &want);
}
