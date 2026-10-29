//! A quick estimate of what the inference may save, to give up early on files where it would
//! save little.
//!
//! A large saving needs records over a large part of the file, so windows spread evenly over
//! it find them: a structure over 15% of the file escapes 18 windows with a chance under 5%.
//! Each window gets the record size its bytes repeat at most (as `fixed::scan_window`), and is
//! compressed with zstd as it is, transposed by that size and split into fields (with their
//! floats lifted, where they have any); the windows' savings, over their compressed sizes,
//! estimate the file's.
//!
//! The estimate is low (on the oracle corpus, 0.6–4% where the inference saves 3–9%, as it
//! misses what small tables and chains of entries save), but it tells large savings: the five
//! files saving 10% or more, 13–47%, are estimated at 6.2–50%; no other above 7.5%.

use crate::fields::{self, Cost};
use crate::module::lifted_width;
use crate::{fixed, floats, zcost};

/// Windows of WINDOW bytes, PROBES of them over the file (the file itself if not larger).
const WINDOW: usize = 1 << 13;
const PROBES: usize = 48;
/// The largest record size a window is tried at.
const MAX_K: usize = 256;

#[derive(Clone, Debug)]
pub struct Estimate {
    /// The fraction of the file's zstd size the transposed windows save.
    pub saves: f64,
    /// The windows that saved, of how many.
    pub hits: usize,
    pub windows: usize,
}

pub fn estimate(data: &[u8]) -> Estimate {
    let n = data.len();
    let w = WINDOW.min(n);
    let starts =
        if n <= w * PROBES { (0..n / w.max(1)).map(|i| i * w).collect() } else { crate::linspace(n - w, PROBES) };
    let (mut before, mut after, mut hits) = (0usize, 0usize, 0usize);
    let mut counts = Vec::new();
    let mut t = Vec::with_capacity(w);
    for &s in &starts {
        let a = &data[s..s + w];
        let z = zcost::len(&[a]);
        before += z;
        let (k, gain, h0) = fixed::scan_window(a, MAX_K, &mut counts);
        if k == 1 || gain < 0.05 || h0 > 7.8 {
            after += z;
            continue;
        }
        let rows = w / k;
        t.clear();
        for j in 0..k {
            t.extend((0..rows).map(|i| a[i * k + j]));
        }
        t.extend_from_slice(&a[rows * k..]);
        // the records' fields (not against each other, which costs much and tells little), or
        // their bytes transposed, whichever is smaller
        let m = &a[..rows * k];
        let (fl, _, _) = fields::split(m, k, Cost::Kt, false);
        let mut f = fields::encode(m, k, &fl);
        f.extend_from_slice(&a[rows * k..]);
        let mut zt = zcost::len(&[&t]).min(zcost::len(&[&f]));
        // and with their floats lifted, where they have any
        let (p, fl) = floats::find(m, k, s);
        if !fl.is_empty() {
            let rows = (w - p) / k;
            let m = &a[p..p + rows * k];
            let fl = floats::widths(m, k, &fl);
            let (l, k2) = (floats::lift(m, k, &fl), lifted_width(k, &fl));
            let (ff, _, _) = fields::split(&l, k2, Cost::Kt, false);
            let mut f = a[..p].to_vec();
            f.extend(fields::encode(&l, k2, &ff));
            f.extend_from_slice(&a[p + rows * k..]);
            zt = zt.min(zcost::len(&[&f]));
        }
        if zt < z {
            hits += 1;
            after += zt;
        } else {
            after += z;
        }
    }
    Estimate { saves: 1.0 - after as f64 / before.max(1) as f64, hits, windows: starts.len() }
}
