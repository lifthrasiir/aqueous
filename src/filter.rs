//! PNG scanline filters.

/// The Paeth predictor, branch-free.
#[inline(always)]
pub fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (a16, b16, c16) = (a as i16, b as i16, c as i16);
    let pa = (b16 - c16).abs();
    let pb = (a16 - c16).abs();
    let pc = (a16 + b16 - 2 * c16).abs();
    let bc = if pb <= pc { b } else { c };
    if pa <= pb && pa <= pc { a } else { bc }
}

/// The filter prediction of byte `x` of a row from the unfiltered bytes left (`a`), above
/// (`b`) and above left (`c`).
#[inline(always)]
pub fn predict(ft: u8, a: u8, b: u8, c: u8) -> u8 {
    match ft {
        1 => a,
        2 => b,
        3 => ((a as u16 + b as u16) >> 1) as u8,
        4 => paeth(a, b, c),
        _ => 0,
    }
}

/// Unfilters `rows` rows of `1 + stride` bytes (filter type first) into `out`
/// (`rows * stride` bytes). Returns false on an invalid filter type.
pub fn unfilter(data: &[u8], stride: usize, bpp: usize, out: &mut Vec<u8>) -> bool {
    let rows = data.len() / (stride + 1);
    out.clear();
    out.resize(rows * stride, 0);
    let zero = vec![0u8; stride];
    for y in 0..rows {
        let src = &data[y * (stride + 1)..(y + 1) * (stride + 1)];
        let ft = src[0];
        if ft > 4 {
            return false;
        }
        let (done, cur) = out.split_at_mut(y * stride);
        let prev = if y == 0 { &zero[..] } else { &done[(y - 1) * stride..] };
        let cur = &mut cur[..stride];
        let src = &src[1..];
        match ft {
            0 => cur.copy_from_slice(src),
            1 => {
                cur[..bpp.min(stride)].copy_from_slice(&src[..bpp.min(stride)]);
                for x in bpp..stride {
                    cur[x] = src[x].wrapping_add(cur[x - bpp]);
                }
            }
            2 => {
                for x in 0..stride {
                    cur[x] = src[x].wrapping_add(prev[x]);
                }
            }
            3 => {
                for x in 0..stride {
                    let a = if x >= bpp { cur[x - bpp] } else { 0 };
                    cur[x] = src[x].wrapping_add(((a as u16 + prev[x] as u16) >> 1) as u8);
                }
            }
            _ => {
                for x in 0..stride {
                    let (a, c) = if x >= bpp { (cur[x - bpp], prev[x - bpp]) } else { (0, 0) };
                    cur[x] = src[x].wrapping_add(paeth(a, prev[x], c));
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn paeth_matches_spec() {
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                for c in 0..=255u8 {
                    let p = a as i16 + b as i16 - c as i16;
                    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
                    let want = if pa <= pb && pa <= pc { a } else if pb <= pc { b } else { c };
                    assert_eq!(super::paeth(a, b, c), want);
                }
            }
        }
    }
}
