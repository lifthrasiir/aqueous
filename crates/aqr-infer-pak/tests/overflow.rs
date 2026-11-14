//! Sizes read from a file are not trusted to add to an offset.

use aqr_infer_pak::fields::Field;
use aqr_infer_pak::tables::Table;
use aqr_infer_pak::tiling::{Cand, Layout, members};

/// Rows of an 8-byte offset and an 8-byte size, the last of size `u64::MAX`.
#[test]
fn last_member_of_size_max() {
    let mut b = Vec::new();
    for (o, s) in [(0x20u64, 0x10u64), (0x30, u64::MAX)] {
        b.extend(o.to_le_bytes());
        b.extend(s.to_le_bytes());
    }
    let field = |pos| Field { pos, be: false, shift: 0, bits: 64 };
    let layout = Layout::Explicit { off: field(0), size: field(8), alt: None, fit: 1.0, ctrl: 1.0 };
    let table = Table { start: 0, end: 32, k: 16, score: 0.0, primary: true };
    let c = Cand { layout, table, ev: 1.0 };
    let ms = members(&b, &c, 0x40);
    assert!(ms.iter().all(|m| m.size <= 0x40), "{ms:?}");
}
