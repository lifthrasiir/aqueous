//! The cost model behind the choice of which zlib streams to rebuild by emulation: an
//! estimate of the time deconstruction and reconstruction each take on one core of an Apple
//! M4, from sizes and from cheap statistics of the streams. It never looks at a clock, so
//! the choice, and the output, depend on the input only. The constants are fitted with
//! scripts/fit-budget.py to measurements by examples/calib.rs and examples/emucost.rs.

use flate_recons::deflate::Token;

/// 100 MB/s.
pub const TARGET_NS_PER_BYTE: f64 = 10.0;

/// What the work that does not depend on the choice is estimated from.
#[derive(Clone, Copy, Debug, Default)]
pub struct Input {
    pub files: u64,
    /// Bytes of every file, of pack files, of loose objects, and of the working tree (files
    /// outside `.git`).
    pub bytes: u64,
    pub pack_bytes: u64,
    pub loose_bytes: u64,
    pub work_bytes: u64,
    /// Pack entries and loose objects.
    pub objects: u64,
}

/// A zlib stream, as the model sees it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stream {
    /// Bytes of data and of the zlib stream.
    pub size: u64,
    pub zsize: u64,
    /// zlib's header byte FLEVEL (0: levels 1 and below, 1: 2-5, 2: 6, 3: 7-9).
    pub flevel: u8,
    pub literals: u64,
    pub matches: u64,
    /// Over every position, earlier positions within zlib's window with its hash, capped at
    /// 32 (about how far zlib searches).
    pub chain: u64,
}

impl Stream {
    /// `z`: the zlib stream, whose data and tokens are `data` and `tokens`.
    pub fn new(data: &[u8], z: &[u8], tokens: &[Token]) -> Stream {
        let matches = tokens.iter().filter(|t| matches!(t, Token::Match { .. })).count() as u64;
        Stream {
            size: data.len() as u64,
            zsize: z.len() as u64,
            flevel: z.get(1).map_or(2, |b| b >> 6),
            literals: tokens.len() as u64 - matches,
            matches,
            chain: occupancy(data),
        }
    }
}

/// See [`Stream::chain`]: zlib's hash (memLevel 8) and window (32 KiB less MIN_LOOKAHEAD).
fn occupancy(d: &[u8]) -> u64 {
    const W: usize = 32768 - 262;
    let mut count = vec![0u8; 1 << 15];
    let h = |p: usize| ((d[p] as usize) << 10 ^ (d[p + 1] as usize) << 5 ^ d[p + 2] as usize) & 0x7fff;
    let mut s = 0u64;
    // Counts saturate at 255; a count leaving the window is only taken off when below it.
    let mut exact = vec![0u32; 1 << 15];
    for p in 0..d.len().saturating_sub(2) {
        if p >= W {
            let k = h(p - W);
            exact[k] -= 1;
            count[k] = exact[k].min(255) as u8;
        }
        let k = h(p);
        s += (count[k] as u64).min(32);
        exact[k] += 1;
        count[k] = exact[k].min(255) as u8;
    }
    s
}

/// ns per file, per byte (of each kind) and per object: deconstruction, reconstruction.
const PER_FILE: [f64; 2] = [5995.0, 4195.0];
const PER_OTHER_BYTE: [f64; 2] = [0.0, 0.0];
const PER_PACK_BYTE: [f64; 2] = [0.342, 0.076];
const PER_LOOSE_BYTE: [f64; 2] = [7.09, 0.0];
const PER_WORK_BYTE: [f64; 2] = [3.04, 1.30];
const PER_OBJECT: [f64; 2] = [0.0, 146.0];

/// zlib emulation of a stream (writing it), by FLEVEL 0 (deflate_fast) and higher
/// (deflate_slow), and by bytes of data below 16, 64 and 256 KiB and beyond: ns per stream,
/// per literal, per match, per byte of the zlib stream, per position's chain.
const EMU_SIZES: [u64; 3] = [16384, 65536, 262144];
const EMU: [[[f64; 5]; 4]; 2] = [
    [
        [7358.7, 0.0, 0.0, 11.27, 0.0],
        [0.0, 0.70, 5.30, 11.10, 0.0],
        [0.0, 0.0, 42.88, 0.0, 0.015],
        [0.0, 22.57, 43.96, 0.0, 0.018],
    ],
    [
        [6148.5, 0.05, 19.48, 14.30, 0.171],
        [0.0, 0.0, 85.91, 13.48, 0.094],
        [0.0, 2.43, 110.77, 15.58, 0.127],
        [0.0, 0.0, 137.52, 17.72, 0.108],
    ],
];
/// Levels 7-9 search further than 6 (not measured yet: a guess).
const EMU_FLEVEL3: f64 = 2.0;

/// What a stream emulated rather than kept costs beyond: a factor on the emulation, ns per
/// stream and per byte of data (deconstruction, reconstruction).
const EMU_FACTOR: [f64; 2] = [1.20, 1.05];
const EMU_PER_STREAM: [f64; 2] = [4027.0, 1317.0];
const EMU_PER_BYTE: [f64; 2] = [2.68, 0.389];

/// The time of zlib emulation alone.
pub fn emulation(s: &Stream) -> f64 {
    let c = &EMU[(s.flevel > 0) as usize][EMU_SIZES.iter().filter(|&&b| s.size >= b).count()];
    let t = c[0] + c[1] * s.literals as f64 + c[2] * s.matches as f64 + c[3] * s.zsize as f64 + c[4] * s.chain as f64;
    if s.flevel == 3 { t * EMU_FLEVEL3 } else { t }
}

/// The work every choice does: deconstruction, reconstruction.
pub fn base(i: &Input) -> [f64; 2] {
    std::array::from_fn(|k| {
        PER_FILE[k] * i.files as f64
            + PER_OTHER_BYTE[k] * (i.bytes - i.pack_bytes - i.loose_bytes - i.work_bytes) as f64
            + PER_PACK_BYTE[k] * i.pack_bytes as f64
            + PER_LOOSE_BYTE[k] * i.loose_bytes as f64
            + PER_WORK_BYTE[k] * i.work_bytes as f64
            + PER_OBJECT[k] * i.objects as f64
    })
}

/// What emulating a stream rather than keeping it adds: deconstruction, reconstruction.
pub fn emulate(s: &Stream) -> [f64; 2] {
    let e = emulation(s);
    std::array::from_fn(|k| EMU_FACTOR[k] * e + EMU_PER_STREAM[k] + EMU_PER_BYTE[k] * s.size as f64)
}

/// What kind of object a stream holds.
#[derive(Clone, Copy, Debug)]
pub enum Class {
    /// Commits, trees, tags.
    Text,
    Blob,
    Delta,
    Loose,
}

/// The order in which streams are offered to the budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    Largest,
    Smallest,
    /// The most output saved per time first, as roughly estimated from the class, the sizes
    /// and the level.
    Gain,
}

/// Output bytes saved per byte of the zlib stream, by class (medians over the corpus).
const GAIN: [f64; 4] = [0.7, 0.4, 0.3, 0.4];

impl Order {
    /// (For examples: ORDER=largest|smallest|gain, gain by default.)
    pub fn from_env() -> Order {
        match std::env::var("ORDER").as_deref() {
            Ok("largest") => Order::Largest,
            Ok("smallest") => Order::Smallest,
            _ => Order::Gain,
        }
    }

    /// The sort key (ascending) of a stream: its class, bytes of data and of the entry, and
    /// FLEVEL.
    pub fn key(self, class: Class, size: u64, zsize: u64, flevel: u8) -> u64 {
        match self {
            Order::Largest => u64::MAX - size,
            Order::Smallest => size,
            Order::Gain => {
                let ns = rough(size, zsize, flevel);
                let gain = GAIN[class as usize] * zsize as f64;
                // Highest gain per ns first: an inverse, in fixed point.
                (ns / gain.max(1e-3) * 1e6).min(u64::MAX as f64 / 2.0) as u64
            }
        }
    }
}

/// The time of zlib emulation from the sizes alone (a rough [`emulation`]).
pub fn rough(size: u64, zsize: u64, flevel: u8) -> f64 {
    if flevel == 0 { 6000.0 + 21.9 * zsize as f64 } else { 6000.0 + 9.07 * size as f64 + 13.66 * zsize as f64 }
}

/// Chooses streams within a budget, in the order they are offered, while both sides fit:
/// up to the first that does not, or (`skip`) passing over those.
pub struct Chooser {
    pub used: [f64; 2],
    pub budget: f64,
    full: bool,
    skip: bool,
}

impl Chooser {
    pub fn new(input: &Input, ns_per_byte: f64, skip: bool) -> Chooser {
        Chooser { used: base(input), budget: ns_per_byte * input.bytes as f64, full: false, skip }
    }

    /// Whether a stream whose emulation takes about `rough` ns could fit (so that it is
    /// worth parsing); none when full.
    pub fn might_fit(&self, rough: f64) -> bool {
        !self.full && (0..2).all(|k| self.used[k] + EMU_FACTOR[k] * rough * 0.5 + EMU_PER_STREAM[k] <= self.budget)
    }

    /// Whether the next stream (`None`: it cannot be emulated, and costs nothing) fits.
    pub fn take(&mut self, s: Option<&Stream>) -> bool {
        let Some(s) = s else { return false };
        if self.full {
            return false;
        }
        let c = emulate(s);
        if (0..2).any(|k| self.used[k] + c[k] > self.budget) {
            // Nothing more fits once not even the smallest stream would.
            self.full = !self.skip || (0..2).any(|k| self.used[k] + EMU_PER_STREAM[k] > self.budget);
            return false;
        }
        for k in 0..2 {
            self.used[k] += c[k];
        }
        true
    }

    pub fn full(&self) -> bool {
        self.full
    }
}
