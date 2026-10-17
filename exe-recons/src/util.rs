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

