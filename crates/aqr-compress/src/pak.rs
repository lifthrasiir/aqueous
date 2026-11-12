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

/// The plan of the contents of `src` ([`Plan`]): empty where nothing is to be taken apart.
pub(crate) fn plan<S: Source + ?Sized>(src: &S, stop: Option<&crate::archive::Stop>) -> Plan {
    // (`AQR_PAKS=0`: none, for a comparison)
    if std::env::var_os("AQR_PAKS").is_some_and(|v| v == "0") {
        return Plan::default();
    }
    let t0 = Instant::now();
    let n = src.len();
    // the contents of no format detected, by their head
    let mut heads: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
    let mut unknown = Vec::new();
    for c in 0..n {
        // (of a name telling no kind: the extensions known are formats' own, and a read of
        // each file before its batch delays the archive)
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
        unknown.push(c);
        if magic_like(&head[..8.min(head.len())]) {
            heads.entry(head[..8].to_vec()).or_default().push(c);
        }
    }
    if unknown.is_empty() {
        return Plan::default();
    }
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
        .chain(unknown.iter().filter(|&&c| src.size(c) >= ALONE).map(std::slice::from_ref))
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
        for (_, fam) in &keys {
            if stop.is_some_and(|s| s.is_requested()) {
                return plan;
            }
            looked += 1;
            if let Some(dirs) = directories(src, fam, &mut templates, true) {
                take(&mut plan, src, fam, &dirs);
            } else {
                retry.push(fam);
            }
        }
        for fam in retry {
            if let Some(dirs) = directories(src, fam, &mut templates, false) {
                take(&mut plan, src, fam, &dirs);
            }
        }
    }
    if std::env::var_os("AQR_PAK_VERBOSE").is_some() {
        eprintln!(
            "paks: {} unknown contents, {looked} families looked at, {} data files to take apart ({:.0} ms)",
            unknown.len(),
            plan.len(),
            t0.elapsed().as_secs_f64() * 1e3
        );
    }
    plan
}

/// The directories of family `fam` (its key first), by a template of `templates` or inferred
/// where `infer` (its template then learned), or None.
fn directories<S: Source + ?Sized>(
    src: &S,
    fam: &[usize],
    templates: &mut Vec<Template>,
    infer: bool,
) -> Option<Vec<Directory>> {
    let data: Vec<_> = fam.iter().map(|&c| src.read(c)).collect::<std::io::Result<_>>().ok()?;
    let files: Vec<File> = fam
        .iter()
        .zip(&data)
        .map(|(&c, d)| File { name: String::from_utf8_lossy(src.name(c)).into_owned(), data: &d[..] })
        .collect();
    if let Some(dirs) = templates.iter().find_map(|t| template::verify(t, &files, 0)) {
        return Some(dirs).filter(|d| !d.is_empty());
    }
    if !infer {
        return None;
    }
    let (dirs, _) = aqr_infer_pak::infer(&files, &aqr_infer_pak::Options::default());
    if let Some(t) = template::learn(&files, 0, &dirs) {
        templates.push(t);
    }
    Some(dirs).filter(|d| !d.is_empty())
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
