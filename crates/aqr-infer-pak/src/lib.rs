//! Directories of unknown archive formats (game paks) inferred, nothing about the format
//! known: docs/pak-inference-design.md. A directory is a table of fixed-size records some of
//! whose fields point into a *target* (a file of the family, or one decoded in place), and it
//! is recognised by what the pointers do there: the members tile the target.
//!
//! - [`tables`]: tables of records in the files' heads and tails;
//! - [`fields`]: the candidate fields of a table, from its bit profile;
//! - [`tiling`]: the tests (offsets and sizes, or sizes laid end to end) and their evidence;
//! - [`codec`]: the members' codecs;
//! - [`target`]: files and targets decoded in place, which the tables are tested on in turn;
//! - [`template`]: the layout found in one family, verified in another of the same format.

pub mod codec;
pub mod fields;
pub mod tables;
pub mod target;
pub mod template;
pub mod tiling;

use std::time::{Duration, Instant};

use fields::{Profile, word};
use tables::Table;
use target::{Part, Target};
use tiling::{Cand, EVIDENCE, Member};

/// A file of the family (files of one stem in one directory).
pub struct File<'a> {
    pub name: String,
    pub data: &'a [u8],
}

pub struct Directory {
    pub target: String,
    pub target_len: u64,
    /// The file the target is or comes from, by index in the family; and the directory whose
    /// members, decoded in place, the target is (where it is no file).
    pub target_file: usize,
    pub parent: Option<usize>,
    /// The file holding the table, by name and index in the family.
    pub owner: String,
    pub owner_file: usize,
    /// The codec the members are of (the most frequent in a sample; some may be stored).
    pub codec: Option<codec::Codec>,
    pub cand: Cand,
    pub members: Vec<Member>,
    pub ev: f64,
    /// Share of the target the members (shared ones once) fill.
    pub cover: f64,
    /// Codecs of a sample of the members, and their contents' first bytes.
    pub codecs: Vec<(&'static str, usize)>,
    pub contents: Vec<(String, usize)>,
    /// The length of the target decoded in place, where it was made one.
    pub decoded: Option<u64>,
}

#[derive(Default, Debug, Clone, Copy)]
pub struct Times {
    pub tables: Duration,
    pub profiles: Duration,
    pub explicit: Duration,
    pub implicit: Duration,
    pub codecs: Duration,
    pub decode: Duration,
}

pub struct Options {
    /// Tables of a file tested, and for one region of it.
    pub keep: usize,
    pub per: usize,
    /// Candidates of each kind a table's best is chosen among (by their members' codecs).
    pub tries: usize,
    /// Tells on standard error what each table gave.
    pub verbose: bool,
    /// Threads the tables are found and tested on.
    pub threads: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            keep: 16,
            per: 4,
            tries: 16,
            verbose: false,
            threads: std::thread::available_parallelism().map_or(1, |n| n.get()),
        }
    }
}

/// Share of the target from its first member on, and of the whole, the members must fill:
/// fields shifted down (offsets and sizes both divided by 2^j) tile too, but only a part.
const COVER: f64 = 0.9;
const SAMPLE: usize = 24;

/// The values of a row's 16-, 32- and 64-bit fields (a decoded size must be among them for
/// an LZ4 block).
pub(crate) fn hints(b: &[u8], t: &Table, row: i64) -> Vec<u64> {
    let p = t.start as i64 + row * t.k as i64;
    let mut v = Vec::with_capacity(6 * t.k);
    for i in 0..t.k as i64 {
        for be in [false, true] {
            let w = word(b, p + i, be);
            if be {
                v.extend([w >> 48, w >> 32, w]);
            } else {
                v.extend([w & 0xffff, w & 0xffff_ffff, w]);
            }
        }
    }
    v
}

pub(crate) fn filled(ms: &[Member]) -> u64 {
    let mut v: Vec<(u64, u64)> = ms.iter().map(|m| (m.offset, m.size)).collect();
    v.sort_unstable();
    v.dedup_by_key(|x| x.0);
    v.iter().map(|x| x.1).sum()
}

pub(crate) fn kind_of(x: &[u8]) -> String {
    let h = &x[..x.len().min(4)];
    if h.len() == 4 && h.iter().all(|&c| (32..127).contains(&c)) {
        String::from_utf8_lossy(h).into_owned()
    } else {
        h.iter().map(|c| format!("{c:02x}")).collect()
    }
}

pub(crate) fn tally<T: Eq + Clone>(v: &[T]) -> Vec<(T, usize)> {
    let mut out: Vec<(T, usize)> = Vec::new();
    for x in v {
        match out.iter_mut().find(|(y, _)| y == x) {
            Some(e) => e.1 += 1,
            None => out.push((x.clone(), 1)),
        }
    }
    out.sort_by_key(|x| std::cmp::Reverse(x.1));
    out
}

/// The target of `members` (of a table of `b`) in file `file` (`src`), decoded in place: the
/// bytes before the first member and between members kept unless they are zeros (padding).
/// Where the members' codec is known, a member is of it or stored (a pak stores the chunks zstd
/// does not make smaller: probing each for every codec took seconds of a texture pak's).
pub fn decode_in_place<'a>(
    name: String,
    file: usize,
    src: &'a [u8],
    members: &[Member],
    b: &[u8],
    table: &Table,
    known: Option<codec::Codec>,
) -> Target<'a> {
    let mut ms = members.to_vec();
    ms.sort_by_key(|m| m.offset);
    ms.dedup_by_key(|m| m.offset);
    let mut parts = Vec::new();
    let mut at = 0usize;
    for m in &ms {
        let (o, s) = (m.offset as usize, m.size as usize);
        if o < at || o + s > src.len() {
            continue;
        }
        if o > at && src[at..o].iter().any(|&x| x != 0) {
            parts.push(Part { len: (o - at) as u64, at, src_len: o - at, codec: None });
        }
        let blob = &src[o..o + s];
        let (len, k) = match (known, blob) {
            // (by its header alone: the directory knows the member's bounds)
            (Some(codec::Codec::Zstd), _) => match codec::zstd_header_size(blob) {
                Some(n) => (n, Some(codec::Codec::Zstd)),
                None => (s as u64, None),
            },
            (_, _) if let Some(n) = codec::decoded_size(blob) => (n, Some(codec::Codec::Zstd)),
            (Some(c), _) => match codec::decode(c, blob) {
                Some(d) => (d.len() as u64, Some(c)),
                None => (s as u64, None),
            },
            (None, _) => match codec::probe(blob, &hints(b, table, m.row)) {
                Some((k, d)) => (d.len() as u64, Some(k)),
                None => (s as u64, None),
            },
        };
        parts.push(Part { len, at: o, src_len: s, codec: k });
        at = o + s;
    }
    Target::decoded(name, file, src, parts)
}

/// The tests of a table ([`test_table`]): how many sizes passed the gate, the candidates of
/// each kind, and their times.
struct Tests {
    sizes: usize,
    cands: Vec<Cand>,
    imp: Vec<Cand>,
    dsz: Duration,
    dex: Duration,
    dim: Duration,
}

/// The tests of table `t` of file `owner` as a directory of a target of length `l` (`same`:
/// the table's own file).
fn test_table(files: &[File], (owner, t): (usize, Table), l: u64, same: bool, opt: &Options) -> Tests {
    let b = files[owner].data;
    let prof = Profile::new(b, &t);
    let te = Instant::now();
    let sizes = tiling::sizes(b, &t, &prof.candidates(l), l);
    let dsz = te.elapsed();
    let mut cands = tiling::explicit(b, &t, &prof, l, &sizes);
    cands.truncate(opt.tries);
    let dex = te.elapsed();
    let tm = Instant::now();
    let run = tiling::run_sizes(b, &t, &sizes, l);
    let mut imp = tiling::implicit(b, &t, &run, l, same);
    imp.truncate(opt.tries);
    Tests { sizes: sizes.len(), cands, imp, dsz, dex, dim: tm.elapsed() }
}

/// `f` of 0..n on up to `threads` threads, in order.
pub(crate) fn par_map<T: Send>(n: usize, threads: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    if threads <= 1 || n <= 1 {
        return (0..n).map(f).collect();
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out: Vec<std::sync::Mutex<Option<T>>> = (0..n).map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..threads.min(n) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    *out[i].lock().unwrap() = Some(f(i));
                }
            });
        }
    });
    out.into_iter().map(|m| m.into_inner().unwrap().unwrap()).collect()
}

struct Tested {
    ev: f64,
    cand: Cand,
    members: Vec<Member>,
    codecs: Vec<Option<codec::Codec>>,
    contents: Vec<String>,
}

/// The directories of a family.
pub fn infer(files: &[File], opt: &Options) -> (Vec<Directory>, Times) {
    let mut times = Times::default();
    let t0 = Instant::now();
    let mut tables: Vec<(usize, Table)> = Vec::new();
    for (i, f) in files.iter().enumerate() {
        tables.extend(tables::of_file(f.data, opt.keep, opt.per, opt.threads).into_iter().map(|t| (i, t)));
    }
    times.tables += t0.elapsed();
    let mut targets: Vec<Target> = files.iter().enumerate().map(|(i, f)| Target::file(&f.name, i, f.data)).collect();
    let mut dirs: Vec<Directory> = Vec::new();
    let mut ti = 0;
    while ti < targets.len() {
        let mut new_targets = Vec::new();
        let tg = &targets[ti];
        let l = tg.len;
        // the tests of the tables there are, at once (taken in order below)
        let tp = Instant::now();
        let (tg_file, tg_is_file, tg_parent) = (tg.file, tg.is_file(), tg.parent);
        let skip = |j: usize| {
            let (owner, t) = tables[j];
            let overlap = |u: &Table| t.end.min(u.end).saturating_sub(t.start.max(u.start)) * 2 >= t.end - t.start;
            tg_parent.is_some_and(|p| dirs[p].owner_file == owner && overlap(&dirs[p].cand.table))
        };
        let mut tested: Vec<Option<Tests>> = par_map(tables.len(), opt.threads, |j| {
            (!skip(j)).then(|| test_table(files, tables[j], l, tg_is_file && tg_file == tables[j].0, opt))
        });
        times.explicit += tp.elapsed();
        let mut j = 0;
        while j < tables.len() {
            let (owner, t) = tables[j];
            let b = files[owner].data;
            // another view of a table already found a directory of this target: skip it
            let overlap = |u: &Table| t.end.min(u.end).saturating_sub(t.start.max(u.start)) * 2 >= t.end - t.start;
            if dirs.iter().any(|d| d.target == tg.name && d.owner == files[owner].name && overlap(&d.cand.table)) {
                j += 1;
                continue;
            }
            // nor the directory a decoded target is made of: its offsets are of the target before
            // it was decoded (members that decode to about their size tile both)
            if tg.parent.is_some_and(|p| dirs[p].owner_file == owner && overlap(&dirs[p].cand.table)) {
                j += 1;
                continue;
            }
            let Tests { sizes, mut cands, imp, dsz, dex, dim } = match tested.get_mut(j).and_then(Option::take) {
                Some(x) => x,
                None => test_table(files, (owner, t), l, tg.is_file() && tg.file == owner, opt),
            };
            if opt.verbose {
                eprintln!(
                    "    {} by {} {:#x}..{:#x}/{} ({} rows): {} sizes ({:.1} ms), {} explicit ({:.1} ms), {} implicit ({:.1} ms)",
                    tg.name,
                    files[owner].name,
                    t.start,
                    t.end,
                    t.k,
                    t.rows(),
                    sizes,
                    dsz.as_secs_f64() * 1e3,
                    cands.len(),
                    dex.as_secs_f64() * 1e3,
                    imp.len(),
                    dim.as_secs_f64() * 1e3
                );
            }
            cands.extend(imp);
            j += 1;
            // the best by its members' codecs too (decoding a member exactly is evidence)
            let tc = Instant::now();
            let mut best: Option<Tested> = None;
            for c in cands {
                let ms = tiling::members(b, &c, l);
                if ms.is_empty() {
                    continue;
                }
                let fill = filled(&ms);
                let first = ms.iter().map(|m| m.offset).min().unwrap();
                if (fill as f64) < COVER * (l - first) as f64 || fill < l / 2 {
                    continue;
                }
                let idx: Vec<usize> = if ms.len() <= SAMPLE {
                    (0..ms.len()).collect()
                } else {
                    (0..SAMPLE).map(|i| i * (ms.len() - 1) / (SAMPLE - 1)).collect()
                };
                let mut codecs = Vec::new();
                let mut contents = Vec::new();
                for &i in &idx {
                    let m = ms[i];
                    let blob = tg.read(m.offset, m.size);
                    if blob.is_empty() {
                        continue;
                    }
                    match codec::probe(&blob, &hints(b, &c.table, m.row)) {
                        Some((k, d)) => {
                            codecs.push(Some(k));
                            contents.push(kind_of(&d));
                        }
                        None => {
                            codecs.push(None);
                            contents.push(kind_of(&blob));
                        }
                    }
                }
                let coded = codecs.iter().filter(|x| x.is_some()).count();
                // a decoder may succeed on a member by chance: the codecs count only where
                // most of the sample decodes
                let bonus = if coded * 2 >= codecs.len() {
                    16.0 * coded as f64 * ms.len() as f64 / codecs.len().max(1) as f64
                } else {
                    0.0
                };
                let ev = c.ev + bonus;
                if best.as_ref().is_none_or(|x| ev > x.ev) {
                    best = Some(Tested { ev, cand: c, members: ms, codecs, contents });
                }
            }
            times.codecs += tc.elapsed();
            let Some(best) = best else { continue };
            if opt.verbose {
                eprintln!("      best {:.0} bits, {} members", best.ev, best.members.len());
            }
            if best.ev < EVIDENCE {
                continue;
            }
            // the same directory by another view of its table: its members nearly the same
            let sig: std::collections::HashSet<(u64, u64)> = best.members.iter().map(|m| (m.offset, m.size)).collect();
            if dirs.iter().any(|d| {
                d.target == tg.name && {
                    let common = d.members.iter().filter(|m| sig.contains(&(m.offset, m.size))).count();
                    common * 20 >= sig.len().max(d.members.len()) * 19
                }
            }) {
                continue;
            }
            // the bytes before the directory, on their own: a smaller table met it there
            let a0 = best.cand.table.start;
            if a0 > 64 {
                let tt = Instant::now();
                let more = tables::rank(tables::find(b, a0.saturating_sub(1 << 16), a0, opt.threads), b.len(), 4, 4);
                for u in more {
                    if !tables.iter().any(|(o, x)| *o == owner && x.same(&u)) {
                        tables.push((owner, u));
                    }
                }
                times.tables += tt.elapsed();
            }
            let coded = best.codecs.iter().filter(|x| x.is_some()).count();
            let cover = filled(&best.members) as f64 / l as f64;
            let mut decoded = None;
            let name = format!("{}~decoded", tg.name);
            if coded * 10 >= best.codecs.len() * 9
                && cover > COVER
                && tg.is_file()
                && !targets.iter().chain(&new_targets).any(|x| x.name == name)
            {
                // the target decoded in place: a target in turn
                let td = Instant::now();
                let known = tally(&best.codecs).into_iter().find_map(|(c, _)| c);
                let v = decode_in_place(name, tg.file, files[tg.file].data, &best.members, b, &best.cand.table, known)
                    .with_parent(dirs.len());
                decoded = Some(v.len);
                new_targets.push(v);
                times.decode += td.elapsed();
            }
            // the codec among the members (some may be stored: chunks zstd did not make smaller)
            let majority = tally(&best.codecs).into_iter().find_map(|(c, _)| c);
            dirs.push(Directory {
                target: tg.name.clone(),
                target_len: l,
                target_file: tg.file,
                parent: tg.parent,
                owner: files[owner].name.clone(),
                owner_file: owner,
                codec: majority,
                cand: best.cand,
                ev: best.ev,
                cover,
                codecs: tally(&best.codecs.iter().map(|c| c.map_or("stored", |c| c.name())).collect::<Vec<_>>()),
                contents: tally(&best.contents),
                members: best.members,
                decoded,
            });
        }
        targets.extend(new_targets);
        ti += 1;
    }
    (dirs, times)
}
