//! Files of unknown archive formats (game paks) whose directories aqr-infer-pak infers
//! (docs/pak-inference-design.md): planned for the contents to archive before their batches
//! are made ([`plan`]), then each data file taken apart where it is recompressed ([`encode`]):
//! its LZO1X members rebuilt from their data and their parse (aqr-lzo), its stored members as
//! they are, each member's data a content of its own.
//!
//! What is looked at: the contents of a name telling no kind ([`crate::kind_by_name`]) and of no
//! format detected (from their first 4 KiB) that start
//! alike, the same 8 bytes in [`GROUP`] of them at least (not text, not one byte): a group, each
//! with its family, the contents of its name's stem in its directory (an index and its data
//! file, The Crew's `.fat` and `.dat`). The group's biggest family is inferred and its layout
//! learned; the others verify that template, inferred (and learned from) where none holds. And
//! a content of [`ALONE`] bytes or more of no group, with its family, inferred alone. The other
//! contents cost a read of their first 4 KiB.
//!
//! Members coded otherwise (zstd: not rebuilt, `zst.rs`) are left as they are.

use std::collections::HashMap;
use std::time::Instant;

use aqr_formula::{Formula, Kind};
use aqr_infer_pak::codec::Codec;
use aqr_infer_pak::template::{self, Template};
use aqr_infer_pak::{Directory, File};

use crate::archive::Source;
use crate::{Cx, Options};

/// Contents starting alike that make a group.
const GROUP: usize = 3;
/// A content of no group inferred alone from this many bytes.
const ALONE: u64 = 16 << 20;
/// Contents shorter than this are not looked at (an index may be small: The Crew's of one
/// entry is 40 bytes).
const MIN: u64 = 16;
/// A family is inferred only if its biggest file has this many bytes (smaller ones are only
/// verified by the templates learned in their group): many contents start alike but are no
/// archives (Python's `.pyc`, gettext's `.mo`), some 17800 families of the 10 GB benchmark
/// inferred in 101 s before any batch was made.
const INFER: u64 = 4 << 20;
/// The inferences of a group that found nothing since its last directory found, after which
/// its other families are only verified by templates: those that tested rows of tables (about
/// 0.5 s a million) more than this many, and more than one for each [`GIVE_UP_BYTES`] of
/// their families (slower than about 120 MB/s). Data that looks like tables (textures) costs
/// a second or more a file (Ori DE: a group of 32 `.resS` files, 61 s), where inferences
/// finding nothing may still come before one that does (Subnautica's bundles: 23 big ones,
/// 11 million rows, before the second of 5 that find any, 66 found by their templates).
const GIVE_UP: u64 = 2_000_000;
const GIVE_UP_BYTES: u64 = 64;

/// The members of a data file to take apart: where they are, and whether they are LZO1X
/// streams (else stored).
#[derive(Clone)]
pub(crate) struct Layout {
    pub members: Vec<(usize, usize)>,
    pub lzo: bool,
}

/// The data files to take apart, by content.
#[derive(Default)]
pub(crate) struct Plan {
    by_content: HashMap<usize, Layout>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.by_content.is_empty()
    }

    pub fn len(&self) -> usize {
        self.by_content.len()
    }

    /// The plan of the contents of a batch, `(c, d)` content `c` of data `d`, by where their
    /// data is (as the contexts of the files see it).
    pub fn placed<'a>(&self, contents: impl IntoIterator<Item = (usize, &'a [u8])>) -> Placed {
        let by_at = contents
            .into_iter()
            .filter_map(|(c, d)| Some(((d.as_ptr() as usize, d.len()), self.by_content.get(&c)?.clone())))
            .collect();
        Placed { by_at }
    }
}

/// The data files of a batch to take apart, by where their data is and its length.
pub(crate) struct Placed {
    by_at: HashMap<(usize, usize), Layout>,
}

impl Placed {
    pub fn is_empty(&self) -> bool {
        self.by_at.is_empty()
    }

    fn get(&self, d: &[u8]) -> Option<&Layout> {
        self.by_at.get(&(d.as_ptr() as usize, d.len()))
    }
}

/// Whether 8 head bytes can tell a format: not zeros or one byte, not text.
fn magic_like(h: &[u8]) -> bool {
    h.len() == 8 && h.iter().any(|&x| x != h[0]) && !h.iter().all(|&x| (0x20..0x7f).contains(&x) || x == b'\n')
}

/// Whether bytes look like text: hardly any control bytes but tabs and line ends (UTF-8 and
/// other encodings' high bytes allowed).
fn texty(b: &[u8]) -> bool {
    let ctl = b.iter().filter(|&&x| (x < 0x20 && !matches!(x, b'\t' | b'\n' | b'\r')) || x == 0x7f).count();
    !b.is_empty() && ctl * 100 < b.len()
}

/// A name's directory and stem (lowercased): the family it is of.
fn family_key(name: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let cut = name.iter().rposition(|&b| b == b'/' || b == b'\\').map_or(0, |i| i + 1);
    let (dir, base) = name.split_at(cut);
    let stem = match base.iter().rposition(|&b| b == b'.') {
        Some(0) | None => base,
        Some(i) => &base[..i],
    };
    (dir.to_vec(), stem.to_ascii_lowercase())
}

/// The plan of a file of layouts: lines of a path, an offset and a size (tab-separated), a
/// member each, stored members; a content is planned whose name ends with the path (`/` or
/// `\` alike, case aside), its members in order, overlapping ones left out.
#[cfg(not(aqr_dist))]
fn given<S: Source + ?Sized>(src: &S, file: &std::path::Path) -> Plan {
    let norm =
        |s: &[u8]| -> Vec<u8> { s.iter().map(|&b| if b == b'\\' { b'/' } else { b.to_ascii_lowercase() }).collect() };
    let text = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("AQR_PAK_LAYOUT {}: {e}", file.display()));
    let mut by_path: HashMap<Vec<u8>, Vec<(usize, usize)>> = HashMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let mut f = line.split('\t');
        let (Some(p), Some(o), Some(s)) = (f.next(), f.next(), f.next()) else { panic!("AQR_PAK_LAYOUT: {line:?}") };
        let (o, s) = (o.trim().parse().expect("offset"), s.trim().parse().expect("size"));
        by_path.entry(norm(p.as_bytes())).or_default().push((o, s));
    }
    let mut plan = Plan::default();
    for c in 0..src.len() {
        let name = norm(src.name(c));
        let Some(ms) = by_path.iter().find(|(p, _)| name.ends_with(p)).map(|(_, m)| m) else { continue };
        let mut ms = ms.clone();
        ms.sort_unstable();
        let len = src.size(c) as usize;
        let mut members = Vec::with_capacity(ms.len());
        let mut end = 0;
        for (o, s) in ms {
            if s > 0 && o >= end && o + s <= len {
                members.push((o, s));
                end = o + s;
            }
        }
        if !members.is_empty() {
            plan.by_content.insert(c, Layout { members, lzo: false });
        }
    }
    if std::env::var_os("AQR_PAK_VERBOSE").is_some() {
        eprintln!("paks: {} data files given by {}", plan.len(), file.display());
    }
    plan
}

/// The plan of the contents of `src` ([`Plan`]): empty where nothing is to be taken apart.
pub(crate) fn plan<S: Source + ?Sized>(src: &S, stop: Option<&crate::archive::Stop>, threads: usize) -> Plan {
    // (`AQR_PAKS=0`: none, for a comparison)
    if std::env::var_os("AQR_PAKS").is_some_and(|v| v == "0") {
        return Plan::default();
    }
    // (`AQR_PAK_LAYOUT=FILE`: the layouts in FILE, not inferred, for the ideal a measurement
    // of the inference is against; not in builds for distribution)
    #[cfg(not(aqr_dist))]
    if let Some(p) = std::env::var_os("AQR_PAK_LAYOUT") {
        return given(src, std::path::Path::new(&p));
    }
    let t0 = Instant::now();
    // (`AQR_PAK_VERBOSE=2`: each family looked at too)
    let each = std::env::var_os("AQR_PAK_VERBOSE").is_some_and(|v| v == "2");
    let n = src.len();
    // the contents of no format detected, by their head
    let mut heads: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
    let mut unknown = Vec::new();
    let mut text = Vec::new();
    for c in 0..n {
        // (of a name telling no kind: the extensions known are formats' own, and a read of
        // each file before its batch delays the archive; read on one thread, as reads of many
        // small files on several were slower)
        if src.size(c) < MIN || crate::kind_by_name(src.name(c)) != Kind::Unknown {
            continue;
        }
        if stop.is_some_and(|s| s.is_requested()) {
            return Plan::default();
        }
        let Ok(head) = src.read_range(c, 0..4096) else { continue };
        if crate::detect(&head).is_some() {
            continue;
        }
        // (a big one inferred alone only where its head or tail is no text: a directory is
        // binary, at the one or the other; the 10 GB benchmark's primes, 57 MB of numbers,
        // took 5 s)
        let size = src.size(c);
        if size >= ALONE && texty(&head) && src.read_range(c, size - 4096..size).is_ok_and(|t| texty(&t)) {
            text.push(c);
        }
        unknown.push(c);
        if magic_like(&head[..8.min(head.len())]) {
            heads.entry(head[..8].to_vec()).or_default().push(c);
        }
    }
    if unknown.is_empty() {
        return Plan::default();
    }
    let heads_time = t0.elapsed();
    let mut families: HashMap<(Vec<u8>, Vec<u8>), Vec<usize>> = HashMap::new();
    for c in 0..n {
        families.entry(family_key(src.name(c))).or_default().push(c);
    }
    let family = |c: usize| -> Vec<usize> {
        let mut f = vec![c];
        f.extend(families[&family_key(src.name(c))].iter().copied().filter(|&o| o != c));
        f
    };
    // the groups, the biggest first, each family once; then the big contents of none
    let mut groups: Vec<Vec<usize>> = heads.into_values().filter(|g| g.len() >= GROUP).collect();
    groups.sort_by_key(|g| std::cmp::Reverse(g.len()));
    let mut done = std::collections::HashSet::new();
    let mut plan = Plan::default();
    let mut looked = 0usize;
    for g in groups
        .iter()
        .map(|g| g.as_slice())
        .chain(unknown.iter().filter(|&&c| src.size(c) >= ALONE && !text.contains(&c)).map(std::slice::from_ref))
    {
        let mut keys: Vec<(usize, Vec<usize>)> = g
            .iter()
            .map(|&c| (c, family(c)))
            .filter(|(_, fam)| {
                let mut k = fam.clone();
                k.sort_unstable();
                done.insert(k)
            })
            .collect();
        keys.sort_by_key(|(_, fam)| std::cmp::Reverse(fam.iter().map(|&c| src.size(c)).sum::<u64>()));
        let mut templates: Vec<Template> = Vec::new();
        let mut retry = Vec::new();
        let mut wasted = (0u64, 0u64);
        for (_, fam) in &keys {
            if stop.is_some_and(|s| s.is_requested()) {
                return plan;
            }
            let infer = wasted.0 < GIVE_UP.max(wasted.1 / GIVE_UP_BYTES)
                && fam.iter().map(|&c| src.size(c)).max().unwrap_or(0) >= INFER;
            looked += infer as usize;
            let t = Instant::now();
            let (dirs, rows) = directories(src, fam, &mut templates, infer, threads);
            if each {
                eprintln!(
                    "  {} ({} in group, {} B): {} in {:.0} ms, {rows} rows",
                    String::from_utf8_lossy(src.name(fam[0])),
                    g.len(),
                    src.size(fam[0]),
                    dirs.as_ref().map_or(0, |d| d.len()),
                    t.elapsed().as_secs_f64() * 1e3
                );
            }
            if let Some(dirs) = dirs {
                take(&mut plan, src, fam, &dirs);
                wasted = (0, 0);
            } else {
                if infer {
                    wasted.0 += rows;
                    wasted.1 += fam.iter().map(|&c| src.size(c)).sum::<u64>();
                }
                retry.push(fam);
            }
        }
        for fam in retry {
            if let (Some(dirs), _) = directories(src, fam, &mut templates, false, threads) {
                take(&mut plan, src, fam, &dirs);
            }
        }
    }
    if std::env::var_os("AQR_PAK_VERBOSE").is_some() {
        eprintln!(
            "paks: {} unknown contents, {looked} families inferred, {} data files to take apart ({:.0} ms, \
             heads {:.0} ms)",
            unknown.len(),
            plan.len(),
            t0.elapsed().as_secs_f64() * 1e3,
            heads_time.as_secs_f64() * 1e3
        );
    }
    // (`AQR_PAK_DUMP=FILE`: the plan written as `AQR_PAK_LAYOUT` reads it)
    #[cfg(not(aqr_dist))]
    if let Some(p) = std::env::var_os("AQR_PAK_DUMP") {
        let mut out = String::new();
        for (&c, layout) in &plan.by_content {
            let name = String::from_utf8_lossy(src.name(c));
            for &(o, s) in &layout.members {
                out.push_str(&format!("{name}\t{o}\t{s}\n"));
            }
        }
        std::fs::write(&p, out).unwrap_or_else(|e| panic!("AQR_PAK_DUMP: {e}"));
    }
    plan
}

/// The directories of family `fam` (its key first), by a template of `templates` or inferred
/// where `infer` (its template then learned), or None; and the rows of the tables the
/// inference tested.
fn directories<S: Source + ?Sized>(
    src: &S,
    fam: &[usize],
    templates: &mut Vec<Template>,
    infer: bool,
    threads: usize,
) -> (Option<Vec<Directory>>, u64) {
    if !infer && templates.is_empty() {
        return (None, 0);
    }
    let Ok(data) = fam.iter().map(|&c| src.read(c)).collect::<std::io::Result<Vec<_>>>() else { return (None, 0) };
    let files: Vec<File> = fam
        .iter()
        .zip(&data)
        .map(|(&c, d)| File { name: String::from_utf8_lossy(src.name(c)).into_owned(), data: &d[..] })
        .collect();
    if let Some(dirs) = templates.iter().find_map(|t| template::verify(t, &files, 0)) {
        return (Some(dirs).filter(|d| !d.is_empty()), 0);
    }
    if !infer {
        return (None, 0);
    }
    let (dirs, times) = aqr_infer_pak::infer(&files, &aqr_infer_pak::Options { threads, ..Default::default() });
    if let Some(t) = template::learn(&files, 0, &dirs) {
        templates.push(t);
    }
    (Some(dirs).filter(|d| !d.is_empty()), times.rows)
}

/// The data files of directories `dirs` of family `fam` to take apart: those of members in
/// the file (not in data decoded from it) LZO1X or stored, apart (shared ones once, those
/// overlapping another left out).
fn take<S: Source + ?Sized>(plan: &mut Plan, src: &S, fam: &[usize], dirs: &[Directory]) {
    for d in dirs {
        if d.parent.is_some() || !matches!(d.codec, None | Some(Codec::Lzo1x)) {
            continue;
        }
        let c = fam[d.target_file];
        if plan.by_content.contains_key(&c) {
            continue;
        }
        let mut ms: Vec<(usize, usize)> =
            d.members.iter().filter(|m| m.size > 0).map(|m| (m.offset as usize, m.size as usize)).collect();
        ms.sort_unstable();
        ms.dedup_by_key(|m| m.0);
        let mut apart = Vec::with_capacity(ms.len());
        let mut end = 0;
        for (o, s) in ms {
            if o >= end && o + s <= d.target_len as usize {
                apart.push((o, s));
                end = o + s;
            }
        }
        let len = src.size(c) as usize;
        if !apart.is_empty() && len as u64 == d.target_len {
            plan.by_content.insert(c, Layout { members: apart, lzo: d.codec == Some(Codec::Lzo1x) });
        }
    }
}

/// The formula of `d`, a data file of the plan, its members apart ([`aqr_lzo::formula::encode_blocks`]).
pub(crate) fn encode(d: &[u8], _: Kind, opt: &Options, _: u32, cx: &Cx) -> Option<Formula> {
    let layout = cx.paks.as_ref()?.get(d)?;
    let blocks: Vec<aqr_lzo::formula::Block> = layout
        .members
        .iter()
        .map(|&(at, len)| aqr_lzo::formula::Block { at, len, data: None, stored: !layout.lzo })
        .collect();
    aqr_lzo::formula::encode_blocks(d, &blocks, opt.threads, true)
}

/// Whether the plan has `d`, a file's data.
pub(crate) fn has(d: &[u8], cx: &Cx) -> bool {
    cx.paks.as_ref().is_some_and(|p| p.get(d).is_some())
}
