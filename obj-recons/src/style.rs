//! How a block's numbers are written: the encoder's choice of number mode per kind of
//! line, from a sample of the block (`recons_num::style` per kind). Only the encoder uses
//! it; the modes are in the templates.

use crate::block::{K_F, MAX_NUMS, kind_of};
use recons_num::style::{Style, choose};

/// The styles of a block: per numeric kind, and whether normals are unit vectors.
#[derive(Clone, Copy, Default)]
pub struct Styles {
    pub(crate) kinds: [Style; 4],
    pub(crate) unit: bool,
}

/// The styles for block `src` after a block with `prev` (kinds absent from `src`'s sample
/// keep theirs).
pub fn next_styles(prev: &Styles, src: &[u8]) -> Styles {
    let mut st = *prev;
    choose_styles(src, &mut st.kinds, &mut st.unit);
    st
}

/// Chooses the styles from lines spread over a block: `%.Pg` when most numbers of a kind
/// are that and their fixed formats vary; unit normals when most normals have length 1. A
/// kind not seen keeps its style (`st`, `unit`: those of the previous block).
fn choose_styles(src: &[u8], st: &mut [Style; 4], unit: &mut bool) {
    const PARTS: usize = 16;
    const PART: usize = 2 << 10;
    // Tokens with their position in the line.
    let mut toks: [Vec<(usize, &[u8])>; 4] = Default::default();
    for i in 0..PARTS {
        let a = src.len() / PARTS * i;
        let sample = &src[a..(a + PART).min(src.len())];
        // Whole lines only: from the first line start.
        let b = if a == 0 { 0 } else { memchr::memchr(b'\n', sample).map_or(sample.len(), |p| p + 1) };
        let mut lines = sample[b..].split(|&c| c == b'\n');
        lines.next_back();
        for l in lines {
            let Some((k, kw)) = kind_of(l) else { continue };
            if k >= K_F {
                continue;
            }
            for (j, t) in l[kw..].split(|&c| matches!(c, b' ' | b'\t' | b'\r')).filter(|t| !t.is_empty()).enumerate() {
                toks[k as usize].push((j.min(MAX_NUMS - 1), t));
            }
        }
    }
    for (st, ts) in st.iter_mut().zip(&toks) {
        if !ts.is_empty() {
            *st = choose(ts);
        }
    }
    // Normals of length 1 within rounding.
    let (mut nvn, mut nunit) = (0, 0);
    for t in toks[2].chunks_exact(3) {
        let x: Vec<f64> = t.iter().filter_map(|t| std::str::from_utf8(t.1).ok()?.parse().ok()).collect();
        if x.len() == 3 {
            nvn += 1;
            nunit += ((x[0] * x[0] + x[1] * x[1] + x[2] * x[2] - 1.0).abs() < 1e-3) as usize;
        }
    }
    if nvn > 0 {
        *unit = nunit * 10 >= nvn * 8;
    }
}
