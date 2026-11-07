//! The memory plan of the module `flate-set` on WebAssembly ([`aqr_api::stack`]), as
//! [`crate::module::decode_part`] lays it out: the instance's memory taken block by block in the
//! order the module takes it ([`Mem`]), and each spawn's tasks, each a copy of the instance's
//! memory as it spawns them and what it takes over it. What the decoders take follows from the
//! records (read here as the module reads them), the built-in catalog of their profile, and the
//! index (each stream's lengths).

use aqr_api::stack::block;

use crate::module::Derived;
use crate::module::{LARGE, RUN};
use crate::tokens::{Kind, Rule};
use crate::{Coding, Shape};

/// Bytes of what the module holds, on WebAssembly (checked there by assertions).
pub mod wasm {
    /// A block's shape in the layout, a segment, a token, a block to write and its tokens (and
    /// the shape it is made of), a rule, a static coding's model.
    pub const SHAPE: u64 = 12;
    pub const SEG: u64 = 56;
    pub const TOKEN: u64 = 6;
    pub const READY: u64 = 20;
    pub const BLOCK: u64 = 1012;
    pub const RULE: u64 = 18;
    pub const MODEL: u64 = 3116;
    /// The state of a pool's scope, an emulator's lane and job, and its window's slack.
    pub const SCOPE: u64 = 36;
    pub const LANE: u64 = 68;
    pub const JOB: u64 = 24;
    pub const WINDOW_SLACK: u64 = 280;
    /// An `Arc` of a vector (its counts and the vector), a stream's lengths, a small stream's
    /// lengths and places in its run.
    pub const ARC_VEC: u64 = 20;
    pub const LENS: u64 = 24;
    pub const RUN: u64 = 20;
}

use aqr_api::stack::Sim as Mem;

/// A segment of a record: tokens, and the data from its start to the next one's.
pub struct Seg {
    pub count: u64,
    pub pos: u64,
    pub end: u64,
    /// Whether it has a zlib emulator (the first, or resumed).
    emu: bool,
}

/// What of a stream's record the memory depends on (read as `reconstruct_into` does).
pub struct Stream {
    /// Bytes of data.
    n: u64,
    rule: Rule,
    adaptive: bool,
    /// The zlib emulator's window bits and memory level.
    zlib: Option<(u8, u8)>,
    blocks: u64,
    stored: u64,
    /// The most tokens of a block, and of all.
    largest: u64,
    pub tokens: u64,
    trailing: u64,
    pub segs: Vec<Seg>,
}

impl Stream {
    pub fn parse(n: u64, meta: &[u8], profile: u16) -> Option<Stream> {
        use crate::code::{get_usize, get_varint};
        let m = crate::profile_data(profile).ok()?;
        let nn = usize::try_from(n).ok()?;
        let adaptive = profile >= crate::ADAPTIVE;
        let mut at = 2;
        let main_len = get_varint(meta, &mut at).ok()? as usize;
        let hdr_len = get_varint(meta, &mut at).ok()? as usize;
        let main = meta.get(at..at.checked_add(main_len)?)?;
        at += main_len;
        at = at.checked_add(hdr_len)?;
        let r = &mut crate::bits::BitReader::new(main);
        let rule = if r.bits(1).ok()? == 1 {
            *m.rules.get(get_usize(r, 1 << 16).ok()?)?
        } else {
            Rule::read(r).ok()?
        };
        let layout = crate::read_blocks(r, nn).ok()?;
        let mut zlib = None;
        if adaptive {
            if r.bits(1).ok()? == 1 {
                r.bits(4).ok()?;
                r.bits(3).ok()?;
                zlib = Some((r.bits(3).ok()? as u8 + 9, r.bits(4).ok()? as u8));
            }
        } else if let Coding::Static(defaults) = m.coding {
            for d in defaults {
                crate::code::Model::read(r, d).ok()?;
            }
        }
        let nseg = get_usize(r, nn + 1).ok()?;
        let mut segs = Vec::with_capacity(nseg);
        let mut first = 0;
        for i in 0..nseg {
            let count = get_usize(r, nn).ok()?;
            if !adaptive {
                get_usize(r, count).ok()?;
                get_usize(r, count).ok()?;
            }
            let pos = if i > 0 { get_usize(r, nn).ok()? } else { 0 };
            let len = get_usize(r, meta.len()).ok()?;
            let cp = matches!(zlib, Some((wbits, _)) if i > 0 && r.bits(1).ok()? == 1 && {
                // (the checkpoint's fields: see get_checkpoint)
                let ws = 1usize << wbits;
                r.bits(1).ok()?;
                get_usize(r, nn).ok()?;
                get_usize(r, 2 * ws).ok()?;
                get_usize(r, 2 * ws).ok()?;
                let length = 2 + get_usize(r, crate::matchfind::MAX_MATCH).ok()?;
                if length > 2 {
                    get_usize(r, ws).ok()?;
                }
                true
            });
            at = at.checked_add(len)?;
            segs.push(Seg {
                count: count as u64,
                pos: pos as u64,
                end: n,
                emu: zlib.is_some() && (i == 0 || cp),
            });
            first += count;
        }
        for i in 1..segs.len() {
            segs[i - 1].end = segs[i].pos;
        }
        let total = first;
        if at != meta.len() {
            return None;
        }
        // (the blocks' token counts, the remaining ones for the block that takes them)
        let fixed: usize = layout
            .blocks
            .iter()
            .map(|b| match b {
                Shape::Coded { count: Some(c), .. } => *c,
                _ => 0,
            })
            .sum();
        let rest = total.checked_sub(fixed)?;
        let (mut largest, mut stored) = (0, 0);
        for b in &layout.blocks {
            match *b {
                Shape::Stored { .. } => stored += 1,
                Shape::Coded { count, .. } => largest = largest.max(count.unwrap_or(rest)),
            }
        }
        Some(Stream {
            n,
            rule,
            adaptive,
            zlib,
            blocks: layout.blocks.len() as u64,
            stored,
            largest: largest as u64,
            tokens: total as u64,
            trailing: layout.trailing.len() as u64,
            segs,
        })
    }
}

/// The bytes that the module `flate-inflate` takes at most, of the module's initial memory
/// (`initial` bytes), as [`crate::inflate::data_streamed`] lays it out: the stream read and the
/// data made (reserved to the most they hold, whatever the lengths), the two tables of the most
/// that a code takes (every block's codes, the fixed ones too, built in them), and over them
/// the code-length table of a dynamic header, made and dropped.
pub fn inflate(initial: u64) -> u64 {
    use crate::inflate::{CL_BITS, DIST_ENTRIES, INPUT, LIT_ENTRIES, OUTPUT};
    let mut m = Mem::new(initial);
    m.take(INPUT as u64);
    m.take(OUTPUT as u64);
    m.take(4 * LIT_ENTRIES as u64);
    m.take(4 * DIST_ENTRIES as u64);
    m.peak(block(4 << CL_BITS));
    m.plan(1)
}

/// How [`rebuild`] rebuilds a stream: from its data pulled (on `threads` threads, a segment a
/// task where more than one), or of its data whole, appended to a vector of room for it (on
/// one thread).
#[derive(Clone, Copy)]
enum How {
    Pulled(usize),
    Onto,
}

/// Bytes that decoding segment `s` of `st` takes at once besides its tokens (as `rebuild`'s
/// `decode_into` takes them), of data from position `off` on, `len` bytes of it at hand: the
/// emulator, the adaptive model, the table or chain.
fn decoder(st: &Stream, s: &Seg, off: u64, len: u64) -> u64 {
    use crate::matchfind::WINDOW;
    const RING: u64 = 1 << 17;
    const LINK_AHEAD: u64 = 8192;
    let window = WINDOW as u64;
    let (start, end) = (s.pos - off, s.end - off);
    let rule = &st.rule;
    let table = match rule.kind {
        Kind::Literals | Kind::Runs => 0,
        Kind::Search => {
            let span = end.saturating_sub(start.saturating_sub(window)).max(1);
            let bits = span.next_power_of_two().trailing_zeros().clamp(10, 15);
            let own = rule.key != 3 || rule.inner != crate::tokens::INNER_ALL;
            let mut t = block(4 * ((rule.ways as u64) << bits));
            if rule.long > 0 {
                t += block(4 << bits);
            }
            if rule.rank {
                t += block(4 * (2 * window).min((span + 1).next_power_of_two()));
                if own {
                    t += block(4 << bits);
                }
            }
            t
        }
        Kind::Chain => {
            // Matcher::single_range: the links, and the heads of Heads::new
            let first = start.saturating_sub(window);
            let span = (end + LINK_AHEAD + 1).min(len) - first;
            let heads = match span > 1 << 22 {
                true => 4 << 24,
                false => {
                    let keys = span.saturating_sub(2);
                    let slots = (4 * keys).min(1 << 22).max(2 * keys);
                    8 << slots.next_power_of_two().max(1024).trailing_zeros()
                }
            };
            block(4 * (2 * RING).min(len + 1)) + block(heads)
        }
    };
    let emu = match st.zlib {
        Some((wbits, mem)) if s.emu => {
            let w = 1u64 << wbits.clamp(9, 15);
            let bits = mem.clamp(1, 9) as u32 + 7;
            block(2 * w + wasm::WINDOW_SLACK)
                + block(2 << bits)
                + block(2 * w)
                + block(wasm::LANE)
                + block(wasm::JOB)
        }
        _ => 0,
    };
    let model = match st.adaptive {
        true => block(2 * crate::adapt::NUM as u64) + block(crate::adapt::NUM as u64),
        false => 0,
    };
    table + emu + model
}

/// [`crate::rebuild`] of stream `st` as `how` tells, on the stack of `m` (given back as it was).
fn rebuild(m: &mut Mem, st: &Stream, how: How) {
    let t0 = m.top;
    let threads = match how {
        How::Pulled(t) => t.max(1),
        How::Onto => 1,
    };
    // the layout, the static models read (gone at once), the segments, the counts, the skips
    m.take(wasm::SHAPE * st.blocks);
    m.take(st.trailing);
    if !st.adaptive {
        m.peak(block(wasm::MODEL * crate::tokens::FIELDS as u64));
    }
    m.take(wasm::SEG * st.segs.len() as u64);
    m.take(4 * st.blocks);
    m.take(8 * st.stored);
    let batch = match how {
        How::Pulled(_) => threads,
        How::Onto => 4 * threads,
    };
    let (n, k) = (st.n as usize, st.segs.len());
    let reads = |s0: usize| {
        let s1 = (s0 + batch).min(k);
        let lo =
            (st.segs[s0].pos as usize).saturating_sub(crate::BACK) / crate::ALIGN * crate::ALIGN;
        let hi = if s1 < k { st.segs[s1].pos as usize } else { n };
        (lo as u64, hi.saturating_add(crate::AHEAD).min(n) as u64)
    };
    if let How::Pulled(_) = how {
        // the writer, the data held
        m.take(crate::writer_room(
            st.largest as usize,
            st.blocks as usize,
            st.trailing as usize,
        ) as u64);
        m.take(crate::pull_room(n, k, |i| st.segs[i].pos as usize, batch) as u64);
    }
    m.take(wasm::SCOPE);
    let most = st
        .segs
        .chunks(batch)
        .map(|b| b.iter().map(|s| s.count).sum::<u64>())
        .max()
        .unwrap_or(0);
    let round = st.tokens.min(2 * st.largest + most);
    for (b, segs) in st.segs.chunks(batch).enumerate() {
        let s0 = b * batch;
        let (off, len) = match how {
            How::Pulled(_) => {
                let (lo, hi) = reads(s0);
                (lo, hi - lo)
            }
            How::Onto => (0, st.n),
        };
        let pending = |m: &mut Mem| {
            if b == 0 {
                m.take(wasm::TOKEN * round);
            }
        };
        if threads > 1 {
            // each segment a task: its tokens, and its decoder or its result (the tokens in 5
            // bytes each); the tasks' handles, the pending tokens (the first time), a result at a
            // time as it is taken
            let tasks = segs
                .iter()
                .map(|s| {
                    m.task(
                        block(wasm::TOKEN * s.count)
                            + decoder(st, s, off, len).max(block(9 + 5 * s.count)),
                    )
                })
                .collect();
            let t = m.top;
            m.take(segs.len() as u64);
            pending(m);
            for s in segs {
                m.peak(block(9 + 5 * s.count));
            }
            m.spawned(tasks);
            // (the handles given back: a hole below the pending tokens the first time)
            if b > 0 {
                m.top = t;
            }
        } else {
            pending(m);
            for s in segs {
                m.peak(decoder(st, s, off, len));
            }
        }
    }
    // a round's blocks to write (and the shapes they are made of), all rounds of as much room
    if st.blocks > 0 {
        let r = st.blocks.min(crate::ROUND_BLOCKS as u64);
        m.peak(block(wasm::READY * r) + block(wasm::BLOCK * r));
    }
    m.top = t0;
}

/// The bytes that the module takes at most, of the module's initial memory (`initial` bytes),
/// params `params`, resources of `resources` bytes, inputs of `inputs` bytes, `records` and
/// `index` (the two inputs' contents) and an output of `len` bytes, with tasks running `slots`
/// at once: the most the instance takes, or the instance as it takes a spawn's results and its
/// tasks, of the spawn whose sum is the largest.
pub fn plan(
    initial: u64,
    params: &[u8],
    resources: [u64; 2],
    inputs: [u64; 3],
    (records, index): (&[u8], &[u8]),
    len: u64,
    slots: usize,
) -> Option<u64> {
    let derived = Derived::from_params(params).ok()?;
    let lens = crate::module::lens_of(
        index,
        inputs[0],
        inputs[1],
        derived.streams_len(len, index).ok()?,
    )
    .ok()?;
    let k = lens.len() as u64;
    let profile = crate::record_profile(records).ok()?;
    let threads = if derived.before() {
        1
    } else {
        crate::module::SET_THREADS
    };
    let mut m = Mem {
        top: initial,
        high: initial,
        spawns: Vec::new(),
    };
    // the params, the resources and the index copied, the lengths, what is derived, the model
    m.take(params.len() as u64);
    m.take(resources[0]);
    m.take(resources[1]);
    m.take(inputs[2]);
    m.take(wasm::LENS * k);
    m.take(k * derived.bytes() as u64);
    if derived.crc() {
        m.take(aqr_common::crc32::WASM_TABLES);
    }
    m.take(wasm::RULE * crate::profile_data(profile).ok()?.rules.len() as u64);
    match profile >= crate::ADAPTIVE {
        true => {
            (
                m.take(2 * crate::adapt::NUM as u64),
                m.take(crate::adapt::NUM as u64),
            )
                .0
        }
        false => m.take(wasm::MODEL * crate::tokens::FIELDS as u64),
    }
    let t1 = m.top;
    let (mut i, mut at) = (0, 0usize);
    let record = |i: usize, at: usize| records.get(at..at + lens[i].1 as usize);
    while i < lens.len() {
        let (d, r, sn) = lens[i];
        if d >= LARGE as u64 {
            let st = Stream::parse(d, record(i, at)?, profile)?;
            // the record, the stream held (where what is derived comes first), the data pulled
            m.take(r);
            m.take(if derived.before() { sn } else { 0 });
            m.take(wasm::ARC_VEC);
            rebuild(&mut m, &st, How::Pulled(threads));
            m.top = t1;
            at += r as usize;
            i += 1;
            continue;
        }
        // a run of small streams: their data and records, their lengths and groups
        let mut j = i;
        let mut held = 0;
        while j < lens.len()
            && lens[j].0 < LARGE as u64
            && (j == i || held + lens[j].0 as usize <= RUN)
        {
            held += lens[j].0 as usize;
            j += 1;
        }
        let mut streams = Vec::with_capacity(j - i);
        for (q, l) in lens.iter().enumerate().take(j).skip(i) {
            streams.push(Stream::parse(l.0, record(q, at)?, profile)?);
            at += l.1 as usize;
        }
        m.take(held as u64);
        m.take(lens[i..j].iter().map(|l| l.1).sum());
        let starts = crate::module::group_starts(lens[i..j].iter().map(|l| l.0 as usize));
        m.take(wasm::RUN * (j - i) as u64);
        m.take(4 * starts.len() as u64);
        // a group's result: its streams' bytes and a byte (and what writing reserves past them),
        // then each stream rebuilt in turn onto it
        let group = |g: usize| {
            let (a, b) = (starts[g], starts[g + 1]);
            let room = block(
                1 + lens[i + a..i + b].iter().map(|l| l.2).sum::<u64>()
                    + crate::write::TOKEN_SLACK as u64,
            );
            let most = (a..b)
                .map(|s| {
                    let mut sub = Mem {
                        top: 0,
                        high: 0,
                        spawns: Vec::new(),
                    };
                    rebuild(&mut sub, &streams[s], How::Onto);
                    sub.high
                })
                .max()
                .unwrap_or(0);
            (room, most)
        };
        let groups = starts.len() - 1;
        if threads > 1 {
            let tasks = (0..groups)
                .map(&group)
                .map(|(room, most)| m.task(room + most))
                .collect();
            m.take(groups as u64);
            for g in 0..groups {
                m.peak(block(
                    1 + lens[i + starts[g]..i + starts[g + 1]]
                        .iter()
                        .map(|l| l.2)
                        .sum::<u64>(),
                ));
            }
            m.spawned(tasks);
        } else {
            for g in 0..groups {
                let (room, most) = group(g);
                m.peak(room + most);
            }
        }
        m.top = t1;
        i = j;
    }
    Some(m.plan(slots))
}
