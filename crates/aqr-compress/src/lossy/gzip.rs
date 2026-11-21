//! gzip files: `coding`, each member's DEFLATE stream made again ([`super::redeflate`]), its
//! header and trailer (the CRC-32 and size of the data, which do not change) kept as they are.
//! The header's XFL, which tells the compressor's level loosely, is kept too: it is a field of
//! the header, which readers show (and none act on). A file of anything but members one after
//! another (a trailer of zeros, other data after them) is kept as it is.

use crate::Options;
use crate::flate::gzip_deflate;

/// `d` with its members' streams made again, None where none is; why it is kept where it is not
/// gzip members alone.
pub(super) fn coding(d: &[u8], opt: &Options) -> Result<Option<Vec<u8>>, String> {
    super::rebuilds(opt)?;
    let mut members = Vec::new();
    let mut p = 0;
    while p < d.len() {
        let m = &d[p..];
        if !m.starts_with(&[0x1f, 0x8b, 8]) {
            return Err("data after the gzip members".into());
        }
        let raw = gzip_deflate(m).ok_or("a gzip member not inflated")?;
        members.push(p + raw.start..p + raw.end);
        p += raw.end + 8;
    }
    let mut r: Option<Vec<u8>> = None;
    let mut from = 0;
    for raw in members {
        if let Some(new) = super::redeflate(&d[raw.clone()], opt) {
            let r = r.get_or_insert_with(|| Vec::with_capacity(d.len()));
            r.extend_from_slice(&d[from..raw.start]);
            r.extend_from_slice(&new);
            from = raw.end;
        }
    }
    Ok(r.map(|mut r| {
        r.extend_from_slice(&d[from..]);
        r
    }))
}
