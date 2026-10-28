//! The blocks themselves: what their endpoints and indices decode to, as the models predict
//! with it (FORMAT.md, "Blocks"). Nothing here needs to match a GPU's decoding exactly: the
//! values only predict, the blocks' bytes are coded as they are.

/// An RGB565 endpoint, its channels expanded to 8 bits.
#[inline(always)]
pub fn rgb565(c: u16) -> [i32; 3] {
    let (r, g, b) = ((c >> 11) as i32, (c >> 5 & 63) as i32, (c & 31) as i32);
    [r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2]
}

/// The four colours of a colour block of endpoints `c0`, `c1`: in four-colour mode (`c0 >
/// c1`, or always in BC2 and BC3) the endpoints and the thirds between them, else the
/// endpoints, their midpoint and black.
#[inline(always)]
pub fn colours(c0: u16, c1: u16, four: bool) -> [[i32; 3]; 4] {
    let (p0, p1) = (rgb565(c0), rgb565(c1));
    let mix = |a: i32, b: i32, n: i32, d: i32| (a * (d - n) + b * n + d / 2) / d;
    if four || c0 > c1 {
        [p0, p1, [0, 1, 2].map(|k| mix(p0[k], p1[k], 1, 3)), [0, 1, 2].map(|k| mix(p0[k], p1[k], 2, 3))]
    } else {
        [p0, p1, [0, 1, 2].map(|k| mix(p0[k], p1[k], 1, 2)), [0; 3]]
    }
}

/// The eight values of a BC4 block of endpoints `a0`, `a1`: in eight-value mode (`a0 > a1`)
/// the endpoints and six values between them, else the endpoints, four values between them,
/// 0 and 255.
#[inline(always)]
pub fn values(a0: u8, a1: u8) -> [i32; 8] {
    let (a, b) = (a0 as i32, a1 as i32);
    let mut v = [a, b, 0, 0, 0, 0, 0, 255];
    if a0 > a1 {
        for k in 2..8 {
            v[k] = (a * (8 - k as i32) + b * (k as i32 - 1) + 3) / 7;
        }
    } else {
        for k in 2..6 {
            v[k] = (a * (6 - k as i32) + b * (k as i32 - 1) + 2) / 5;
        }
    }
    v
}

/// The level of each index of a colour block in four-colour mode: its place on the line from
/// the first endpoint to the second.
pub const COLOUR_LEVEL: [u8; 4] = [0, 3, 1, 2];
pub const COLOUR_INDEX: [u8; 4] = [0, 2, 3, 1];
/// In three-colour mode, the levels of the line (0 to 2) and black (3).
pub const COLOUR3_LEVEL: [u8; 4] = [0, 2, 1, 3];
pub const COLOUR3_INDEX: [u8; 4] = [0, 2, 1, 3];

/// The level of each index of a BC4 block in eight-value mode, as for colours.
pub const VALUE_LEVEL: [u8; 8] = [0, 7, 1, 2, 3, 4, 5, 6];
pub const VALUE_INDEX: [u8; 8] = [0, 2, 3, 4, 5, 6, 7, 1];
/// In six-value mode, the levels of the line (0 to 5), then 0 (6) and 255 (7).
pub const VALUE6_LEVEL: [u8; 8] = [0, 5, 1, 2, 3, 4, 6, 7];
pub const VALUE6_INDEX: [u8; 8] = [0, 2, 3, 4, 5, 1, 6, 7];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels() {
        for (l, i) in [(COLOUR_LEVEL, COLOUR_INDEX), (COLOUR3_LEVEL, COLOUR3_INDEX)] {
            for k in 0..4 {
                assert_eq!(i[l[k] as usize], k as u8);
            }
        }
        for (l, i) in [(VALUE_LEVEL, VALUE_INDEX), (VALUE6_LEVEL, VALUE6_INDEX)] {
            for k in 0..8 {
                assert_eq!(i[l[k] as usize], k as u8);
            }
        }
        // the levels are in order of value
        let v = values(200, 10);
        assert!((0..7).all(|l| v[VALUE_INDEX[l] as usize] > v[VALUE_INDEX[l + 1] as usize]));
        let v = values(10, 200);
        assert!((0..5).all(|l| v[VALUE6_INDEX[l] as usize] < v[VALUE6_INDEX[l + 1] as usize]));
        let c = colours(0xffff, 0, true);
        assert!((0..3).all(|l| c[COLOUR_INDEX[l] as usize][1] > c[COLOUR_INDEX[l + 1] as usize][1]));
    }
}
