//! Blocks of audio as FLAC models them: per channel a subframe (constant, verbatim, a fixed
//! predictor or LPC, with wasted bits) whose residuals go through the [residual
//! model](crate::audio::residual), and the samples rebuilt from them. FLAC files parse into
//! these frames exactly (aqr-flac); PCM is cut into them by [`crate::audio::pcm`].

/// Largest block size accepted (the format allows 65536).
pub const MAX_BLOCK: usize = 65536;
pub const MAX_CHANNELS: usize = 8;

pub const SUB_CONSTANT: u8 = 0;
pub const SUB_VERBATIM: u8 = 1;
/// Fixed predictor of order `typ - SUB_FIXED` (0..=4).
pub const SUB_FIXED: u8 = 8;
/// LPC of order `typ - SUB_LPC + 1` (1..=32).
pub const SUB_LPC: u8 = 32;

/// Marks an escaped partition in `Subframe::params`; the low 5 bits are the raw sample size.
pub const ESCAPE: u8 = 0x80;

/// One subframe. Residuals live in [`Frame::res`] (by channel), samples of verbatim
/// subframes in [`Frame::verbatim`].
#[derive(Clone, Default, Debug)]
pub struct Subframe {
    /// Raw 6-bit type field.
    pub typ: u8,
    /// Wasted bits per sample.
    pub wasted: u8,
    /// Sample size of the subframe (frame sample size, +1 for a side channel, − wasted).
    pub bits: u8,
    /// Predictor order (fixed or LPC); 0 otherwise.
    pub order: u8,
    /// LPC coefficient precision (1..=15) and shift (raw signed 5-bit value).
    pub precision: u8,
    pub shift: i8,
    /// Residual coding method (0: 4-bit parameters, 1: 5-bit) and partition order.
    pub method: u8,
    pub porder: u8,
    /// Constant value, or warm-up samples.
    pub warm: [i64; 32],
    pub coefs: [i32; 32],
    /// Rice parameter per partition, or [`ESCAPE`] | raw bits.
    pub params: Vec<u8>,
}

impl Subframe {
    pub fn has_residual(&self) -> bool {
        self.typ >= SUB_FIXED
    }
}

#[derive(Clone, Default, Debug)]
pub struct Frame {
    /// The container's header of the frame: FLAC's, without the CRC-8 (none for PCM).
    pub header: Vec<u8>,
    pub block_size: usize,
    /// Channel assignment (0..=10).
    pub assign: u8,
    pub channels: usize,
    /// Sample size of the frame.
    pub bps: u32,
    pub sub: Vec<Subframe>,
    /// Residuals per channel (`block_size - order` each).
    pub res: Vec<Vec<i32>>,
    /// Samples of verbatim subframes, in channel order.
    pub verbatim: Vec<i64>,
}

/// Reconstructs the samples of channel `ch` (before stereo decorrelation is undone).
pub fn decode_channel(f: &Frame, ch: usize, out: &mut Vec<i64>) {
    let s = &f.sub[ch];
    out.clear();
    match s.typ {
        SUB_CONSTANT => out.resize(f.block_size, s.warm[0]),
        SUB_VERBATIM => {
            let vi = f.sub[..ch].iter().filter(|s| s.typ == SUB_VERBATIM).count() * f.block_size;
            out.extend_from_slice(&f.verbatim[vi..vi + f.block_size]);
        }
        _ => {
            let order = s.order as usize;
            out.extend_from_slice(&s.warm[..order]);
            let res = &f.res[ch];
            if s.typ >= SUB_LPC {
                let c = &s.coefs[..order];
                for (i, &e) in res.iter().enumerate() {
                    let mut acc = 0i64;
                    for j in 0..order {
                        acc = acc.wrapping_add((c[j] as i64).wrapping_mul(out[i + order - 1 - j]));
                    }
                    let p = if s.shift >= 0 { acc >> s.shift } else { acc.wrapping_shl(-s.shift as u32) };
                    out.push(p.wrapping_add(e as i64));
                }
            } else {
                for (i, &e) in res.iter().enumerate() {
                    let n = i + order;
                    let o = |k: usize| out[n - k];
                    let p = match order {
                        0 => 0,
                        1 => o(1),
                        2 => o(1).wrapping_mul(2).wrapping_sub(o(2)),
                        3 => o(1).wrapping_sub(o(2)).wrapping_mul(3).wrapping_add(o(3)),
                        _ => o(1)
                            .wrapping_add(o(3))
                            .wrapping_mul(4)
                            .wrapping_sub(o(2).wrapping_mul(6))
                            .wrapping_sub(o(4)),
                    };
                    out.push(p.wrapping_add(e as i64));
                }
            }
        }
    }
    if s.wasted > 0 {
        for v in out.iter_mut() {
            *v = v.wrapping_shl(s.wasted as u32);
        }
    }
}
