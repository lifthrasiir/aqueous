//! Context mixing in integers (so that every platform predicts alike): probabilities of 12 bits,
//! their logits (`stretch`, of 12 bits signed), adaptive counters, and a logistic mixer.

/// A coder of binary decisions: given the probability (of 16 bits) that the bit is 1, the
/// encoder codes `bit` and returns it; the decoder returns the bit it decodes.
pub trait Coder {
    fn code(&mut self, p1: u32, bit: bool) -> bool;
}

/// The cost in bits of what is coded, for measuring a model.
#[derive(Debug, Default, Clone, Copy)]
pub struct Cost(pub f64);

impl Coder for Cost {
    fn code(&mut self, p1: u32, bit: bool) -> bool {
        let p = if bit { p1 } else { 65536 - p1 } as f64 / 65536.0;
        self.0 -= p.log2();
        bit
    }
}

impl Coder for aqr_common::arith::ArithEncoder {
    fn code(&mut self, p1: u32, bit: bool) -> bool {
        self.encode(bit, p1);
        bit
    }
}

impl Coder for aqr_common::arith::ArithDecoder<'_> {
    fn code(&mut self, p1: u32, _: bool) -> bool {
        self.decode(p1)
    }
}

/// squash: a logit d (-2047..=2047, 8 fractional bits) to a probability of 12 bits.
pub fn squash(d: i32) -> i32 {
    const T: [i32; 33] = [
        1, 2, 3, 6, 10, 16, 27, 45, 73, 120, 194, 310, 488, 747, 1101, 1546, 2047, 2549, 2994, 3348, 3607, 3785, 3901,
        3975, 4024, 4050, 4068, 4079, 4085, 4089, 4092, 4093, 4094,
    ];
    if d > 2047 {
        return 4095;
    }
    if d < -2047 {
        return 1;
    }
    let w = d & 127;
    let i = ((d >> 7) + 16) as usize;
    (T[i] * (128 - w) + T[i + 1] * w + 64) >> 7
}

/// stretch: the inverse of [`squash`], as a table of the 4096 probabilities.
pub struct Stretch(Box<[i16; 4096]>);

impl Default for Stretch {
    fn default() -> Self {
        let mut t = Box::new([0i16; 4096]);
        let mut pi = 0;
        for x in -2047..=2047 {
            let v = squash(x);
            for p in pi..=v as usize {
                t[p] = x as i16;
            }
            pi = v as usize + 1;
        }
        for p in pi..4096 {
            t[p] = 2047;
        }
        Stretch(t)
    }
}

impl Stretch {
    pub fn get(&self, p12: i32) -> i32 {
        self.0[p12.clamp(0, 4095) as usize] as i32
    }
}

/// An adaptive probability (16 bits) with a count of what it has seen: fast to learn, then slower,
/// to a floor of 1/`LIMIT`.
#[derive(Debug, Clone, Copy)]
pub struct Counter {
    p: u16,
    n: u8,
}

const LIMIT: u8 = 60;

impl Default for Counter {
    fn default() -> Self {
        Counter { p: 1 << 15, n: 0 }
    }
}

impl Counter {
    /// A counter at probability `p16` (of 16 bits), as if it had seen `n` bits.
    pub fn at(p16: u32, n: u8) -> Counter {
        Counter { p: p16.clamp(1, 65535) as u16, n }
    }

    pub fn p12(self) -> i32 {
        (self.p >> 4) as i32
    }

    pub fn update(&mut self, bit: bool) {
        let target = if bit { 65535 } else { 0 };
        let p = self.p as i32;
        self.p = (p + (target - p) / (self.n as i32 + 2)) as u16;
        if self.n < LIMIT {
            self.n += 1;
        }
    }
}

/// A logistic mixer of `N` inputs (logits), its weights chosen by a context.
#[derive(Debug, Clone)]
pub struct Mixer<const N: usize> {
    weights: Vec<[i32; N]>,
    inputs: [i32; N],
    set: usize,
    p: i32,
    rate: i32,
}

impl<const N: usize> Mixer<N> {
    /// `sets` weight sets, each starting at `init` (16 fractional bits).
    pub fn new(sets: usize, init: [i32; N], rate: i32) -> Self {
        Mixer { weights: vec![init; sets], inputs: [0; N], set: 0, p: 2048, rate }
    }

    /// The probability (12 bits) of a 1 from `inputs` under weight set `set`.
    pub fn mix(&mut self, set: usize, inputs: [i32; N]) -> i32 {
        self.set = set;
        self.inputs = inputs;
        let w = &self.weights[set];
        let dot: i64 = (0..N).map(|i| w[i] as i64 * inputs[i] as i64).sum();
        self.p = squash((dot >> 16) as i32);
        self.p
    }

    pub fn update(&mut self, bit: bool) {
        let err = ((bit as i32) << 12) - self.p;
        for (w, &x) in self.weights[self.set].iter_mut().zip(&self.inputs) {
            *w += (x * err * self.rate) >> 16;
        }
    }
}

/// A probability of 12 bits as one of 16 for the coder, never 0 or 1.
pub fn p16(p12: i32) -> u32 {
    ((p12.clamp(1, 4095) as u32) << 4).clamp(32, 65536 - 32)
}

/// An adaptive map of a probability, per context (an APM, or SSE): 33 points over the logit,
/// starting as the identity, the two around the input interpolated and both moved to the bit.
#[derive(Debug, Clone)]
pub struct Apm {
    t: Vec<u16>,
    at: usize,
    rate: u32,
}

impl Apm {
    pub fn new(contexts: usize, rate: u32) -> Self {
        let mut t = Vec::with_capacity(contexts * 33);
        for _ in 0..contexts {
            for i in 0..33 {
                t.push((squash((i - 16) * 128) * 16) as u16);
            }
        }
        Apm { t, at: 0, rate }
    }

    /// The probability (12 bits) for logit `st` in context `cx`.
    pub fn p(&mut self, st: i32, cx: usize) -> i32 {
        let st = st.clamp(-2047, 2047) + 2048;
        let (lo, w) = ((st >> 7) as usize, st & 127);
        self.at = cx * 33 + lo + (w >> 6) as usize;
        let i = cx * 33 + lo;
        ((self.t[i] as i32 * (128 - w) + self.t[i + 1] as i32 * w) >> 11).clamp(1, 4095)
    }

    pub fn update(&mut self, bit: bool) {
        let g = ((bit as u32) << 16) + ((bit as u32) << self.rate) - bit as u32 - bit as u32;
        let t = &mut self.t[self.at];
        *t = (*t as i32 + ((g as i32 - *t as i32) >> self.rate)) as u16;
    }
}
