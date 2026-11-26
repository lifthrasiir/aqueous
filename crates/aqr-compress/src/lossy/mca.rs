//! Minecraft's region files (`.mca`, aqr-minecraft's [`region`]): `slack`, the bytes no chunk
//! takes zeroed in place. Minecraft reads a chunk by its location (its sectors) and its length:
//! what is in the sectors no location points to (those freed as chunks grew or moved), in a
//! chunk's sectors after its length, and after the last chunk's, it never reads. Those bytes are
//! the stale data of earlier writes; about a third of a world's files (aqr-minecraft's FORMAT.md,
//! section 9, "Stale bytes are kept").
//! The header (locations and timestamps) is kept as it is, as is the length of the file.

use aqr_minecraft::region;

use crate::Options;

/// `d` with its slack zeroed, None where it has none; why it is kept where it is not a region
/// file of whole chunks.
pub(super) fn slack(d: &[u8], _: &Options) -> Result<Option<Vec<u8>>, String> {
    let chunks = region::parse(d).ok_or("not a region file of whole chunks")?;
    let mut live = Vec::with_capacity(chunks.len() + 1);
    live.push(0..region::HEADER);
    for c in &chunks {
        let at = c.range().start;
        let len = u32::from_be_bytes(d[at..at + 4].try_into().expect("4 bytes")) as usize;
        live.push(at..at + 4 + len);
    }
    // (in the order of their sectors, apart: [`region::parse`] tells)
    let mut r: Option<Vec<u8>> = None;
    let mut from = 0;
    for gap in live.iter().map(|l| l.start).skip(1).chain([d.len()]).zip(&live).map(|(next, l)| l.end..next) {
        debug_assert!(gap.start >= from);
        from = gap.end;
        if d[gap.clone()].iter().any(|&b| b != 0) {
            r.get_or_insert_with(|| d.to_vec())[gap].fill(0);
        }
    }
    Ok(r)
}
