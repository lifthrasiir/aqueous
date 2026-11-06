//! aqr-flate as a module of entry formulas (see aqr-formula): [`reconstruct_with`] on three
//! inputs, the data and the records of streams of one profile and the index cutting them, which
//! it gives one after another, then what its parameters derive of them ([`Derived`]). Its two resources
//! are the model data of the records' profile, in binary (FORMAT.md, "Resources"): the rules of
//! the catalog, then the default models or the priors.
//!
//! [`reconstruct_with`]: crate::reconstruct_with

use std::sync::OnceLock;

use aqr_formula::{Apply, Formula, Module, Node, Resource};

use crate::Shape;
use crate::tokens::Kind;

use crate::adapt::Priors;
use crate::code::{self};
use crate::tokens::{FIELDS, Rule};
use crate::{ADAPTIVE, Coding, Result};

use crate::module::RULE_BYTES;
pub use crate::module::{
    Derived, decode, index, models_from_bytes, priors_from_bytes, rules_from_bytes,
};

pub const MODULE: Module = Module::new("flate-set", 1).with_memory(memory);

/// The module `flate-inflate`: the data of a raw DEFLATE stream ([`crate::module::inflate`]),
/// for a file whose data a DEFLATE stream archived along with it holds (the body of a ZIP
/// archive's entry, a gzip member or a zlib stream, not rebuilt): the stream is then stored once.
pub const INFLATE: Module = Module::new("flate-inflate", 1).with_memory(memory_inflate);

/// The memory of [`INFLATE`], a part module: what it holds of the stream and of the data, with
/// the pages of slack of each, its tables among the small allocations.
fn memory_inflate(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let held = (crate::inflate::INPUT + crate::inflate::OUTPUT) as u64;
    Some(u.initial() + aqr_formula::SMALL + held + 2 * aqr_formula::PAGE)
}

/// The application of [`INFLATE`] to `raw`, a raw DEFLATE stream of `len` bytes of data.
pub fn inflated(raw: Node, len: u64) -> Node {
    Node::apply(INFLATE, Vec::new(), vec![raw], len)
}

/// Bytes of a rule in a catalog resource, and a rule in memory (`RULE_BYTES` and the fields of
/// [`Rule`] on 32 bits, rounded up).
const RULE_MEM: u64 = 24;

/// Bytes a token takes: [`Token`](crate::deflate::Token) has `len` and `dist` (u16), a flag and a tag.
const TOKEN: u64 = 6;

/// Bytes of data from which a stream gets threads (as `module::LARGE`), and how many tasks of an
/// instance run at once (the module's slots).
const LARGE: u64 = 2 << 20;
const SLOTS: usize = 8;

/// Slack for the large allocations of a task, and the small ones.
const TASK_SLACK: u64 = 4 * aqr_formula::PAGE + aqr_formula::SMALL;

/// A byte count `a + l * L`, `L` being the bytes of the streams made (`l` times the share of one
/// stream in them: the sum of the streams' is at most `L`).
#[derive(Clone, Copy, Default)]
struct Lin {
    a: u64,
    l: u64,
}

impl Lin {
    fn of(a: u64) -> Lin {
        Lin { a, l: 0 }
    }

    fn l(l: u64) -> Lin {
        Lin { a: 0, l }
    }

    fn add(self, o: Lin) -> Lin {
        Lin {
            a: self.a + o.a,
            l: self.l + o.l,
        }
    }

    fn mul(self, k: u64) -> Lin {
        Lin {
            a: self.a * k,
            l: self.l * k,
        }
    }

    fn max(self, o: Lin) -> Lin {
        Lin {
            a: self.a.max(o.a),
            l: self.l.max(o.l),
        }
    }

    fn eval(self, l: u64) -> u64 {
        self.a.saturating_add(self.l.saturating_mul(l))
    }
}

/// The cut of `data` and `records` by `index` (as `module::cut`), the two of each stream.
fn cut_items<'a>(
    data: &'a [u8],
    records: &'a [u8],
    index: &[u8],
) -> Option<Vec<(&'a [u8], &'a [u8])>> {
    if index.is_empty() {
        return Some(vec![(data, records)]);
    }
    let (mut at, mut d, mut r) = (0, data, records);
    let mut out = Vec::new();
    while at < index.len() {
        let mut take = |from: &mut &'a [u8]| {
            let n = usize::try_from(crate::code::get_varint(index, &mut at).ok()?).ok()?;
            let (a, b) = from.split_at_checked(n)?;
            *from = b;
            Some(a)
        };
        out.push((take(&mut d)?, take(&mut r)?));
    }
    (d.is_empty() && r.is_empty()).then_some(out)
}

/// A segment of a record: tokens, and the data from its start to the next one's.
struct Seg {
    count: u64,
    pos: u64,
    end: u64,
    /// Whether it has a zlib emulator (the first, or resumed).
    emu: bool,
}

/// What of a stream's record the memory depends on (read as `reconstruct_into` does).
struct Stream {
    /// Bytes of data.
    n: u64,
    rule: Rule,
    adaptive: bool,
    /// The zlib emulator's window bits and memory level.
    zlib: Option<(u8, u8)>,
    blocks: u64,
    stored: u64,
    /// Bytes of the stored blocks.
    stored_bytes: u64,
    /// The most tokens of a block, and of all.
    largest: u64,
    tokens: u64,
    trailing: u64,
    segs: Vec<Seg>,
}

impl Stream {
    fn parse(n: u64, meta: &[u8], profile: u16) -> Option<Stream> {
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
        let (mut largest, mut stored, mut stored_bytes) = (0, 0, 0);
        for b in &layout.blocks {
            match *b {
                Shape::Stored { len, .. } => {
                    stored += 1;
                    stored_bytes += len as u64;
                }
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
            stored_bytes,
            largest: largest as u64,
            tokens: total as u64,
            trailing: layout.trailing.len() as u64,
            segs,
        })
    }

    /// Bytes of the stream at most: 6 for each token (48 bits at most), the stored blocks' data,
    /// a block's header (a dynamic one of some 300 bytes at most), the trailing bytes.
    fn most(&self) -> u64 {
        TOKEN * self.tokens + self.stored_bytes + 320 * self.blocks + self.trailing + 64
    }

    /// Bytes of the table, chain or emulator and the contexts that decoding segment `s` makes
    /// (`code_range`, `Emulator`, `Adaptive::new`).
    fn decoder(&self, s: &Seg) -> u64 {
        const WINDOW: u64 = 32768;
        let rule = &self.rule;
        let table = match rule.kind {
            Kind::Literals | Kind::Runs => 0,
            Kind::Search => {
                let span = s.end.saturating_sub(s.pos.saturating_sub(WINDOW)).max(1);
                let bits = span.next_power_of_two().trailing_zeros().clamp(10, 15);
                let own = rule.key != 3 || rule.inner != crate::tokens::INNER_ALL;
                let mut entries = (rule.ways as u64) << bits;
                if rule.long > 0 {
                    entries += 1 << bits;
                }
                if rule.rank {
                    entries += (1 << 16).min((span + 1).next_power_of_two());
                    if own {
                        entries += 1 << bits;
                    }
                }
                4 * entries
            }
            Kind::Chain => {
                // Matcher::single_range: the links, and the heads of Heads::new
                let first = s.pos.saturating_sub(WINDOW);
                let span = (s.end + 8192 + 1).min(self.n).saturating_sub(first);
                let heads = if span > 1 << 22 {
                    4 << 24
                } else {
                    let keys = span.saturating_sub(2);
                    let slots = (4 * keys).min(1 << 22).max(2 * keys);
                    8 << slots.next_power_of_two().max(1024).trailing_zeros()
                };
                4 * (2 * (1u64 << 17)).min(self.n + 1) + heads
            }
        };
        let emu = match self.zlib {
            Some((wbits, mem)) if s.emu => crate::zlib::emulator_bytes(wbits, mem),
            _ => 0,
        };
        // (the contexts' probabilities and counts)
        let model = if self.adaptive {
            3 * crate::adapt::NUM as u64
        } else {
            0
        };
        table + emu + model
    }

    /// Bytes of the encoding of the result of a task of segment `s` (reserved): a literal takes 2
    /// and a match, which covers 3 bytes of data at least, 5.
    fn encoded(&self, s: &Seg) -> u64 {
        let (c, d) = (s.count, s.end.saturating_sub(s.pos));
        16 + 2 * c + 3 * c.min(d.saturating_sub(c) / 2)
    }

    /// The growth of a task of segment `s`: the decoder and the tokens, then the tokens and their
    /// encoding; the decoder is dropped before, the memory it had maybe not reused.
    fn task(&self, s: &Seg) -> u64 {
        self.decoder(s) + TOKEN * s.count + self.encoded(s) + TASK_SLACK
    }

    /// The memory of the instance that decodes the stream, from the memory it has at the start
    /// (`w0`), on `threads` threads; `alone` where it is the output, reserved at its length.
    fn run(&self, w0: Lin, threads: usize, alone: bool) -> Lin {
        let t = threads > 1;
        let batch = 4 * threads;
        let n = self.n;
        // the layout (grown by pushes beyond 2^16 blocks), the counts, the skips, the segments, the
        // models (static)
        let grow = if self.blocks > 1 << 16 { 3 } else { 1 };
        let stat = 12 * grow * self.blocks
            + 4 * self.blocks
            + 24 * self.stored
            + 64 * self.segs.len() as u64
            + self.trailing
            + if self.adaptive {
                0
            } else {
                9 * std::mem::size_of::<crate::code::Model>() as u64
            }
            + aqr_formula::SMALL;
        let me = w0.add(Lin::of(stat));
        let nready = self.blocks.min(1 << 12);
        // the largest batch's tokens
        let batches: Vec<&[Seg]> = self.segs.chunks(batch).collect();
        let tokens = |b: &[Seg]| b.iter().map(|s| s.count).sum::<u64>();
        let results = |b: &[Seg]| TOKEN * tokens(b) + 24 * b.len() as u64;
        let most = batches.iter().map(|b| tokens(b)).max().unwrap_or(0);
        // the pending tokens, reserved for the most at once
        let round = self.tokens.min(2 * self.largest + most);
        let pending = Lin::of(TOKEN * round);
        // the writer's output, reserved as it first writes: the stream alone, or half the data and
        // grown by pushes (the old and new one held: three times the stream)
        // (the stream's bytes at most `Stream::most`)
        let out = match alone {
            true => Lin::l(1),
            false => Lin::of((n / 2 + 64).max(self.most()) + 2 * self.most()),
        };
        // and the blocks written (a few thousand at a time); on threads, runs of them by writers
        // of their own, grown by pushes (a token is 6 bytes at most, as TOKEN), appended
        let ready = 3600 * nready;
        let runs = Lin::of(
            ready
                + if t {
                    3 * (TOKEN * round + 16 * nready)
                } else {
                    0
                },
        );
        let merge = Lin::of(batches.iter().map(|b| results(b)).max().unwrap_or(0));
        let mut peak = me.add(pending).add(out).add(runs.max(merge));
        for (b, segs) in batches.iter().enumerate() {
            // the memory a batch's tasks copy: with the results of the one before and, from the
            // third batch, the pending tokens, the output and the earlier writing
            let mut sp = me;
            if b >= 1 {
                sp = sp.add(Lin::of(results(batches[b - 1])));
            }
            if b >= 2 {
                sp = sp.add(pending).add(out).add(runs);
            }
            if t && segs.len() > 1 {
                let mut costs: Vec<u64> = segs.iter().map(|s| self.task(s)).collect();
                costs.sort_unstable_by(|a, b| b.cmp(a));
                let k = costs.len().min(SLOTS);
                let grown: u64 = costs[..k].iter().sum();
                peak = peak.max(sp.add(sp.mul(k as u64)).add(Lin::of(grown)));
            } else {
                let work = segs
                    .iter()
                    .map(|s| self.decoder(s) + TOKEN * s.count)
                    .max()
                    .unwrap_or(0);
                peak = peak.max(sp.add(Lin::of(results(segs) + work)));
            }
        }
        peak
    }
}

impl Stream {
    /// [`Stream::run`] of the stream rebuilt from its data pulled ([`crate::reconstruct_pulled`]),
    /// its record of `record` bytes held: the instance's memory at most, from `w0`, and that of
    /// each batch's tasks besides (`Seg::task` each; the tasks copy the instance's memory, which
    /// the caller gives). A batch has a segment for each thread; the data it reads is held at the
    /// most, the next batch decoded once the blocks of one are written, the stored blocks written
    /// a few MiB at a time.
    fn pulled(&self, w0: Lin, record: u64, l: u64) -> (Lin, Vec<(usize, u64)>) {
        let batch = SLOTS;
        let n = self.n;
        let grow = if self.blocks > 1 << 16 { 3 } else { 1 };
        let stat = 12 * grow * self.blocks
            + 4 * self.blocks
            + 24 * self.stored
            + 64 * self.segs.len() as u64
            + self.trailing
            + if self.adaptive {
                0
            } else {
                9 * std::mem::size_of::<crate::code::Model>() as u64
            }
            + aqr_formula::SMALL;
        let me = w0.add(Lin::of(stat + record));
        let nready = self.blocks.min(1 << 12);
        let batches: Vec<&[Seg]> = self.segs.chunks(batch).collect();
        let tokens = |b: &[Seg]| b.iter().map(|s| s.count).sum::<u64>();
        let results = |b: &[Seg]| TOKEN * tokens(b) + 24 * b.len() as u64;
        let most = batches.iter().map(|b| tokens(b)).max().unwrap_or(0);
        let round = self.tokens.min(2 * self.largest + most);
        let pending = Lin::of(TOKEN * round);
        // the data a batch reads (see `reconstruct_pulled`), and the stored blocks written (a
        // block more) while it is held
        let (back, ahead, align) = (crate::BACK as u64, crate::AHEAD as u64, crate::ALIGN as u64);
        let span = batches
            .iter()
            .map(|b| {
                let lo = b[0].pos.saturating_sub(back) / align * align;
                b.last().map_or(n, |s| s.end).saturating_add(ahead).min(n) - lo
            })
            .max()
            .unwrap_or(0)
            .saturating_add(crate::ROUND as u64 + 65536)
            .min(n);
        // the writer's output of a round of blocks (`ROUND`, a block more: its tokens of 6 bytes at
        // most; the streams' bytes `l` at most), grown by pushes, and its room as it is taken
        let written = (crate::ROUND as u64 + (TOKEN * self.largest).max(65536)).min(l);
        let out = Lin::of(2 * written + (1 << 20) + 64);
        let ready = 3600 * nready;
        let runs = Lin::of(ready + 3 * (written + 16 * nready));
        let merge = Lin::of(batches.iter().map(|b| results(b)).max().unwrap_or(0));
        // the instance: all at once at most, or a batch's results with its decoding (no tasks)
        let work = |segs: &[Seg]| {
            segs.iter()
                .map(|s| self.decoder(s) + TOKEN * s.count)
                .max()
                .unwrap_or(0)
        };
        let alone = batches
            .iter()
            .map(|b| results(b) + work(b))
            .max()
            .unwrap_or(0);
        let inst = me
            .add(Lin::of(span))
            .add(pending)
            .add(out)
            .add(runs.max(merge).max(Lin::of(alone)));
        // a batch's tasks: how many, and the most they grow
        let tasks = batches
            .iter()
            .filter(|b| b.len() > 1)
            .map(|b| {
                let mut costs: Vec<u64> = b.iter().map(|s| self.task(s)).collect();
                costs.sort_unstable_by(|a, b| b.cmp(a));
                let k = costs.len().min(SLOTS);
                (k, costs[..k].iter().sum())
            })
            .collect();
        (inst, tasks)
    }
}

/// A group of the streams `par_jobs` runs together (a task each).
struct Group {
    items: Vec<usize>,
}

/// The groups of streams with data of `sizes` bytes, as `par_jobs` makes them on WebAssembly.
fn groups(sizes: &[u64]) -> Vec<Group> {
    let large = |i: &usize| sizes[*i] >= LARGE;
    let order: Vec<usize> = (0..sizes.len())
        .filter(large)
        .chain((0..sizes.len()).filter(|i| !large(i)))
        .collect();
    let mut out = Vec::new();
    let mut k = 0;
    while k < order.len() {
        let (start, mut sum) = (k, 0);
        while k < order.len() && (k == start || sum + sizes[order[k]] <= LARGE) {
            sum += sizes[order[k]];
            k += 1;
        }
        out.push(Group {
            items: order[start..k].to_vec(),
        });
    }
    out
}

/// What the module holds at most besides its windows: see [`Lin`] and the code of
/// [`crate::module::decode_part`].
///
/// The instance holds the parameters, the resources and the index copied, the lengths, what is
/// derived of the streams, the rules and the models or priors. A stream of [`LARGE`] bytes of
/// data or more is rebuilt from its data pulled ([`Stream::pulled`]), its record held. The others
/// go in runs of `module::RUN` bytes of data at most, their data and records held, rebuilt as
/// `decode` rebuilds them: in groups by `par_jobs` (a task each where there are several), each
/// group's streams in turn with the earlier ones' bytes held; then their bytes, which the output
/// bounds, held as they are written.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let derived = Derived::from_params(u.params).ok()?;
    let [data, records, index] = u.all_contents()?;
    let items = cut_items(data, records, index)?;
    let k = items.len() as u64;
    let l = u.output().checked_sub(k * derived.bytes() as u64)?;
    let profile = crate::record_profile(items[0].1).ok()?;
    let streams = items
        .iter()
        .map(|&(d, r)| Stream::parse(d.len() as u64, r, profile))
        .collect::<Option<Vec<_>>>()?;
    let m = crate::profile_data(profile).ok()?;
    let model = match m.coding {
        Coding::Static(_) => 9 * std::mem::size_of::<crate::code::Model>() as u64,
        Coding::Adaptive(_) => 3 * crate::adapt::NUM as u64,
    };
    // (the index and the lists grown as they are read or pushed: twice them)
    let lists = 2 * index.len() as u64
        + 2 * 16 * k
        + 2 * derived.bytes() as u64 * k
        + RULE_MEM * m.rules.len() as u64
        + model;
    let copied = u.params.len() as u64 + u.resources.iter().sum::<u64>();
    let inst = u.initial() + copied + lists + aqr_formula::SMALL + 4 * aqr_formula::PAGE;
    // The instance's memory at most (`high`), which tasks copy, all of it (WebAssembly's memory
    // never shrinks); then the tasks, of `grown` bytes besides each.
    let mut high = Lin::of(inst);
    let mut tasks: Vec<(usize, Lin)> = Vec::new();
    let alone = k == 1;
    let mut i = 0;
    while i < items.len() {
        if streams[i].n >= LARGE {
            let (peak, t) = streams[i].pulled(Lin::of(inst), items[i].1.len() as u64, l);
            if derived.before() {
                // its output held, grown by pushes (`Stream::most`, nor more than the output), and
                // no tasks
                let stream = streams[i].most().min(l);
                high = high.max(peak.add(Lin::of(2 * stream)));
            } else {
                high = high.max(peak);
                tasks.extend(t.into_iter().map(|(k, grown)| (k, Lin::of(grown))));
            }
            i += 1;
            continue;
        }
        let mut j = i;
        let mut held = 0;
        while j < items.len() && streams[j].n < LARGE && (j == i || held + streams[j].n <= RUN) {
            held += streams[j].n;
            j += 1;
        }
        let records: u64 = items[i..j].iter().map(|it| it.1.len() as u64).sum();
        // (the data and records, and the cuts)
        let w0 = Lin::of(inst + held + records + 32 * (j - i) as u64 + 2 * aqr_formula::PAGE);
        let sizes: Vec<u64> = streams[i..j].iter().map(|s| s.n).collect();
        let groups = groups(&sizes);
        // a group's streams in turn, the earlier ones' bytes held, from `w0`
        // (the streams' bytes `Stream::most` at most, nor more than the output)
        let group = |g: &Group, w0: Lin| {
            let (mut kept, mut peak) = (0, Lin::default());
            for &s in &g.items {
                peak = peak.max(streams[i + s].run(w0.add(Lin::of(kept.min(l))), 1, alone));
                kept += streams[i + s].most();
            }
            peak.max(w0.add(Lin::of(9 * g.items.len() as u64 + 16 + 2 * kept.min(l))))
        };
        // the run's streams (`Stream::most`, nor more than the output)
        let made = Lin::of(streams[i..j].iter().map(Stream::most).sum::<u64>().min(l));
        if derived.before() {
            // (no tasks: the groups in turn, the earlier ones' streams held)
            for g in &groups {
                high = high.max(group(g, w0.add(made)));
            }
        } else if groups.len() == 1 {
            high = high.max(group(&groups[0], w0));
        } else {
            // each group a task, up to SLOTS at once, growing as it runs from nothing
            let mut grown: Vec<Lin> = groups.iter().map(|g| group(g, Lin::default())).collect();
            grown.sort_unstable_by_key(|t| std::cmp::Reverse(t.a));
            let sum = grown
                .iter()
                .take(SLOTS)
                .fold(Lin::default(), |s, t| s.add(Lin::of(t.a)))
                .add(Lin::l(
                    grown.iter().take(SLOTS).map(|t| t.l).max().unwrap_or(0),
                ));
            tasks.push((grown.len().min(SLOTS), sum));
        }
        // the streams made, held as they are written
        high = high.max(w0.add(Lin::of(32 * (j - i) as u64)).add(made));
        i = j;
    }
    let mut peak = high;
    for (k, grown) in tasks {
        peak = peak.max(high.mul(k as u64 + 1).add(grown));
    }
    Some(peak.eval(l))
}

/// Bytes of data of the runs of small streams (as `module::RUN`).
const RUN: u64 = 4 * LARGE;

/// The application rebuilding streams of `profile` from `data` and `records` (what
/// [`crate::deconstruct`] gives, one after another) as `index` cuts them ([`index`]), of `len`
/// bytes with `derived` after them.
pub fn apply(
    profile: u16,
    data: Node,
    records: Node,
    index: Node,
    len: u64,
    derived: Derived,
) -> Result<Apply> {
    Ok(Apply {
        module: MODULE,
        resources: resources(profile)?,
        params: derived.params(),
        inputs: vec![data, records, index],
        len,
        memory_limit: aqr_formula::NO_LIMIT,
        task_limits: Vec::new(),
    })
}

/// The application rebuilding a single stream of `len` bytes from `data` and `record`, with
/// `derived` after it.
pub fn apply_one(
    profile: u16,
    data: Node,
    record: Node,
    len: u64,
    derived: Derived,
) -> Result<Node> {
    let len = len + derived.bytes() as u64;
    let a = apply(
        profile,
        data,
        record,
        Node::Concat(Vec::new()),
        len,
        derived,
    )?;
    Ok(Node::Apply(Box::new(a)))
}

/// A DEFLATE stream of `len` bytes to rebuild from data and a record of `profile`, of these
/// nodes and lengths.
pub struct Item {
    pub profile: u16,
    pub data: (Node, usize),
    pub record: (Node, usize),
    pub len: u64,
}

/// The streams of `items` in `f`, in order: by profile, the outputs of an application split
/// into them (or the application itself, for a single stream), their indexes streams of class
/// `flate/index`.
pub fn streams(f: &mut Formula, items: Vec<Item>) -> Result<Vec<Node>> {
    let mut profiles: Vec<u16> = items.iter().map(|i| i.profile).collect();
    profiles.sort_unstable();
    profiles.dedup();
    let mut out: Vec<Option<Node>> = (0..items.len()).map(|_| None).collect();
    let mut groups: Vec<(Vec<usize>, Vec<Item>)> =
        profiles.iter().map(|_| Default::default()).collect();
    for (k, item) in items.into_iter().enumerate() {
        let g = &mut groups[profiles.binary_search(&item.profile).unwrap()];
        g.0.push(k);
        g.1.push(item);
    }
    for (&p, (at, items)) in profiles.iter().zip(groups) {
        if let [one] = &items[..] {
            let (data, record) = (one.data.0.clone(), one.record.0.clone());
            out[at[0]] = Some(apply_one(p, data, record, one.len, Derived::None)?);
            continue;
        }
        let lens: Vec<(usize, usize)> = items.iter().map(|i| (i.data.1, i.record.1)).collect();
        let sizes: Vec<u64> = items.iter().map(|i| i.len).collect();
        let index = f.push("flate/index", crate::module::index(&lens));
        // (the data of each a content of its own, so not merged)
        let (data, records): (Vec<Node>, Vec<Node>) =
            items.into_iter().map(|i| (i.data.0, i.record.0)).unzip();
        let a = apply(
            p,
            Node::Concat(data),
            Node::concat(records),
            index,
            sizes.iter().sum(),
            Derived::None,
        )?;
        for (k, n) in at.into_iter().zip(f.split(a, sizes)) {
            out[k] = Some(n);
        }
    }
    Ok(out.into_iter().map(Option::unwrap).collect())
}

/// The resources of a profile: its built-in model data in binary.
pub fn resources(profile: u16) -> Result<Vec<Resource>> {
    static CATALOGS: [OnceLock<Resource>; crate::LAMBDAS.len()] =
        [const { OnceLock::new() }; crate::LAMBDAS.len()];
    static CODINGS: [OnceLock<Resource>; 1 + crate::PRIORS.len()] =
        [const { OnceLock::new() }; 1 + crate::PRIORS.len()];
    let m = crate::profile_data(profile)?;
    // the catalog's index, as in profile_catalog
    let k = (profile as usize).min(CATALOGS.len() - 1);
    // the default models (one for every static profile), or the priors
    let c = profile.saturating_sub(ADAPTIVE - 1) as usize;
    let catalog = CATALOGS[k].get_or_init(|| Resource::new(rules_to_bytes(m.rules)));
    let coding = CODINGS[c].get_or_init(|| {
        Resource::new(match m.coding {
            Coding::Static(models) => models_to_bytes(models),
            Coding::Adaptive(priors) => priors_to_bytes(priors),
        })
    });
    Ok(vec![catalog.clone(), coding.clone()])
}

/// A catalog's rules in binary: per rule, its fields in `RULE_BYTES` bytes.
pub fn rules_to_bytes(rules: &[Rule]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rules.len() * RULE_BYTES);
    for r in rules {
        out.extend_from_slice(&[r.kind as u8, r.key, r.ways, r.long, r.rank as u8]);
        out.extend_from_slice(&r.inner.to_le_bytes());
        out.extend_from_slice(&r.lazy.to_le_bytes());
        out.extend_from_slice(&[r.margin, r.min_len, r.far_len]);
        out.extend_from_slice(&r.far_dist.to_le_bytes());
        out.extend_from_slice(&r.window.to_le_bytes());
        out.extend_from_slice(&[r.depth, r.nice]);
    }
    out
}

/// Default models in binary: every field's [`code::Model::qs`].
pub fn models_to_bytes(models: &[code::Model; FIELDS]) -> Vec<u8> {
    models.iter().flat_map(|m| m.qs()).copied().collect()
}

/// Priors in binary: every context's probability (u16 LE), then every context's count (u8).
pub fn priors_to_bytes(p: &Priors) -> Vec<u8> {
    let mut out: Vec<u8> = p.p1.iter().flat_map(|p| p.to_le_bytes()).collect();
    out.extend_from_slice(&p.n);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The binary model data is the built-in one.
    #[test]
    fn binary_roundtrip() {
        for p in crate::profiles() {
            let m = crate::profile_data(p).unwrap();
            let [rules, coding] = &resources(p).unwrap()[..] else {
                panic!()
            };
            assert_eq!(rules_from_bytes(rules).unwrap(), m.rules);
            match m.coding {
                Coding::Static(models) => {
                    let back = models_from_bytes(coding).unwrap();
                    assert!(back.iter().zip(models).all(|(a, b)| a.qs() == b.qs()));
                }
                Coding::Adaptive(priors) => {
                    let back = priors_from_bytes(coding).unwrap();
                    assert_eq!((back.p1, back.n), (priors.p1.clone(), priors.n.clone()));
                }
            }
        }
        // Profiles share the default models, and 9 and 10 the catalog.
        assert_eq!(resources(0).unwrap()[1], resources(9).unwrap()[1]);
        assert_eq!(resources(9).unwrap()[0], resources(10).unwrap()[0]);
    }

    /// The record is read as `reconstruct_into` reads it, and the estimate covers the real work.
    #[test]
    fn memory_reads_records() {
        use crate::{Options, deconstruct_with, zlib};
        let mut data = Vec::new();
        let mut x = 12345u32;
        while data.len() < 700_000 {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let w = (x >> 16) % 400;
            data.extend_from_slice(format!("word{w} ").as_bytes());
        }
        let p = zlib::Params {
            level: 6,
            strategy: zlib::Strategy::Default,
            wbits: 15,
            mem: 8,
        };
        let raw = zlib::compress(&data, p).unwrap();
        for profile in [3u16, crate::ADAPTIVE] {
            let mut o = Options::for_profile(profile).unwrap();
            o.segment = 100_000;
            o.threads = 1;
            let (d, r) = deconstruct_with(&raw, &o).unwrap();
            let s = Stream::parse(d.len() as u64, &r, profile).expect("the record");
            assert!(s.segs.len() > 3, "{}", s.segs.len());
            assert_eq!(s.segs.iter().map(|g| g.count).sum::<u64>(), s.tokens);
            assert_eq!(s.segs.last().unwrap().end, d.len() as u64);
            let u = aqr_formula::Usage {
                params: &[],
                resources: &[],
                inputs: &[d.len() as u64, r.len() as u64, 0],
                outputs: &[raw.len() as u64],
                contents: &vec![&d[..], &r[..], &[][..]],
                initial_pages: 20,
            };
            let est = memory(&u).expect("an estimate");
            assert!(est > u.base() + raw.len() as u64, "{est}");
        }
    }
}
