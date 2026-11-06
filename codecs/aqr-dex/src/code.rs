//! The instructions of code items (FORMAT.md §4.7): each instruction's fields by its format,
//! each field to a stream of its class, the indices through a cache of the recent ones of
//! their kind. Every bit of every code unit goes to some field, so any units come back.

use aqr_common::varint;

use crate::dex::{Raw, St, s};
use crate::{Result, corrupt};

/// What a field of an instruction is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Bits that the format leaves zero.
    Zero,
    /// Two registers (or a register and a literal) of 4 bits.
    Reg4,
    Reg8,
    Reg16,
    /// The count of arguments and the fifth register of `35c` and `45cc` (or the count of `3rc`).
    ArgN,
    /// The four argument registers of `35c` and `45cc`.
    Args,
    Lit,
    Branch,
    /// An index of the instruction's kind ([`Kind`]).
    Index,
    /// The prototype of `45cc` and `4rcc`.
    Proto,
}

/// What an index refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    String,
    Type,
    Field,
    Method,
    Proto,
    CallSite,
    MethodHandle,
}

/// The kinds of indices, by the cache and the stream of misses that each has.
pub const KINDS: usize = 7;

impl Kind {
    fn hits(self) -> usize {
        match self {
            Kind::String => s::HIT_STRING,
            Kind::Type => s::HIT_TYPE,
            Kind::Field => s::HIT_FIELD,
            Kind::Method => s::HIT_METHOD,
            Kind::Proto | Kind::CallSite | Kind::MethodHandle => s::IDX_HIT,
        }
    }

    fn stream(self) -> usize {
        match self {
            Kind::String => s::IDX_STRING,
            Kind::Type => s::IDX_TYPE,
            Kind::Field => s::IDX_FIELD,
            Kind::Method => s::IDX_METHOD,
            Kind::Proto | Kind::CallSite | Kind::MethodHandle => s::IDX_OTHER,
        }
    }
}

/// A field: in which code unit of the instruction, from which bit, how many bits (32 and 64
/// span units, low one first).
#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub unit: u8,
    pub shift: u8,
    pub bits: u8,
    pub class: Class,
}

const fn f(unit: u8, shift: u8, bits: u8, class: Class) -> Field {
    Field { unit, shift, bits, class }
}

use Class::*;

/// The code units and the fields after the opcode of instructions of opcode `op`, and the kind
/// of the index they hold.
pub fn format(op: u8) -> (usize, &'static [Field], Option<Kind>) {
    const F10X: &[Field] = &[f(0, 8, 8, Zero)];
    const F12X: &[Field] = &[f(0, 8, 8, Reg4)];
    const F11X: &[Field] = &[f(0, 8, 8, Reg8)];
    const F10T: &[Field] = &[f(0, 8, 8, Branch)];
    const F20T: &[Field] = &[f(0, 8, 8, Zero), f(1, 0, 16, Branch)];
    const F22X: &[Field] = &[f(0, 8, 8, Reg8), f(1, 0, 16, Reg16)];
    const F21T: &[Field] = &[f(0, 8, 8, Reg8), f(1, 0, 16, Branch)];
    const F21S: &[Field] = &[f(0, 8, 8, Reg8), f(1, 0, 16, Lit)];
    const F21C: &[Field] = &[f(0, 8, 8, Reg8), f(1, 0, 16, Index)];
    const F23X: &[Field] = &[f(0, 8, 8, Reg8), f(1, 0, 8, Reg8), f(1, 8, 8, Reg8)];
    const F22B: &[Field] = &[f(0, 8, 8, Reg8), f(1, 0, 8, Reg8), f(1, 8, 8, Lit)];
    const F22T: &[Field] = &[f(0, 8, 8, Reg4), f(1, 0, 16, Branch)];
    const F22S: &[Field] = &[f(0, 8, 8, Reg4), f(1, 0, 16, Lit)];
    const F22C: &[Field] = &[f(0, 8, 8, Reg4), f(1, 0, 16, Index)];
    const F30T: &[Field] = &[f(0, 8, 8, Zero), f(1, 0, 32, Branch)];
    const F32X: &[Field] = &[f(0, 8, 8, Zero), f(1, 0, 16, Reg16), f(2, 0, 16, Reg16)];
    const F31I: &[Field] = &[f(0, 8, 8, Reg8), f(1, 0, 32, Lit)];
    const F31T: &[Field] = &[f(0, 8, 8, Reg8), f(1, 0, 32, Branch)];
    const F31C: &[Field] = &[f(0, 8, 8, Reg8), f(1, 0, 32, Index)];
    // (the index first: the count of arguments is predicted from the method)
    const F35C: &[Field] = &[f(1, 0, 16, Index), f(0, 8, 8, ArgN), f(2, 0, 16, Args)];
    const F3RC: &[Field] = &[f(1, 0, 16, Index), f(0, 8, 8, ArgN), f(2, 0, 16, Reg16)];
    const F45CC: &[Field] = &[f(1, 0, 16, Index), f(0, 8, 8, ArgN), f(2, 0, 16, Args), f(3, 0, 16, Proto)];
    const F4RCC: &[Field] = &[f(1, 0, 16, Index), f(0, 8, 8, ArgN), f(2, 0, 16, Reg16), f(3, 0, 16, Proto)];
    const F51L: &[Field] = &[f(0, 8, 8, Reg8), f(1, 0, 64, Lit)];
    use Kind as K;
    match op {
        0x01 | 0x04 | 0x07 | 0x21 | 0x7b..=0x8f | 0xb0..=0xcf => (1, F12X, None),
        0x12 => (1, F12X, None),
        0x0a..=0x0d | 0x0f..=0x11 | 0x1d | 0x1e | 0x27 => (1, F11X, None),
        0x28 => (1, F10T, None),
        0x29 => (2, F20T, None),
        0x02 | 0x05 | 0x08 => (2, F22X, None),
        0x38..=0x3d => (2, F21T, None),
        0x13 | 0x15 | 0x16 | 0x19 => (2, F21S, None),
        0x1a => (2, F21C, Some(K::String)),
        0x1c | 0x1f | 0x22 => (2, F21C, Some(K::Type)),
        0x60..=0x6d => (2, F21C, Some(K::Field)),
        0xfe => (2, F21C, Some(K::MethodHandle)),
        0xff => (2, F21C, Some(K::Proto)),
        0x2d..=0x31 | 0x44..=0x51 | 0x90..=0xaf => (2, F23X, None),
        0xd8..=0xe2 => (2, F22B, None),
        0x32..=0x37 => (2, F22T, None),
        0xd0..=0xd7 => (2, F22S, None),
        0x20 | 0x23 => (2, F22C, Some(K::Type)),
        0x52..=0x5f => (2, F22C, Some(K::Field)),
        0x2a => (3, F30T, None),
        0x03 | 0x06 | 0x09 => (3, F32X, None),
        0x14 | 0x17 => (3, F31I, None),
        0x26 | 0x2b | 0x2c => (3, F31T, None),
        0x1b => (3, F31C, Some(K::String)),
        0x24 => (3, F35C, Some(K::Type)),
        0x6e..=0x72 => (3, F35C, Some(K::Method)),
        0xfc => (3, F35C, Some(K::CallSite)),
        0x25 => (3, F3RC, Some(K::Type)),
        0x74..=0x78 => (3, F3RC, Some(K::Method)),
        0xfd => (3, F3RC, Some(K::CallSite)),
        0xfa => (4, F45CC, Some(K::Method)),
        0xfb => (4, F4RCC, Some(K::Method)),
        0x18 => (5, F51L, None),
        // nop, return-void and the opcodes not used
        _ => (1, F10X, None),
    }
}

/// The code units of a payload (the pseudo-instructions after a `nop` whose high byte is 1, 2
/// or 3) from its first unit `ident` and the units after it `next` (as many as there are).
pub fn payload_units(ident: u16, next: &[u16]) -> Option<u64> {
    let u = |i: usize| next.get(i).map(|&v| v as u64);
    Some(match ident {
        0x0100 => 4 + 2 * u(0)?,
        0x0200 => 2 + 4 * u(0)?,
        0x0300 => 4 + (u(0)? * (u(1)? | u(2)? << 16)).div_ceil(2),
        _ => return None,
    })
}

/// The units a payload's size needs to be told: after its first.
pub fn payload_head(ident: u16) -> usize {
    if ident == 0x0300 { 3 } else { 1 }
}

/// The recent indices of each kind (as many as `len` tells), the most recent first, and the
/// last that missed.
pub struct Caches {
    recent: [[u32; CACHE]; KINDS],
    len: [usize; KINDS],
    last: [u32; KINDS],
}

/// The indices a cache keeps.
pub const CACHE: usize = 128;

impl Default for Caches {
    fn default() -> Self {
        Caches { recent: [[0; CACHE]; KINDS], len: [0; KINDS], last: [0; KINDS] }
    }
}

impl Caches {
    /// `v` to the front of the cache of `k`, from its place `i` there (none: dropping the last
    /// where it is full).
    fn used(&mut self, k: Kind, i: Option<usize>, v: u32) {
        let (r, n) = (&mut self.recent[k as usize], &mut self.len[k as usize]);
        let end = match i {
            Some(i) => i,
            None => {
                *n = (*n + 1).min(CACHE);
                *n - 1
            }
        };
        r.copy_within(..end, 1);
        r[0] = v;
    }

    /// Codes index `v` of kind `k` into `hit` and its stream of misses.
    pub fn encode(&mut self, k: Kind, v: u32, st: &mut St) {
        let r = &self.recent[k as usize][..self.len[k as usize]];
        let at = r.iter().position(|&x| x == v);
        match at {
            Some(i) => varint::put(&mut st[k.hits()], i as u64 + 1),
            None => {
                varint::put(&mut st[k.hits()], 0);
                varint::put(&mut st[k.stream()], varint::zz(v as i64 - self.last[k as usize] as i64));
                self.last[k as usize] = v;
            }
        }
        self.used(k, at, v);
    }

    /// An index of kind `k` from `hit` and its stream of misses, below `max` + 1.
    pub fn decode(&mut self, k: Kind, raw: &mut Raw, max: u32) -> Result<u32> {
        let i = raw[k.hits()].var()? as usize;
        let (v, at) = if i > 0 {
            let r = &self.recent[k as usize][..self.len[k as usize]];
            (*r.get(i - 1).ok_or_else(corrupt)?, Some(i - 1))
        } else {
            let v = crate::dex::moved(self.last[k as usize], raw[k.stream()].var()?, max)?;
            self.last[k as usize] = v;
            (v, None)
        };
        self.used(k, at, v);
        Ok(v)
    }
}

/// The stream of a field of `class` that is not an index.
fn stream(class: Class) -> usize {
    match class {
        Zero => s::INS_ZERO,
        Reg4 => s::INS_REG4,
        Reg8 => s::INS_REG8,
        Reg16 => s::INS_REG16,
        ArgN => s::INS_ARGN,
        Args => s::INS_ARGS,
        Lit => s::INS_LIT,
        Branch => s::INS_BRANCH,
        Index | Proto => unreachable!(),
    }
}

/// The count of arguments of invoking method `m` by `op` as `words` (each method's words of
/// parameters) predicts it, placed as the instruction's `ArgN` field holds it; 0 for other
/// instructions.
fn args_predicted(op: u8, m: u64, words: &[u8]) -> u64 {
    let w = match op {
        0x6e..=0x72 | 0x74..=0x78 => *words.get(m as usize).unwrap_or(&0) as u64 + (op != 0x71 && op != 0x77) as u64,
        _ => return 0,
    };
    match op {
        0x6e..=0x72 if w < 16 => w << 4,
        0x74..=0x78 => w & 0xff,
        _ => 0,
    }
}

/// The words of arguments of an invoking instruction `op` whose `ArgN` field is `a`.
fn args_of(op: u8, a: u64) -> u64 {
    match op {
        0x6e..=0x72 | 0xfa | 0xfc => a >> 4,
        0x74..=0x78 | 0xfb | 0xfd => a,
        _ => 0,
    }
}

/// Splits the code units `u` (an instruction array) into the streams; `None` where an
/// instruction runs past them. The most words of arguments its invocations pass.
pub fn encode(u: &[u16], caches: &mut Caches, words: &[u8], st: &mut St) -> Option<u64> {
    let mut outs = 0;
    let mut i = 0;
    while i < u.len() {
        let op = u[i] as u8;
        st[s::INS_OP].push(op);
        let (n, fields, kind) = format(op);
        let n = if op == 0 && matches!(u[i] >> 8, 1..=3) {
            let n = usize::try_from(payload_units(u[i], &u[i + 1..])?).ok()?;
            let end = i.checked_add(n).filter(|&e| e <= u.len())?;
            st[s::INS_ZERO].push((u[i] >> 8) as u8);
            for &x in &u[i + 1..end] {
                st[s::INS_PAYLOAD].extend_from_slice(&x.to_le_bytes());
            }
            i = end;
            continue;
        } else {
            n
        };
        let ins = u.get(i..i + n)?;
        let mut index = 0;
        for fl in fields {
            let v = get(ins, fl);
            match fl.class {
                Index => {
                    index = v;
                    caches.encode(kind.unwrap(), v as u32, st)
                }
                Proto => caches.encode(Kind::Proto, v as u32, st),
                ArgN => {
                    outs = outs.max(args_of(op, v));
                    st[s::INS_ARGN].push((v ^ args_predicted(op, index, words)) as u8)
                }
                c => st[stream(c)].extend_from_slice(&v.to_le_bytes()[..fl.bits as usize / 8]),
            }
        }
        i += n;
    }
    Some(outs)
}

/// The `n` code units of an instruction array from the streams, written to `out`; the most
/// words of arguments its invocations pass.
pub fn decode(n: usize, caches: &mut Caches, words: &[u8], raw: &mut Raw, out: &mut crate::dex::W) -> Result<u64> {
    let mut outs = 0;
    let mut i = 0;
    let mut ins = [0u16; 5];
    while i < n {
        let op = raw[s::INS_OP].u8()?;
        let (units, fields, kind) = format(op);
        if op == 0 {
            let high = raw[s::INS_ZERO].u8()?;
            if matches!(high, 1..=3) {
                let ident = (high as u16) << 8;
                let head = payload_head(ident);
                let mut next = [0u16; 3];
                for x in &mut next[..head] {
                    *x = u16::from_le_bytes(raw[s::INS_PAYLOAD].bytes(2)?.try_into().unwrap());
                }
                let total = payload_units(ident, &next[..head]).ok_or_else(corrupt)?;
                let end = (i as u64).checked_add(total).filter(|&e| e <= n as u64).ok_or_else(corrupt)?;
                out.put(&ident.to_le_bytes())?;
                for x in &next[..head] {
                    out.put(&x.to_le_bytes())?;
                }
                out.put(raw[s::INS_PAYLOAD].bytes(2 * (total as usize - 1 - head))?)?;
                i = end as usize;
                continue;
            }
            ins[0] = (high as u16) << 8;
        } else {
            ins[..units].fill(0);
            ins[0] = op as u16;
        }
        if i + units > n {
            return Err(corrupt());
        }
        let mut index = 0;
        for fl in fields {
            if op == 0 && fl.class == Zero {
                continue;
            }
            let max = (1u64 << fl.bits.min(32)) - 1;
            let v = match fl.class {
                Index => {
                    index = caches.decode(kind.unwrap(), raw, max as u32)? as u64;
                    index
                }
                ArgN => {
                    let a = raw[s::INS_ARGN].u8()? as u64 ^ args_predicted(op, index, words);
                    outs = outs.max(args_of(op, a));
                    a
                }
                Proto => caches.decode(Kind::Proto, raw, max as u32)? as u64,
                c => {
                    let b = raw[stream(c)].bytes(fl.bits as usize / 8)?;
                    let mut a = [0u8; 8];
                    a[..b.len()].copy_from_slice(b);
                    u64::from_le_bytes(a)
                }
            };
            set(&mut ins, fl, v);
        }
        for x in &ins[..units] {
            out.put(&x.to_le_bytes())?;
        }
        i += units;
    }
    Ok(outs)
}

fn get(ins: &[u16], fl: &Field) -> u64 {
    let u = fl.unit as usize;
    match fl.bits {
        32 => ins[u] as u64 | (ins[u + 1] as u64) << 16,
        64 => (0..4).map(|k| (ins[u + k] as u64) << (16 * k)).sum(),
        b => (ins[u] as u64 >> fl.shift) & ((1 << b) - 1),
    }
}

fn set(ins: &mut [u16], fl: &Field, v: u64) {
    let u = fl.unit as usize;
    match fl.bits {
        32 | 64 => {
            for k in 0..fl.bits as usize / 16 {
                ins[u + k] = (v >> (16 * k)) as u16;
            }
        }
        b => ins[u] |= ((v & ((1 << b) - 1)) << fl.shift) as u16,
    }
}
