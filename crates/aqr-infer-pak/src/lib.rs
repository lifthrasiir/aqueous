//! Directories of unknown archive formats (game paks) inferred, nothing about the format
//! known: docs/pak-inference-design.md. A directory is a table of fixed-size records some of
//! whose fields point into a *target* (a file of the family, or one decoded in place), and it
//! is recognised by what the pointers do there: the members tile the target.
//!
//! - [`tables`]: tables of records in the files' heads and tails;
//! - [`fields`]: the candidate fields of a table, from its bit profile;
//! - [`tiling`]: the tests (offsets and sizes, or sizes laid end to end) and their evidence;
//! - [`codec`]: the members' codecs;
//! - [`target`]: files and targets decoded in place, which the tables are tested on in turn.

pub mod codec;
pub mod fields;
pub mod tables;
pub mod target;
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
    /// The file holding the table.
    pub owner: String,
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
}

impl Default for Options {
    fn default() -> Self {
        Options { keep: 16, per: 4, tries: 16, verbose: false }
    }
}

/// Share of the target from its first member on, and of the whole, the members must fill:
/// fields shifted down (offsets and sizes both divided by 2^j) tile too, but only a part.
const COVER: f64 = 0.9;
const SAMPLE: usize = 24;

/// The values of a row's 16-, 32- and 64-bit fields (a decoded size must be among them for
/// an LZ4 block).
fn hints(b: &[u8], t: &Table, row: i64) -> Vec<u64> {
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

fn filled(ms: &[Member]) -> u64 {
    let mut v: Vec<(u64, u64)> = ms.iter().map(|m| (m.offset, m.size)).collect();
    v.sort_unstable();
    v.dedup_by_key(|x| x.0);
    v.iter().map(|x| x.1).sum()
}

fn kind_of(x: &[u8]) -> String {
    let h = &x[..x.len().min(4)];
    if h.len() == 4 && h.iter().all(|&c| (32..127).contains(&c)) {
        String::from_utf8_lossy(h).into_owned()
    } else {
        h.iter().map(|c| format!("{c:02x}")).collect()
    }
}

fn tally<T: Eq + Clone>(v: &[T]) -> Vec<(T, usize)> {
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
        tables.extend(tables::of_file(f.data, opt.keep, opt.per).into_iter().map(|t| (i, t)));
    }
    times.tables += t0.elapsed();
    let mut profiles: Vec<Option<Profile>> = Vec::new();
    let mut targets: Vec<Target> = files.iter().enumerate().map(|(i, f)| Target::file(&f.name, i, f.data)).collect();
    let mut dirs: Vec<Directory> = Vec::new();
    let mut ti = 0;
    while ti < targets.len() {
        let mut new_targets = Vec::new();
        let tg = &targets[ti];
        let l = tg.len;
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
            if profiles.len() <= j {
                profiles.resize_with(j + 1, || None);
            }
            let tp = Instant::now();
            let prof = profiles[j].get_or_insert_with(|| Profile::new(b, &t));
            times.profiles += tp.elapsed();
            let te = Instant::now();
            let sizes = tiling::sizes(b, &t, &prof.candidates(l), l);
            let dsz = te.elapsed();
            let mut cands = tiling::explicit(b, &t, prof, l, &sizes);
            cands.truncate(opt.tries);
            let dex = te.elapsed();
            times.explicit += dex;
            let tm = Instant::now();
            let run = tiling::run_sizes(b, &t, &sizes, l);
            let mut imp = tiling::implicit(b, &t, &run, l, tg.is_file() && tg.file == owner);
            imp.truncate(opt.tries);
            let dim = tm.elapsed();
            times.implicit += dim;
            if opt.verbose {
                eprintln!(
                    "    {} by {} {:#x}..{:#x}/{} ({} rows): {} sizes ({:.1} ms), {} explicit ({:.1} ms), {} implicit ({:.1} ms)",
                    tg.name,
                    files[owner].name,
                    t.start,
                    t.end,
                    t.k,
                    t.rows(),
                    sizes.len(),
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
                let more = tables::rank(tables::find(b, a0.saturating_sub(1 << 16), a0), b.len(), 4, 4);
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
                let src = files[tg.file].data;
                let mut ms = best.members.clone();
                ms.sort_by_key(|m| m.offset);
                ms.dedup_by_key(|m| m.offset);
                let mut parts = Vec::new();
                let mut at = 0usize;
                for m in &ms {
                    let (o, s) = (m.offset as usize, m.size as usize);
                    if o < at {
                        continue;
                    }
                    if o > at && src[at..o].iter().any(|&x| x != 0) {
                        parts.push(Part { len: (o - at) as u64, at, src_len: o - at, codec: None });
                    }
                    let blob = &src[o..o + s];
                    let (len, k) = match codec::decoded_size(blob) {
                        Some(n) => (n, Some(codec::Codec::Zstd)),
                        None => match codec::probe(blob, &hints(b, &best.cand.table, m.row)) {
                            Some((k, d)) => (d.len() as u64, Some(k)),
                            None => (s as u64, None),
                        },
                    };
                    parts.push(Part { len, at: o, src_len: s, codec: k });
                    at = o + s;
                }
                let v = Target::decoded(name, tg.file, src, parts);
                decoded = Some(v.len);
                new_targets.push(v);
                times.decode += td.elapsed();
            }
            dirs.push(Directory {
                target: tg.name.clone(),
                target_len: l,
                owner: files[owner].name.clone(),
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
