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

/// An empty vector with room for `n` bytes, or `None` where the heap has none: a length that
/// comes from the input (a declared output) is no reason to abort.
pub(crate) fn room(n: usize) -> Option<alloc::vec::Vec<u8>> {
    let mut v = alloc::vec::Vec::new();
    v.try_reserve_exact(n).ok()?;
    Some(v)
}
