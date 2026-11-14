//! Templates: the layout of the directories inferred in one family, verified in another of the
//! same format (its files starting with the same magic). A verification is no search, and
//! needs no evidence of its own: a directory of a single row is found where its template
//! holds.
//!
//! A template's directory is where its table starts (at a fixed offset, or right after
//! another directory's table), how many rows it has (a field of the header that told it, else
//! as many as fit), its stride and fields (from the record's start), which files it is in and
//! points into (the family's key file, the one starting with the magic, or another by its
//! extension; or a target decoded in place by another directory), the base its members are
//! laid from (for sizes end to end), and the codec of its members.

use crate::codec::{self, Codec};
use crate::fields::{Field, word};
use crate::tables::Table;
use crate::target::Target;
use crate::tiling::{self, Cand, Layout, Member};
use crate::{Directory, File, decode_in_place, filled, kind_of, tally};

/// A file of a family: the key file (where the magic is), or another by its extension.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Role {
    Key,
    Other(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// At this offset of its file.
    At(usize),
    /// This many bytes after the end of another directory's table (by index).
    After { dir: usize, gap: usize },
}

/// A field of the header that holds the table's rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Count {
    pub pos: usize,
    pub bytes: u8,
    pub be: bool,
}

#[derive(Clone, Debug)]
pub struct TDir {
    pub owner: Role,
    /// The target: a file, or the target of another directory (by index) decoded in place.
    pub target: Role,
    pub decoded: Option<usize>,
    pub anchor: Anchor,
    pub count: Option<Count>,
    /// The rows it had where it was learned.
    pub rows: usize,
    pub k: usize,
    /// Fields from the record's start; an implicit layout's base is replaced by `base`.
    pub layout: Layout,
    /// Sizes laid end to end: from the table's end, this many bytes on, aligned up to this.
    pub base: (u64, u64),
    pub codec: Option<Codec>,
}

#[derive(Clone, Debug)]
pub struct Template {
    pub magic: Vec<u8>,
    pub dirs: Vec<TDir>,
}

/// The extension of a name, lowercased (with its dot).
pub fn ext(name: &str) -> String {
    match name.rfind('.') {
        Some(i) => name[i..].to_lowercase(),
        None => String::new(),
    }
}

fn role(files: &[File], key: usize, i: usize) -> Role {
    if i == key { Role::Key } else { Role::Other(ext(&files[i].name)) }
}

fn resolve(files: &[File], key: usize, r: &Role) -> Option<usize> {
    match r {
        Role::Key => Some(key),
        Role::Other(e) => (0..files.len()).find(|&i| i != key && ext(&files[i].name) == *e),
    }
}

/// The fields of a layout moved by `shift` bytes (to the record's start).
fn moved(layout: &Layout, shift: i64) -> Layout {
    let mv = |f: Field| Field { pos: f.pos - shift, ..f };
    match layout {
        Layout::Explicit { off, size, alt, fit, ctrl } => {
            Layout::Explicit { off: mv(*off), size: mv(*size), alt: alt.map(mv), fit: *fit, ctrl: *ctrl }
        }
        Layout::Implicit { size, align, base } => Layout::Implicit { size: mv(*size), align: *align, base: *base },
    }
}

fn fields_of(layout: &Layout) -> Vec<Field> {
    match layout {
        Layout::Explicit { off, size, alt, .. } => [Some(*off), Some(*size), *alt].into_iter().flatten().collect(),
        Layout::Implicit { size, .. } => vec![*size],
    }
}

/// The header field before `before` (in the first 256 bytes) whose value is `rows`, or a row
/// or two more (a table's edge may have lost them where it was found: the header's count is
/// then the true one).
fn count_field(b: &[u8], before: usize, rows: usize) -> Option<Count> {
    for extra in 0..3u64 {
        let mut best = None;
        for pos in 0..before.min(256) {
            for bytes in [2u8, 4, 8] {
                if pos + bytes as usize > before {
                    continue;
                }
                for be in [false, true] {
                    let c = Count { pos, bytes, be };
                    if read_count(b, &c) == rows as u64 + extra {
                        best = Some(c); // the last before the table, the widest there
                    }
                }
            }
        }
        if best.is_some() {
            return best;
        }
    }
    None
}

fn read_count(b: &[u8], c: &Count) -> u64 {
    let v = word(b, c.pos as i64, c.be);
    if c.be { v >> (64 - 8 * c.bytes as u32) } else { v & crate::fields::mask(8 * c.bytes) }
}

/// The template of the directories found in `files` (`key` the file with the magic).
pub fn learn(files: &[File], key: usize, dirs: &[Directory]) -> Option<Template> {
    if dirs.is_empty() {
        return None;
    }
    let mut out: Vec<TDir> = Vec::new();
    let mut ends: Vec<(usize, usize)> = Vec::new(); // (owner, end of table) of each directory
    for d in dirs {
        let t = &d.cand.table;
        let k = t.k as i64;
        // the record's start: the row moved so that its first field is in it
        let lo = fields_of(&d.cand.layout).iter().map(|f| f.pos).min().unwrap_or(0);
        let shift = lo.div_euclid(k) * k;
        let start = (t.start as i64 + shift) as usize;
        let rows = t.rows();
        let end = start + rows * t.k;
        ends.push((d.owner_file, end));
        out.push(TDir {
            owner: role(files, key, d.owner_file),
            target: role(files, key, d.target_file),
            decoded: d.parent,
            anchor: Anchor::At(start),
            count: None,
            rows,
            k: t.k,
            layout: moved(&d.cand.layout, shift),
            base: (0, 1),
            codec: d.codec,
        });
        let o = out.last_mut().unwrap();
        o.count = count_field(files[d.owner_file].data, start, rows);
        if let Layout::Implicit { base, .. } = d.cand.layout {
            let gap = base.saturating_sub(end as u64);
            let al = [4096u64, 2048, 512, 16].into_iter().find(|&a| gap > 0 && base == (end as u64).div_ceil(a) * a);
            o.base = match al {
                Some(a) => (0, a),
                None => (gap, 1),
            };
        }
    }
    // tables right after another's (within 16 bytes): anchored to it
    for i in 0..out.len() {
        let Anchor::At(start) = out[i].anchor else { continue };
        let owner = dirs[i].owner_file;
        if let Some(j) =
            (0..out.len()).find(|&j| j != i && ends[j].0 == owner && ends[j].1 <= start && start - ends[j].1 < 16)
        {
            out[i].anchor = Anchor::After { dir: j, gap: start - ends[j].1 };
        }
    }
    Some(Template { magic: files[key].data[..files[key].data.len().min(8)].to_vec(), dirs: out })
}

/// The directories of `template` in `files` (`key` the file with the magic), all of them, or
/// None where one does not hold.
pub fn verify(t: &Template, files: &[File], key: usize) -> Option<Vec<Directory>> {
    let why = |s: String| {
        if std::env::var_os("AQR_PAK_VERBOSE").is_some() {
            eprintln!("    template: {s}");
        }
    };
    if files[key].data.len() < t.magic.len() || files[key].data[..t.magic.len()] != t.magic[..] {
        return None;
    }
    let n = t.dirs.len();
    let mut tables: Vec<Option<Table>> = vec![None; n];
    // the tables' places first, where their counts tell their ends (one table's start may be
    // another's end, whose target the first one's members make: NMS's entries and chunks)
    let mut starts: Vec<Option<usize>> = vec![None; n];
    let mut counted: Vec<Option<usize>> = vec![None; n];
    for _ in 0..n {
        for i in 0..n {
            let d = &t.dirs[i];
            let owner = resolve(files, key, &d.owner)?;
            let b = files[owner].data;
            let start = match d.anchor {
                Anchor::At(s) => Some(s),
                Anchor::After { dir, gap } => starts[dir]
                    .zip(counted[dir])
                    .and_then(|(s, r)| r.checked_mul(t.dirs[dir].k)?.checked_add(s)?.checked_add(gap)),
            };
            starts[i] = start;
            counted[i] = d.count.map(|c| read_count(b, &c) as usize);
        }
    }
    let mut found: Vec<Option<Directory>> = (0..n).map(|_| None).collect();
    let mut decoded: Vec<Option<Target>> = (0..n).map(|_| None).collect();
    // in passes: a table's start may wait for another's end (uncounted), a target for
    // another's members
    for _ in 0..n + 1 {
        let mut progress = false;
        for i in 0..n {
            if found[i].is_some() {
                continue;
            }
            let d = &t.dirs[i];
            let owner = resolve(files, key, &d.owner)?;
            let start = match (starts[i], d.anchor) {
                (Some(s), _) => s,
                (None, Anchor::At(s)) => s,
                (None, Anchor::After { dir, gap }) => match tables[dir] {
                    Some(u) => u.end.checked_add(gap)?,
                    None => continue,
                },
            };
            let b = files[owner].data;
            if start > b.len() {
                return None;
            }
            let rows = counted[i];
            // (the record's start learned is its first field's: the last row may run past the
            // file's end by less than a record, its fields in it)
            let last_field = fields_of(&d.layout).iter().map(|f| f.end()).max().unwrap_or(d.k as i64);
            let past = |r: usize| {
                let last =
                    (r - 1).checked_mul(d.k).and_then(|x| x.checked_add(start)).and_then(|x| i64::try_from(x).ok());
                last.and_then(|x| x.checked_add(last_field)).is_none_or(|e| e > b.len() as i64)
            };
            if rows.is_some_and(|r| r > 0 && past(r)) {
                why(format!("dir {i}: {rows:?} rows from {start:#x} past the end"));
                return None;
            }
            // the target
            let file_target;
            let target: &Target = match d.decoded {
                Some(p) => match &decoded[p] {
                    Some(v) => v,
                    None => continue,
                },
                None => {
                    let f = resolve(files, key, &d.target)?;
                    file_target = Target::file(&files[f].name, f, files[f].data);
                    &file_target
                }
            };
            let l = target.len;
            let same = target.is_file() && target.file == owner;
            let (table, members) = match &d.layout {
                Layout::Explicit { off, size, alt, .. } => {
                    let rows = rows.unwrap_or_else(|| guess_rows(b, start, d, l));
                    let table = Table { start, end: start + rows * d.k, k: d.k, score: 0.0, primary: true };
                    let Some(m) = explicit(b, &table, off, size, alt.as_ref(), l) else {
                        why(format!("dir {i}: offsets and sizes of {rows} rows from {start:#x} do not tile {l}"));
                        return None;
                    };
                    (table, m)
                }
                Layout::Implicit { size, align, .. } => {
                    let Some((rows, base)) = implicit(b, start, d, size, *align, l, same, rows) else {
                        why(format!("dir {i}: sizes of {rows:?} rows from {start:#x} do not end at {l}"));
                        return None;
                    };
                    let table = Table { start, end: start + rows * d.k, k: d.k, score: 0.0, primary: true };
                    let c = Cand { layout: Layout::Implicit { size: *size, align: *align, base }, table, ev: 0.0 };
                    (table, tiling::members(b, &c, l))
                }
            };
            // its members' codec, on a sample
            let idx = tiling::even(members.len(), 8);
            let mut codecs = Vec::new();
            let mut contents = Vec::new();
            for &j in &idx {
                let m = members[j as usize];
                // the template's codec alone (a probe of every codec on stored textures of
                // megabytes took seconds: an LZ4 block decodes most bytes a long way)
                let blob = target.read(m.offset, m.size.min(if d.codec.is_some() { u64::MAX } else { 64 }));
                let decoded = d.codec.and_then(|c| codec::decode(c, &blob).map(|x| (c, x)));
                match decoded {
                    Some((c, x)) => {
                        codecs.push(Some(c));
                        contents.push(kind_of(&x));
                    }
                    None => {
                        codecs.push(None);
                        contents.push(kind_of(&blob));
                    }
                }
            }
            // the codec learned among the members, and no other (a pak may store some chunks
            // as they are: those zstd does not make smaller)
            let majority = tally(&codecs).into_iter().find_map(|(c, _)| c);
            // (all of them stored too: sounds zstd or LZO would not make smaller)
            let other = codecs.iter().flatten().any(|c| Some(*c) != d.codec);
            if !members.is_empty() && other {
                why(format!("dir {i}: members of {:?}, not {:?}", tally(&codecs), d.codec));
                return None;
            }
            let cover = filled(&members) as f64 / l.max(1) as f64;
            let cand = Cand {
                layout: match &d.layout {
                    Layout::Implicit { size, align, .. } => {
                        let base = members.first().map_or(0, |m| m.offset);
                        Layout::Implicit { size: *size, align: *align, base }
                    }
                    other => other.clone(),
                },
                table,
                ev: 0.0,
            };
            let (tname, tfile, tis_file) = (target.name.clone(), target.file, target.is_file());
            let new_target = (t.dirs.iter().any(|x| x.decoded == Some(i)) && tis_file).then(|| {
                decode_in_place(format!("{tname}~decoded"), tfile, files[tfile].data, &members, b, &table, d.codec)
                    .with_parent(i)
            });
            let decoded_len = new_target.as_ref().map(|v| v.len);
            decoded[i] = new_target;
            found[i] = Some(Directory {
                target: tname,
                target_len: l,
                target_file: tfile,
                parent: d.decoded,
                owner: files[owner].name.clone(),
                owner_file: owner,
                codec: majority,
                cand,
                members,
                ev: 0.0,
                cover,
                codecs: tally(&codecs.iter().map(|c| c.map_or("stored", |c| c.name())).collect::<Vec<_>>()),
                contents: tally(&contents),
                decoded: decoded_len,
            });
            tables[i] = Some(table);
            progress = true;
        }
        if !progress {
            break;
        }
    }
    if let Some(i) = found.iter().position(|f| f.is_none()) {
        why(format!("dir {i}: waits for another"));
        return None;
    }
    found.into_iter().collect()
}

/// The rows of a table without a count: up to the first row whose offset is out of the target
/// or whose size is (none past ten times the rows learned).
fn guess_rows(b: &[u8], start: usize, d: &TDir, l: u64) -> usize {
    let Layout::Explicit { off, size, .. } = &d.layout else { return 0 };
    let t = Table { start, end: b.len(), k: d.k, score: 0.0, primary: true };
    let max = ((b.len() - start) / d.k).min(10 * d.rows.max(16));
    (0..max).find(|&r| off.get(b, &t, r as i64) >= l || size.get(b, &t, r as i64) > l).unwrap_or(max)
}

/// Offsets and sizes: nine members in ten at least fitting their rooms (the last within its
/// target), and filling their span.
fn explicit(b: &[u8], t: &Table, off: &Field, size: &Field, alt: Option<&Field>, l: u64) -> Option<Vec<Member>> {
    let rows: Vec<i64> = (0..t.rows() as i64).collect();
    if rows.is_empty() {
        return None;
    }
    let o = off.values(b, t, &rows);
    let s = size.values(b, t, &rows);
    let s2 = alt.map(|a| a.values(b, t, &rows));
    let rm = tiling::rooms(&o, l);
    let mut members = Vec::new();
    for i in 0..rows.len() {
        if o[i] >= l {
            continue;
        }
        let room = rm[i];
        let ok = |x: u64| match room {
            Some(r) => tiling::fits(r, x),
            None => x <= l - o[i], // the last member: its room runs to the end
        };
        let pick = if ok(s[i]) { Some(s[i]) } else { s2.as_ref().map(|x| x[i]).filter(|&x| ok(x)) };
        if let Some(sz) = pick {
            members.push(Member { offset: o[i], size: sz, row: i as i64 });
        }
    }
    // filling the span of the members (not the whole target: a template has told the format,
    // and a decoded target may end in much padding, the 64 KiB of a pak's only chunk holding
    // 7 KB of members)
    let first = members.iter().map(|m| m.offset).min()?;
    let last = members.iter().map(|m| m.offset + m.size).max()?;
    let fill = filled(&members);
    (members.len() * 10 >= rows.len() * 9 && fill as f64 >= 0.9 * (last - first) as f64).then_some(members)
}

/// Sizes laid end to end from the table's end (by the template's base): the rows (the count's,
/// else those after which they end at the target's end) and the base.
#[allow(clippy::too_many_arguments)]
fn implicit(
    b: &[u8],
    start: usize,
    d: &TDir,
    size: &Field,
    align: u64,
    l: u64,
    same: bool,
    rows: Option<usize>,
) -> Option<(usize, u64)> {
    let t = Table { start, end: b.len(), k: d.k, score: 0.0, primary: true };
    let base_of = |r: usize| -> u64 {
        if !same {
            return 0;
        }
        let end = (start + r * d.k) as u64;
        end.div_ceil(d.base.1) * d.base.1 + d.base.0
    };
    let max = rows.unwrap_or(((b.len() - start) / d.k).min(10 * d.rows.max(16)));
    let mut sum = 0u64;
    for r in 1..=max {
        let v = size.get(b, &t, r as i64 - 1);
        if v == 0 || v > l {
            return None;
        }
        sum += v.div_ceil(align) * align;
        let base = base_of(r);
        if rows.is_some_and(|n| r < n) {
            continue;
        }
        if base + sum <= l && l - (base + sum) < align.max(16) {
            return Some((r, base));
        }
        if base + sum > l {
            return None;
        }
    }
    None
}
