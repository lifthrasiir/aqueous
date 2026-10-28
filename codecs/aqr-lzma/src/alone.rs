//! The `.lzma` container (LZMA-alone): properties, dictionary size and data size, then the stream.

use crate::lzma::Props;

pub const HEADER: usize = 13;

pub struct Header {
    pub props: Props,
    pub dict: u32,
    /// None: unknown, the stream ends with an end marker.
    pub size: Option<u64>,
}

pub fn parse(f: &[u8]) -> Option<Header> {
    if f.len() < HEADER + 5 {
        return None;
    }
    let props = Props::from_byte(f[0])?;
    let dict = u32::from_le_bytes(f[1..5].try_into().unwrap());
    let size = u64::from_le_bytes(f[5..13].try_into().unwrap());
    Some(Header { props, dict, size: (size != u64::MAX).then_some(size) })
}
