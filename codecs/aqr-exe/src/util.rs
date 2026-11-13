#[cfg(feature = "encode")]
pub(crate) fn to_u16(s: &[u8]) -> u16 {
    u16::from_le_bytes(s.try_into().unwrap())
}

pub(crate) fn to_u32(s: &[u8]) -> u32 {
    u32::from_le_bytes(s.try_into().unwrap())
}

pub(crate) fn to_u64(s: &[u8]) -> u64 {
    u64::from_le_bytes(s.try_into().unwrap())
}

/// `v` as a LEB128 varint.
#[cfg(feature = "encode")]
pub(crate) fn put_varint(out: &mut alloc::vec::Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// A LEB128 varint of at most 10 bytes (the bits of the last above the 64th dropped), taken off
/// the front of `s`.
#[cfg(feature = "decode")]
pub(crate) fn varint(s: &mut &[u8]) -> Option<u64> {
    let mut v = 0u64;
    for k in 0..10 {
        let (&x, rest) = s.split_first()?;
        *s = rest;
        v |= ((x & 0x7f) as u64) << (7 * k);
        if x < 0x80 {
            return Some(v);
        }
    }
    None
}

/// An empty vector with room for `n` bytes, or `None` where the heap has none: a length that
/// comes from the input (a declared output) is no reason to abort.
pub(crate) fn room(n: usize) -> Option<alloc::vec::Vec<u8>> {
    let mut v = alloc::vec::Vec::new();
    v.try_reserve_exact(n).ok()?;
    Some(v)
}
