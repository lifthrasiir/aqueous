//! `.lzma` files (LZMA-alone): `coding`, the stream made again by [`aqr_lzma::canonical`], whose
//! record by aqr-lzma is a few percent of XZ Utils' streams'. The 13 bytes of header (properties,
//! dictionary size, size of the data) are kept as they are; so is the end marker, where the stream
//! had one (a known size may have one too). A file with anything after the stream is kept as it is.

use aqr_lzma::{alone, canonical, lzma};

use crate::Options;

/// The most bytes of data made again (the same as `lossy:max-data`'s default).
const MAX_DATA: usize = 1 << 30;

pub(super) fn coding(d: &[u8], _: &Options) -> Result<Option<Vec<u8>>, String> {
    let h = alone::parse(d).ok_or("not an LZMA-alone header")?;
    let body = &d[alone::HEADER..];
    let dec =
        lzma::decode(body, h.props, h.size, MAX_DATA).map_err(|_| "a stream not decoded, or over 1 GiB of data")?;
    if dec.data.len() >= MAX_DATA {
        return Err("over 1 GiB of data".into());
    }
    if dec.used != body.len() {
        return Err("data after the stream".into());
    }
    let mut r = d[..alone::HEADER].to_vec();
    aqr_common::allowance::check();
    r.extend(canonical::lzma(&dec.data, h.props, h.dict, dec.marker));
    aqr_common::allowance::check();
    Ok((r[..] != d[..]).then_some(r))
}
