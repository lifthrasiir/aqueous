//! What the raster modules ([`super::raster`], [`super::raster16`]) share: a raster cut into
//! bands of rows, each an application of the module.

use aqr_formula::{Formula, Kind, Module, Node};

/// A raster layout and the module that codes it.
pub trait Layout: Copy + Send + Sync {
    /// The module, and the classes of its two streams.
    const MODULE: Module;
    const CODED: &'static str;
    const PADDING: &'static str;

    fn height(&self) -> usize;
    fn stride(&self) -> usize;
    /// The layout of a band of `height` rows.
    fn with_height(self, height: usize) -> Self;
    fn to_bytes(&self) -> Vec<u8>;
    /// The coded samples and the row padding of the raster `d` (exactly of this layout).
    fn encode(d: &[u8], f: Self) -> Option<(Vec<u8>, Vec<u8>)>;
}

/// The rows of each band of a raster of `height` rows of `stride` bytes, in bands of about `band`
/// bytes: equal parts, the last the shorter.
fn bands(height: usize, stride: usize, band: usize) -> Vec<usize> {
    let rows = (band / stride.max(1)).max(1);
    let n = height.div_ceil(rows).max(1);
    let rows = height.div_ceil(n);
    (0..n).map(|i| rows.min(height - i * rows)).collect()
}

/// [`super::raster::apply_in`]: the application giving `samples` (exactly the raster of `fmt`)
/// in bands of about `band` bytes, `threads` coded at a time, its inputs pushed to `f`.
pub fn apply_in<F: Layout>(f: &mut Formula, samples: &[u8], fmt: F, threads: usize, band: usize) -> Option<Node> {
    let rows = bands(fmt.height(), fmt.stride(), band);
    if let [_] = rows[..] {
        let (coded, pad) = F::encode(samples, fmt)?;
        let inputs = vec![f.push_as(F::CODED, Kind::Compressed, coded), f.push(F::PADDING, pad)];
        return Some(Node::apply(F::MODULE, fmt.to_bytes(), inputs, samples.len() as u64));
    }
    let stride = fmt.stride();
    let bands: Vec<(F, &[u8])> = rows
        .iter()
        .scan(0, |at, &h| {
            let b = &samples[*at..*at + h * stride];
            *at += h * stride;
            Some((fmt.with_height(h), b))
        })
        .collect();
    let (mut coded, mut pad) = (Vec::new(), Vec::new());
    let mut lens = Vec::with_capacity(bands.len());
    for first in (0..bands.len()).step_by(threads.max(1)) {
        let n = threads.max(1).min(bands.len() - first);
        let done = aqr_common::pool::par_map(n, threads, |i| {
            let (fmt, b) = bands[first + i];
            let c = F::encode(b, fmt);
            aqr_common::progress::done(b);
            c
        });
        for c in done {
            let (c, p) = c?;
            lens.push((c.len() as u64, p.len() as u64));
            coded.extend_from_slice(&c);
            pad.extend_from_slice(&p);
        }
    }
    let base = f.streams.len();
    f.push_as(F::CODED, Kind::Compressed, coded);
    f.push(F::PADDING, pad);
    let mut at = (0u64, 0u64);
    let nodes: Vec<Node> = bands
        .iter()
        .zip(lens)
        .map(|(&(fmt, b), (c, p))| {
            let inputs = vec![Node::slice(base, at.0, c), Node::slice(base + 1, at.1, p)];
            at = (at.0 + c, at.1 + p);
            Node::apply(F::MODULE, fmt.to_bytes(), inputs, b.len() as u64)
        })
        .collect();
    Some(Node::concat(nodes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_of_bands() {
        assert_eq!(bands(10, 100, 10_000), [10]);
        assert_eq!(bands(10, 100, 400), [4, 4, 2]);
        assert_eq!(bands(10, 100, 1), [1; 10]);
        assert_eq!(bands(7, 100, 300), [3, 3, 1]);
        assert_eq!(bands(1, 100, 1), [1]);
    }
}
