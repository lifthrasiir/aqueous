//! aqr-rar's formulas (see aqr-formula): a RAR archive (RAR5, or RAR 1.5–4.x) whose files'
//! packed data is rebuilt from their contents. Stored files are their contents; the compressed
//! streams (a solid group's files together, or a file alone) are the outputs of one application
//! of the module `rar5-recons` or `rar3-recons` (`FORMAT.md`), split into the files' packed
//! data. The rest (the headers, what is not taken) is kept as it is. The contents are streams by
//! their kinds by their names, so that whoever takes the formula may recompress them in turn and
//! store them once where the same files are archived along with it.

use std::ops::Range;

use aqr_formula::{Apply, Formula, Kind, Module, Node};

use crate::{rar4, rar5, recons3, recons5};

pub use crate::module::{decode3, decode5, index, params3};

pub const MODULE5: Module = Module::new("rar5-recons", 2).with_plan(plan5);
pub const MODULE3: Module = Module::new("rar3-recons", 2).with_plan(plan3);

/// The memory of `rar5-recons`: its plan ([`crate::plan::decode5`]).
fn plan5(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let files = u.all_contents::<3>()?;
    crate::plan::decode5(u.initial(), u.params, u.inputs, u.output(), files)
}

/// The memory of `rar3-recons`: its plan ([`crate::plan::decode3`]).
fn plan3(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let files = u.all_contents::<3>()?;
    crate::plan::decode3(u.initial(), u.params, u.inputs, u.output(), files)
}

/// The application of `module` (of `params`: none for [`MODULE5`], [`params3`] for [`MODULE3`])
/// rebuilding streams of `len` bytes in all from their data `data`, their records `records` and
/// the index of their lengths `index` ([`index`]).
pub fn apply_recons(module: Module, params: Vec<u8>, data: Node, records: Node, index: Node, len: u64) -> Apply {
    Apply {
        module,
        resources: vec![],
        params,
        inputs: vec![data, records, index],
        len,
        memory_limit: aqr_formula::NO_LIMIT,
        task_limits: Vec::new(),
    }
}

/// Contents of at least this many bytes are slices of their own, apart from the others of their
/// kind next to them: so that the same contents elsewhere are found by their slices.
pub const OWN_MIN: usize = 1 << 10;

#[derive(Debug, Clone)]
pub struct Options {
    pub threads: usize,
    /// The most bytes of data that the streams may decode to, all together, by the sizes their
    /// headers tell: the streams past it are kept as they are.
    pub limit: u64,
    /// What a file's contents are by its name: the contents of each [`Kind`] are a stream of their
    /// own, sliced in order. By default all are unknown.
    pub kind: fn(&[u8]) -> Kind,
    /// Whether streams with PPMd blocks are taken (else kept as they are, turned down at their
    /// first PPMd block): rebuilding PPMd is slow, and its data seldom compresses better apart.
    pub ppmd: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options { threads: 1, limit: u64::MAX, kind: |_| Kind::Unknown, ppmd: true }
    }
}

/// [`Options::limit`] for an archive of `n` bytes on its own: 64 MiB, and 32 times the archive.
pub fn limit(n: usize) -> u64 {
    (64 << 20) + 32 * n as u64
}

/// What [`encode_with`] made.
pub struct Encoded {
    pub formula: Formula,
    /// Bytes of data decoded from streams (what counts against [`Options::limit`]).
    pub inflated: u64,
    /// Whether each file's data is rebuilt (else kept as it is), by entry.
    pub rebuilt: Vec<bool>,
}

/// A file of the archive as the formula has it.
struct File {
    name: Range<usize>,
    data: Range<usize>,
    size: u64,
    stored: bool,
}

/// A stream: its files (indexes), whether it can be taken, the parameters of its decoder.
struct Group {
    files: Vec<usize>,
    ok: bool,
    v1: bool,
    dict: u64,
}

/// The archive read: RAR5's or not, its files, its streams.
fn read(d: &[u8]) -> Option<(bool, Vec<File>, Vec<Group>)> {
    let mut files = Vec::new();
    let mut groups: Vec<Group> = Vec::new();
    let mut open = false;
    if let Some(a) = rar5::parse(d) {
        for e in a.entries.iter().filter(|e| !e.service && !e.dir && !e.data.is_empty()) {
            let fine = !e.split && !e.encrypted && e.size.is_some() && e.version <= 1;
            let stored = e.method == 0 && fine && e.size == Some(e.data.len() as u64);
            let i = files.len();
            files.push(File { name: e.name.clone(), data: e.data.clone(), size: e.size.unwrap_or(0), stored });
            if e.method == 0 {
                open = false;
                continue;
            }
            match groups.last_mut() {
                Some(g) if e.solid && open => {
                    g.files.push(i);
                    g.ok &= fine && e.version == g.v1 as u8 && e.dict == g.dict;
                }
                _ => groups.push(Group { files: vec![i], ok: fine && !e.solid, v1: e.version == 1, dict: e.dict }),
            }
            open = true;
        }
        return Some((true, files, groups));
    }
    let a = rar4::parse(d)?;
    for e in a.entries.iter().filter(|e| !e.service && !e.dir && !e.data.is_empty()) {
        let fine = !e.split && !e.encrypted;
        let stored = e.method == 0x30 && fine && e.size == e.data.len() as u64;
        let i = files.len();
        files.push(File { name: e.name.clone(), data: e.data.clone(), size: e.size, stored });
        if e.method == 0x30 {
            open = false;
            continue;
        }
        let fine = fine && e.version == 29 && (0x31..=0x35).contains(&e.method);
        match groups.last_mut() {
            Some(g) if e.solid && open => {
                g.files.push(i);
                g.ok &= fine && e.dict == g.dict;
            }
            _ => groups.push(Group { files: vec![i], ok: fine && !e.solid, v1: false, dict: e.dict }),
        }
        open = true;
    }
    Some((false, files, groups))
}

/// Whether `d` starts with a RAR archive.
pub fn starts(d: &[u8]) -> bool {
    rar5::starts(d) || rar4::starts(d)
}

/// Bytes of the RAR archive at the start of `d`, to its end.
pub fn extent(d: &[u8]) -> Option<usize> {
    match rar5::parse(d) {
        Some(a) => Some(a.end),
        None => rar4::parse(d).map(|a| a.end),
    }
}

/// The formula rebuilding the archive `input`, decoding at most [`limit`] bytes. None where it is
/// not one, or no file is rebuilt.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    let opt = Options { threads, limit: limit(input.len()), ..Options::default() };
    encode_with(input, &opt).map(|e| e.formula)
}

/// An entry's data made ready for the formula.
enum Made {
    Stored,
    /// Rebuilt by the application.
    Split,
}

/// [`encode`] with `opt`.
pub fn encode_with(input: &[u8], opt: &Options) -> Option<Encoded> {
    let (v5, files, groups) = read(input)?;
    let mut made: Vec<Option<Made>> = files.iter().map(|f| f.stored.then_some(Made::Stored)).collect();
    // the streams that may be split, those decoding within the limit by their headers' sizes
    let mut left = opt.limit;
    let mut take = Vec::new();
    for (gi, g) in groups.iter().enumerate() {
        let size: u64 = g.files.iter().map(|&i| files[i].size).sum();
        if g.ok && size <= left && usize::try_from(size).is_ok() {
            left -= size;
            take.push(gi);
        }
    }
    // (the large ones first, side by side on one thread each)
    take.sort_by_key(|&gi| std::cmp::Reverse(groups[gi].files.iter().map(|&i| files[i].data.len()).sum::<usize>()));
    let done = aqr_common::pool::par_map(take.len(), opt.threads, |k| {
        let g = &groups[take[k]];
        let parts: Vec<&[u8]> = g.files.iter().map(|&i| &input[files[i].data.clone()]).collect();
        let sizes: Vec<usize> = g.files.iter().map(|&i| files[i].size as usize).collect();
        let packed: usize = parts.iter().map(|p| p.len()).sum();
        let sp = if v5 {
            recons5::split(&parts, &sizes, g.v1, g.dict)
        } else {
            recons3::split_with(&parts, &sizes, g.dict, opt.ppmd)
        };
        // (a stream kept costs nothing more than itself, its headers kept anyway)
        sp.ok().filter(|sp| sp.record.len() < packed)
    });
    let mut splits: Vec<(usize, recons5::Split)> =
        take.into_iter().zip(done).filter_map(|(gi, s)| Some((gi, s?))).collect();
    // (in the archive's order)
    splits.sort_by_key(|s| s.0);
    for (gi, _) in &splits {
        for &i in &groups[*gi].files {
            made[i] = Some(Made::Split);
        }
    }
    if made.iter().all(Option::is_none) {
        return None;
    }

    let mut f = Formula::default();
    f.push("rar", Vec::new());
    // the contents of stored files and of streams with their kinds and ends; the records and
    // their lengths
    let mut kept = (Vec::new(), Vec::new());
    let mut split = (Vec::new(), Vec::new());
    let (mut records, mut lens) = (Vec::new(), Vec::new());
    let mut inflated = 0;
    let mut kept_sizes = Vec::new();
    for (i, file) in files.iter().enumerate() {
        if let Some(Made::Stored) = made[i] {
            kept.0.extend_from_slice(&input[file.data.clone()]);
            kept.1.push(((opt.kind)(&input[file.name.clone()]), kept.0.len()));
            kept_sizes.push(file.data.len() as u64);
        }
    }
    let mut split_sizes = Vec::new();
    for (gi, sp) in &splits {
        let mut at = 0;
        for &i in &groups[*gi].files {
            let n = files[i].size as usize;
            split.0.extend_from_slice(&sp.data[at..at + n]);
            split.1.push(((opt.kind)(&input[files[i].name.clone()]), split.0.len()));
            at += n;
            split_sizes.push(files[i].data.len() as u64);
        }
        inflated += sp.data.len() as u64;
        lens.push((sp.data.len(), sp.record.len()));
        records.extend_from_slice(&sp.record);
    }
    let by_kind = |f: &mut Formula, (data, ends): (Vec<u8>, Vec<(Kind, usize)>)| {
        let raw = f.streams.len();
        f.push("raw", Vec::new());
        f.push_by_kind(raw, data, &ends, "rar", OWN_MIN)
    };
    let kept_nodes = match kept_sizes.is_empty() {
        true => Vec::new(),
        false => by_kind(&mut f, kept).cut(&kept_sizes),
    };
    let split_nodes = match split_sizes.is_empty() {
        true => Vec::new(),
        false => {
            let records = f.push_as("rar/record", Kind::Compressed, records);
            let index = match index(&lens) {
                i if i.is_empty() => Node::Concat(Vec::new()),
                i => f.push("rar/index", i),
            };
            let data = by_kind(&mut f, split);
            let (module, params) = match v5 {
                true => (MODULE5, Vec::new()),
                false => (MODULE3, params3(splits.iter().map(|s| s.1.ppmd).max().unwrap_or(0))),
            };
            let a = apply_recons(module, params, data, records, index, split_sizes.iter().sum());
            match split_sizes.len() {
                1 => vec![Node::Apply(Box::new(a))],
                _ => f.split(a, split_sizes),
            }
        }
    };
    // the files rebuilt in the archive's order: stored ones in turn, split ones by stream
    let mut kept_nodes = kept_nodes.into_iter();
    let mut by_file: Vec<Option<Node>> = files.iter().map(|_| None).collect();
    for (i, m) in made.iter().enumerate() {
        if let Some(Made::Stored) = m {
            by_file[i] = kept_nodes.next();
        }
    }
    let mut split_nodes = split_nodes.into_iter();
    for (gi, _) in &splits {
        for &i in &groups[*gi].files {
            by_file[i] = split_nodes.next();
        }
    }
    // the root: the bytes kept and the data rebuilt in turn
    let (mut container, mut root, mut at) = (Vec::new(), Vec::new(), 0);
    for (file, node) in files.iter().zip(by_file) {
        let Some(node) = node else { continue };
        root.push(Node::slice(0, container.len() as u64, (file.data.start - at) as u64));
        container.extend_from_slice(&input[at..file.data.start]);
        root.push(node);
        at = file.data.end;
    }
    root.push(Node::slice(0, container.len() as u64, (input.len() - at) as u64));
    container.extend_from_slice(&input[at..]);
    f.streams[0].data = container.into();
    f.root = Node::concat(root);
    let rebuilt = made.iter().map(Option::is_some).collect();
    Some(Encoded { formula: f, inflated, rebuilt })
}

/// The output of `a`, an application of [`MODULE5`] or [`MODULE3`], on `inputs`.
pub fn apply(a: &Apply, inputs: &[&[u8]], threads: usize) -> Result<Vec<u8>, String> {
    match a.module {
        m if m == MODULE5 && a.resources.is_empty() => {
            decode5(&a.params, inputs, a.len, threads).map_err(|e| e.to_string())
        }
        m if m == MODULE3 && a.resources.is_empty() => {
            decode3(&a.params, inputs, a.len, threads).map_err(|e| e.to_string())
        }
        m => Err(format!("module {m:?} not of aqr-rar")),
    }
}
