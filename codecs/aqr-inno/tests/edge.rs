//! Edge cases: the table beyond where it is looked for, an old table's checksum, dictionary
//! sizes at the extremes, installers cut or damaged.

use std::time::Duration;

use aqr_inno::{Table, end, split, table};
use aqr_lzma::recons;
use aqr_testkit::{Refusing, case, damaged, most};

#[global_allocator]
static ALLOC: Refusing = Refusing(256 << 20);

const LIMIT: Duration = Duration::from_secs(60);

/// The offset of the table in a made-up installer of [`filler`] bytes before it.
const AT: usize = 0x40;

fn text(k: u8) -> Vec<u8> {
    (0..4000).flat_map(|i: u32| format!("line {} of {k}\n", i % 97).into_bytes()).collect()
}

/// An installer: `before` bytes (the table after them), the chunks of `texts` with the
/// dictionary size `dict` written in each, the setup header and program.
fn installer(before: usize, texts: &[Vec<u8>], dict: u32) -> Vec<u8> {
    let data = before + 44;
    let mut d = vec![0u8; before];
    d.extend(vec![0u8; 44]);
    for t in texts {
        let alone = aqr_lzma::greedy::alone(t);
        d.extend_from_slice(b"zlb\x1a");
        d.push(alone[0]);
        d.extend_from_slice(&dict.to_le_bytes());
        d.extend_from_slice(&alone[13..]);
    }
    let header = d.len();
    d.extend_from_slice(b"Inno Setup Setup Data (6.4.2)");
    let exe = d.len();
    d.extend_from_slice(&[7; 40]);
    let t = Table { total: d.len(), exe, header, data };
    d[before..before + 44].copy_from_slice(&t.to_bytes());
    d
}

fn check(d: &[u8]) {
    case("inno");
    let _ = end(d);
    for p in split(d, 2) {
        assert_eq!(recons::join(&p.data, &p.record, 1).as_deref(), Some(&d[p.range.clone()]));
    }
    assert!(most() < 256 << 20);
}

/// The table is looked for in the first 8 MiB: one that starts within is found, one past it is
/// not (the installer is no installer: its stub would be larger than any loader).
#[test]
fn table_past_8_mib() {
    let texts = [text(1)];
    let within = installer((8 << 20) - 44, &texts, 1 << 20);
    assert!(table(&within).is_some());
    // (a table beginning at the last byte within is cut by the window: ID starts within it)
    let at = (8 << 20) - 12;
    assert!(table(&installer(at, &texts, 1 << 20)).is_some());
    assert!(table(&installer(8 << 20, &texts, 1 << 20)).is_none());
    assert!(table(&installer((8 << 20) + 5000, &texts, 1 << 20)).is_none());
    assert!(end(&installer(9 << 20, &texts, 1 << 20)).is_none());
}

/// The checksum of an old table (07) covers every byte before it, and its own bytes are the
/// checksum: any byte changed in either is not a table.
#[test]
fn old_07_crc() {
    let mut d = vec![0u8; 0x80];
    d.extend_from_slice(&[7; 100]);
    let at = d.len();
    let total = at + 40;
    let mut t = b"rDlPtS07\x87eVx".to_vec();
    for f in [total, 0x90, 1000, 0, 0xa0, 0x80] {
        t.extend((f as u32).to_le_bytes());
    }
    t.extend(crc32fast::hash(&t).to_le_bytes());
    d.extend(t);
    d[0x30..0x34].copy_from_slice(b"Inno");
    d[0x34..0x38].copy_from_slice(&(at as u32).to_le_bytes());
    d[0x38..0x3c].copy_from_slice(&(!(at as u32)).to_le_bytes());
    assert!(table(&d).is_some());
    for i in 12..40 {
        let mut e = d.clone();
        e[at + i] ^= 0x10;
        assert!(table(&e).is_none(), "byte {i}");
    }
}

/// Dictionary sizes of the chunks at the extremes (0, 1, below 4 KiB, 4 GiB - 1): no panic and
/// no allocation of the size; where the chunk is taken it is rebuilt.
#[test]
fn dictionaries() {
    let texts = [text(1), text(2)];
    for dict in [0, 1, 4095, 4096, 1 << 30, (1 << 30) + 1, u32::MAX] {
        let d = installer(AT, &texts, dict);
        check(&d);
        // (the head's dictionary is the stream's own: any value is a valid LZMA header, and
        // the chunks are taken whatever it says)
        assert_eq!(split(&d, 1).len(), 2, "dict {dict}");
    }
}

#[test]
fn cut_installers() {
    let d = installer(AT, &[text(1)], 1 << 20);
    let mut cuts: Vec<usize> = (0..0x80).collect();
    cuts.extend((0..d.len()).step_by(97));
    cuts.extend(d.len() - 70..d.len());
    for cut in cuts {
        check(&d[..cut]);
    }
}

#[test]
fn damaged_installers() {
    let d = installer(AT, &[text(1)], 1 << 20);
    damaged(&d, 60, 0x80 + 20, LIMIT, check);
}
