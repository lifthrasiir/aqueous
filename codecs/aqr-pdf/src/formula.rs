//! aqr-pdf as a module of entry formulas (see aqr-formula, FORMAT.md): the table, the
//! skeleton and the items' data as input slots, no parameters.
//!
//! The items that other recompressors code are not decoded by the module. Their results are
//! packed input slots, which the formula fills with the children's formulas: the DEFLATE
//! streams of `FULL` items (aqr-flate applied to a slice of a pool and a slice of the
//! metadata), the PNG files that wrap `PNG` items (aqr-png) and the `JPEG` items (aqr-jpeg).
//! The module only places their bytes, with the zlib header and Adler-32 around a DEFLATE
//! stream.

use crate::encode::{self, Options, Stats};
use crate::tok;
use aqr_formula::{Apply, Formula, Module};

pub use crate::module::{DUP, FULL, JPEG, MODES, NMODES, PNG, RAW, TOK, decode};

pub const MODULE: Module = Module::new("pdf-recons", 2).with_memory(memory);

/// Bytes of a block of a stream's layout in memory (`(Block, usize)`, `Body::Dynamic` the
/// largest), and of a dynamic block's header (`ops` and `lens` at their worst, capacities and
/// the allocator's words included).
const BLOCK: u64 = 96;
const HEADER: u64 = 1408;
/// Bytes of a token; of the symbols of a block's DEFLATE bits at most per token (and of a
/// dynamic block's header and the end of block).
const TOKEN: u64 = 6;
const BLOCK_BITS: u64 = 640 + 16;
/// The window's `STEP`, `WINDOW`, tokens decoded at a time (258 bytes each) and `SLACK`.
const STEP: u64 = tok::STEP as u64;
const WINDOW: u64 = tok::WINDOW as u64;
const CHUNK_BYTES: u64 = 258 * tok::CHUNK as u64;
const SLACK: u64 = tok::SLACK as u64;

/// The bytes of a `Vec` of `n` elements of `size` bytes grown by pushes from empty (at the
/// moment it grows last: the old block and the new, twice and then half again).
fn pushed(n: u64, size: u64) -> u64 {
    let cap = n.max(4).next_power_of_two();
    (cap + if n > 4 { cap / 2 } else { 0 }) * size
}

/// The bytes of the growth of a `Vec` (`cap` elements to begin with) as each of `adds` is
/// reserved after `len` elements: the old block and the new at the worst of the moments it
/// grows, or the capacity if it never does. `adds` gives (elements in it before, to reserve).
fn reserved(mut cap: u64, size: u64, adds: impl Iterator<Item = (u64, u64)>) -> u64 {
    let mut peak = cap * size;
    for (len, add) in adds {
        if cap < len.saturating_add(add) {
            let new = (2 * cap).max(len.saturating_add(add));
            peak = peak.max((cap + new) * size);
            cap = new;
        }
    }
    peak
}

/// The memory of the module, from the table, the layout and the tables of the token coder as
/// the inputs give them (the file's `len` being the output): the inputs and the file (made at
/// its length); the unpacked slots (three vectors of 8 bytes an item); `placed` (8 bytes an
/// item of the table, grown by pushes); the coder's tables if any (tok.rs `DecoderState::new`:
/// the frequencies of each table read, 4 bytes a symbol, then the decoding tables, 8 KiB of
/// `lut` and 512 symbols of 4 bytes of `fc` each, or fewer without a literal table; and its
/// `ctx`); and the most any one token stream takes: its blocks (96 bytes each and 1408 of
/// header for each dynamic one, the layout read to know), its matches sent as 284 + 31, the
/// window (a stream's data slides: three times its largest length, `min(2 (plain + 16), keep
/// + max(STEP, keep) + 258 KiB + 16)`, the old block held while it grows), the tokens of a
/// block (6 bytes, the growth simulated over the blocks' counts), and the DEFLATE writer (it
/// holds the whole stream: 64 KiB and the growth simulated, each block taking at most 6 bytes
/// a token and 656 for the header and the end, 6 for a stored block's, the sum capped by the
/// file's length).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    use crate::kind::CLASSES;
    use crate::rans::BitR;
    use crate::tok::{DIST_ALPHA, LIT_ALPHA};
    use crate::varint::Reader;
    let &[table, _, _, deflate, png, jpeg, layout, tb, st, raw, headers] = u.contents else { return None };
    let len = u.output();
    let slots: u64 = [deflate, png, jpeg]
        .iter()
        .map(|p| {
            let n = u64::from_le_bytes(*p.first_chunk()?);
            (n <= p.len() as u64 / 8).then_some(8 * n)
        })
        .sum::<Option<u64>>()?;
    // The table: the number of items and the token streams' lengths and layouts.
    let (mut t, mut items, mut streams) = (Reader::new(table), 0u64, vec![]);
    let limit = |v: u64| usize::try_from(v).ok();
    while !t.at_end() {
        t.usize(limit(len)?).ok()?;
        match t.u8().ok()? {
            0 => drop(t.usize(limit(len)?).ok()?),
            1 => drop(t.usize(items as usize).ok()?),
            4 => {
                let flags = t.u8().ok()?;
                if flags & !7 != 0 {
                    return None;
                }
                if flags & 1 != 0 {
                    t.bytes(2).ok()?;
                }
                t.u8().ok().filter(|&c| (c as usize) < CLASSES.len())?;
                let n = t.usize(limit(len.saturating_mul(1032))?).ok()? as u64;
                let px = if flags & 4 != 0 {
                    let bpp = t.usize(64).ok()? as u64;
                    Some((bpp, t.usize((1u64 << 34) as usize).ok()? as u64))
                } else {
                    None
                };
                streams.push((n, px));
            }
            2 => {
                let flags = t.u8().ok()?;
                if flags & !3 != 0 {
                    return None;
                }
                t.bytes(2 * (flags & 1) as usize + 4 * (flags >> 1) as usize).ok()?;
            }
            3 | 5 => {}
            _ => return None,
        }
        items += 1;
    }
    let mut extra = slots + pushed(items, 8);
    let mut buffers = [slots, pushed(items, 8)].iter().filter(|&&b| b >= 1 << 16).count() as u64 * 2;
    // The coder's tables.
    let coded = [layout, tb, st, raw, headers].iter().any(|p| !p.is_empty());
    if coded {
        let mut tr = BitR::new(tb);
        let (mut lit, mut dist) = (0u64, 0u64);
        for ci in 0..=CLASSES.len() {
            if tr.bits(1) == 0 {
                continue;
            }
            let res = tr.bits(2) as u8;
            for _ in 0..tok::slot_tables(ci, res) {
                if tr.bits(1) == 1 {
                    crate::rans::read_table(&mut tr, LIT_ALPHA).ok()?;
                    lit += 1;
                }
            }
            for _ in 0..8 {
                if tr.bits(1) == 1 {
                    crate::rans::read_table(&mut tr, DIST_ALPHA).ok()?;
                    dist += 1;
                }
            }
            if tr.overrun() {
                return None;
            }
        }
        let n = lit + dist + 1;
        let stride = if lit > 0 {
            512
        } else if dist > 0 {
            32
        } else {
            1
        };
        let freqs = lit * (4 * LIT_ALPHA as u64 + 16) + dist * (4 * DIST_ALPHA as u64 + 16) + 20 + pushed(n, 12);
        let (lut, fc) = (n * 8192, n * stride * 4);
        extra += freqs + lut + fc + (CLASSES.len() as u64 + 1) * 2 * 264;
        buffers += [freqs, lut, fc].iter().filter(|&&b| b >= 1 << 16).count() as u64;
    }
    // The streams, one at a time.
    let (mut l, mut worst, mut big) = (BitR::new(layout), 0u64, 0u64);
    let layout_bits = layout.len() as u64 * 8;
    for &(plain, px) in &streams {
        if !coded {
            return None;
        }
        let nblocks = l.gamma().ok()? as u64;
        if nblocks > layout_bits / 3 {
            return None;
        }
        let std_last = l.bits(1) == 1;
        // Per block: its tokens (or stored bytes), and whether it is stored.
        let (mut blocks, mut prev, mut dynamic, mut ntok) = (vec![], 0u64, 0u64, 0u64);
        for _ in 0..nblocks {
            if !std_last {
                l.bits(1);
            }
            match l.bits(2) {
                0 => {
                    l.bits(8);
                    blocks.push((true, l.bits(16) as u64));
                }
                t @ (1 | 2) => {
                    let count = if l.bits(1) == 1 { prev } else { l.gamma().ok()? as u64 };
                    if count > plain {
                        return None;
                    }
                    (prev, ntok, dynamic) = (count, ntok + count, dynamic + (t == 2) as u64);
                    blocks.push((false, count));
                }
                _ => return None,
            }
        }
        l.bits(8);
        let nalts = l.gamma().ok()? as u64;
        if nalts > ntok {
            return None;
        }
        for _ in 0..nalts {
            l.gamma().ok()?;
        }
        if l.overrun() || ntok > plain {
            return None;
        }
        let keep = px.map_or(WINDOW, |(bpp, stride)| WINDOW.max(stride.saturating_add(bpp + 1)));
        let far = keep.saturating_add(STEP.max(keep));
        let window = 3 * far.saturating_add(CHUNK_BYTES + SLACK).min(2 * (plain + SLACK));
        let tokens = reserved(0, TOKEN, blocks.iter().filter(|b| !b.0).map(|&(_, n)| (0, n)));
        let mut at = 0u64;
        let writer = reserved(
            1 << 16,
            1,
            blocks.iter().map(|&(stored, n)| {
                let before = at.min(len);
                at += if stored { n + 6 } else { TOKEN * n + BLOCK_BITS };
                (before, if stored { n + 6 } else { TOKEN * n + BLOCK_BITS })
            }),
        );
        let total = blocks.len() as u64 * BLOCK + dynamic * HEADER + 4 * nalts + window + tokens + writer;
        if total > worst {
            (worst, big) = (total, [window, tokens, writer].iter().filter(|&&b| b >= 1 << 16).count() as u64 * 2 + 1);
        }
    }
    Some(u.whole(extra + worst, buffers + big))
}

/// The formula giving `input`, or `None` where the formula would not be smaller than `input`
/// (or no item is coded). `threads` as in [`Options::threads`].
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_with(input, &Options { threads, ..Options::default() })
}

/// [`encode`] with options.
pub fn encode_with(input: &[u8], opts: &Options) -> Option<Formula> {
    encode_stats(input, opts, &mut Stats::default())
}

/// [`encode_with`], and what the encoder did (as of the last attempt, if a panic or a failed
/// verification made it code the file again).
pub fn encode_stats(input: &[u8], opts: &Options, st: &mut Stats) -> Option<Formula> {
    let attempt = |o: &Options| {
        std::panic::catch_unwind(|| {
            let mut s = Stats::default();
            let f = encode::try_formula(input, o, &mut s);
            (f, s)
        })
        .ok()
    };
    let (mut res, mut s) = attempt(opts)?;
    if opts.verify
        && let Some(f) = &res
        && f.eval(&|a: &Apply, i: &[&[u8]]| apply(a, i, opts.threads)).as_deref() != Ok(input)
    {
        let o = Options { tok: false, full: false, png: false, ..opts.clone() };
        (res, s) = attempt(&o)?;
    }
    *st = s;
    res
}

/// Runs the modules a formula of this module has in it (this one, aqr-flate, aqr-png and
/// aqr-jpeg).
pub fn apply(a: &Apply, inputs: &[&[u8]], threads: usize) -> aqr_formula::Result<Vec<u8>> {
    let err = |e: &dyn std::fmt::Debug| aqr_formula::Error::new(format!("{}: {e:?}", a.module));
    if a.module == MODULE {
        decode(&a.params, inputs, a.len, threads).map_err(|e| err(&e))
    } else if a.module == aqr_flate::formula::MODULE {
        let resources: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
        aqr_flate::formula::decode(&a.params, &resources, inputs, a.len, 1).map_err(|e| err(&e))
    } else if a.module == aqr_png::formula::MODULE {
        aqr_png::formula::decode(&a.params, inputs, a.len, 1).map_err(|e| err(&e))
    } else if a.module == aqr_jpeg::formula::MODULE {
        aqr_jpeg::formula::decode(&a.params, inputs, a.len, 1).map_err(|e| err(&e))
    } else {
        Err(aqr_formula::Error::new(format!("no module {}", a.module)))
    }
}
