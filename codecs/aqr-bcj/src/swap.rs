//! 7z's Swap2 and Swap4 filters: the bytes of each unit of 2 or 4 reversed, from the start of the
//! data (a last unit cut short kept as it is). Each is its own inverse.

/// Converts `d` in place, by units of `width` (2 or 4) bytes.
pub fn convert(d: &mut [u8], width: usize) {
    d.chunks_exact_mut(width).for_each(<[u8]>::reverse);
}
