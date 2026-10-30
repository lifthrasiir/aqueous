//! The Delta filter of xz and 7z: each byte less the one `dist` (1 to 256) bytes before it
//! (those before the data taken as zeros).

/// Encodes `d` in place.
pub fn encode(d: &mut [u8], dist: usize) {
    for i in (dist..d.len()).rev() {
        d[i] = d[i].wrapping_sub(d[i - dist]);
    }
}

/// Decodes `d` in place.
pub fn decode(d: &mut [u8], dist: usize) {
    for i in dist..d.len() {
        d[i] = d[i].wrapping_add(d[i - dist]);
    }
}
