//! Executables: aqr-exe's filters on each executable section, the x86 disfilter or the ARM64
//! filter, the rest of the file sliced in between (§4.2, example 1). PE images, and Mach-O
//! images, thin or in a universal file (whose images of other architectures are kept with
//! the rest). Of a .NET image, the section holding the metadata goes through the CIL filter,
//! the native code of a ReadyToRun image within what that keeps as it is. The rest of the file,
//! and what the CIL filter keeps as it is, are contents (`Stream::content`): scanned in turn,
//! e.g. for the images among the resources. Of a PE image, its tables of known layouts outside
//! the code are taken too: the base relocations by `exe-recons/pe-reloc`, the tables of exports,
//! imports and runtime functions by `recs-fields`, their numbers as deltas; of an NSIS
//! installer, the files its data stores, each a content of its format ([`crate::nsis`]). Of a 64-bit Mach-O
//! image, the symbol table and its string table by `exe-recons/macho-symtab`; of its `__DWARF`
//! segment, its DWARF by aqr-dwarf's modules ([`crate::dwarf`]). ELF images, of their code and
//! their DWARF.
//!
//! The ARM64 and x86 images of the Mach-O and ELF files of an archive alike (other versions of
//! one program) are planned beforehand ([`plan`]) as a forest, each image's targets (of BL and
//! ADRP; of calls, jumps and RIP-relative addresses) remapped to its parent's address space, as
//! its parent's are to its own, by `exe-recons/arm64-remap` and `exe-recons/x86-remap`; so that
//! their code streams match where the code moved, for the pieces of streams of a class put
//! together (aqr-formula's `merge_plan`). The code stream of an ARM64 image's largest section is
//! coded against that of its tree's root besides (`exe-recons/arm64-delta`), in pieces (of a
//! fourth of a solid block, [`PIECE_SHARE`]) each against [`MOST`] of the root's, which keeps its
//! stream in pieces of a block's bound apart (a block each), so that a piece decodes with no more
//! than a block's bound of them: the
//! image's formula holds a copy of the root's stream ([`REFERENCE`]), which [`resolve`] makes
//! the root's own once the formulas are put together.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, OnceLock};

use aqr_decompress::{ARM64, ARM64_DELTA, ARM64_REMAP, CIL, MACHO_SYMTAB, PE_RELOC, X86, X86_REMAP};
use aqr_exe::arch::{arm64, cil, x86};
use aqr_exe::format::pe::{TableKind, locate_code, tables};
use aqr_exe::format::{elf, macho};
use aqr_exe::{reloc, symtab};
use aqr_formula::{Apply, Bytes, Formula, Kind, Node, Resource};
use aqr_infer_recs::formula::{Field, How};

/// What a range of a file holds: code, or a table of a PE image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arch {
    /// x86-64 if true, else x86-32, of image `k` of the file (as `Arm64`).
    X86(bool, usize),
    /// Of image `k` of the file (whose remap, if any, is the `k`-th).
    Arm64(usize),
    /// .NET metadata and IL, the metadata root at this offset (the virtual address is the
    /// section's RVA).
    Cil(usize),
    Table(TableKind),
    /// The symbol table of a Mach-O image, of its string table at `stroff` of `strsize` bytes,
    /// its external symbols from `ext` on.
    Symbols {
        ext: u32,
        stroff: usize,
        strsize: usize,
    },
    /// Output `k` of the outputs [`symbols`] made.
    Output(usize),
    /// A file of this format, stored in an NSIS installer's data ([`crate::nsis`]): a content
    /// of its own.
    Stored(&'static str),
}

/// Tables shorter than this are left with the rest.
const MIN_TABLE: usize = 64;

/// Symbol tables shorter than this, with their string tables, are left with the rest: the
/// module would cost more.
const MIN_SYMBOLS: usize = 16 << 10;

pub(crate) fn pe(d: &[u8]) -> Option<Formula> {
    let code = locate_code(&mut Cursor::new(d)).ok()?;
    let arch = match code.machine {
        0x14c | 0x8664 => Some(Arch::X86(code.is64, 0)),
        0xaa64 => Some(Arch::Arm64(0)),
        _ => None,
    };
    let within = |off: u64, size: usize| {
        let off = usize::try_from(off).ok().filter(|&o| o < d.len())?;
        Some((off, size.min(d.len() - off)))
    };
    let mut sections: Vec<_> = code
        .sections
        .iter()
        .filter_map(|&(va, off, size)| Some((va, within(off, size)?, arch?)))
        .map(|(va, (off, size), arch)| (va, off, size, arch))
        .collect();
    if let Some(cli) = code.cli {
        let (rva, off, size) = cli.section;
        if let Some((off, size)) = within(off, size).filter(|&(_, size)| cli.metadata < size) {
            sections.push((rva as u64, off, size, Arch::Cil(cli.metadata)));
        }
    }
    // the tables outside the code
    let code: Vec<_> = sections.iter().map(|s| s.1..s.1 + s.2).collect();
    sections.extend(
        tables(d)
            .into_iter()
            .filter(|t| t.len >= MIN_TABLE && !code.iter().any(|c| t.off < c.end && c.start < t.off + t.len))
            .map(|t| (0, t.off, t.len, Arch::Table(t.kind))),
    );
    // the files stored in an installer's data
    for b in crate::nsis::in_pe(d).unwrap_or_default().into_iter().filter(|b| b.len() >= crate::nsis::MIN_BLOCK) {
        if let Some(format) = crate::nsis::format(&d[b.clone()]) {
            sections.push((0, b.start, b.len(), Arch::Stored(format.name())));
        }
    }
    filtered(d, "pe", sections, &[], &[], &[])
}

/// An image's code: `(virtual address, offset, size)` of each section, within its file.
type Code = Vec<(u64, usize, usize)>;

/// The images of code of the Mach-O or ELF file `d`, x86 or ARM64: of each, its architecture
/// (its index in the file within) and its code.
fn code_images(d: &[u8]) -> Vec<(Arch, Code)> {
    let arch = |x86: bool, arm64: bool, is64: bool, k: usize| match () {
        _ if x86 => Some(Arch::X86(is64, k)),
        _ if arm64 => Some(Arch::Arm64(k)),
        _ => None,
    };
    if let Some(images) = macho::images(d) {
        let arch = |(k, i): (usize, macho::Image)| {
            Some((arch(i.is_x86(), i.cputype == macho::CPU_TYPE_ARM64, i.is64(), k)?, i.sections))
        };
        images.into_iter().enumerate().filter_map(arch).collect()
    } else if let Some(i) = elf::image(d) {
        arch(i.is_x86(), i.machine == elf::EM_AARCH64, i.is64(), 0).map(|a| (a, i.code)).into_iter().collect()
    } else {
        Vec::new()
    }
}

/// Whether `d` is a Mach-O or ELF file, whose images [`plan`] may remap.
pub(crate) fn remappable(d: &[u8]) -> bool {
    macho::starts(d) || elf::starts(d)
}

fn with_arch((a, code): (Arch, Code)) -> impl Iterator<Item = (u64, usize, usize, Arch)> {
    code.into_iter().map(move |(va, off, size)| (va, off, size, a))
}

/// The formula of ELF file `d`, its code remapped as `remaps` tells. (Its tables are left
/// in the rest: `.eh_frame_hdr`'s and the relocations as `recs-fields` takes them do not
/// archive smaller.)
pub(crate) fn elf(d: &[u8], remaps: &[Option<Arc<Remap>>], deltas: &[Option<Delta>]) -> Option<Formula> {
    let sections = code_images(d).into_iter().flat_map(with_arch);
    // (compressed sections are left to the scan of the rest)
    let debug: Vec<_> = elf::image(d)
        .map(|i| i.sections)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.flags & elf::SHF_COMPRESSED == 0)
        .filter_map(|s| {
            let section = aqr_dwarf::Section::by_name(&s.name)?;
            Some(crate::dwarf::Debug { section, image: 0, off: s.off, size: s.size })
        })
        .collect();
    filtered(d, "elf", sections.collect(), remaps, deltas, &debug)
}

/// The formula of Mach-O file `d`, its images remapped as `remaps` tells (by image).
pub(crate) fn macho(d: &[u8], remaps: &[Option<Arc<Remap>>], deltas: &[Option<Delta>]) -> Option<Formula> {
    let images = macho::images(d)?;
    let sections = code_images(d).into_iter().flat_map(with_arch);
    let symbols = images.iter().filter_map(|i| i.symtab).map(|s| {
        let arch = Arch::Symbols { ext: s.ext, stroff: s.stroff, strsize: s.strsize };
        (0, s.symoff, s.nsyms as usize * 16, arch)
    });
    let symbols = symbols.filter(|s| matches!(s.3, Arch::Symbols { strsize, .. } if s.2 + strsize >= MIN_SYMBOLS));
    let debug: Vec<_> = images
        .iter()
        .enumerate()
        .flat_map(|(image, i)| {
            i.debug.iter().filter_map(move |(name, off, size)| {
                let section = aqr_dwarf::Section::by_name(name)?;
                Some(crate::dwarf::Debug { section, image, off: *off, size: *size })
            })
        })
        .collect();
    filtered(d, "macho", sections.chain(symbols).collect(), remaps, deltas, &debug)
}

/// Images less alike than this (`Signature::alike`) have no parent: builds of aqr a day
/// apart are 0.14 to 0.16 alike (their code remapped 33 to 36% smaller against each other),
/// Chrome's 156.0.8078.49 and .76 0.36, Electron 42 and 43 0.21 (20%), Chromium and Electron
/// 0.05 (0.7%), programs not related 0.003.
const MIN_ALIKE: f64 = 0.1;

/// The remap of an image of code of either architecture.
pub(crate) enum Remap {
    Arm64(arm64::remap::Remap),
    X86(x86::remap::Remap),
}

impl Remap {
    fn compose(self, parent: &Remap) -> Remap {
        match (self, parent) {
            (Remap::Arm64(r), Remap::Arm64(p)) => Remap::Arm64(r.compose(p)),
            (Remap::X86(r), Remap::X86(p)) => Remap::X86(r.compose(p)),
            (r, _) => r,
        }
    }

    fn is_identity(&self) -> bool {
        match self {
            Remap::Arm64(r) => r.is_identity(),
            Remap::X86(r) => r.is_identity(),
        }
    }
}

/// The remaps of the images of Mach-O and ELF files, by where each file's data is: of each
/// image of the file, its remap where it has one; and of the ARM64 images, what their code is
/// to the others' of their tree ([`Delta`]).
#[derive(Default)]
pub(crate) struct Remaps(HashMap<usize, Vec<Option<Arc<Remap>>>>, HashMap<usize, Vec<Option<Delta>>>);

impl Remaps {
    /// The remaps of the images of the file whose data is `d`.
    pub fn of(&self, d: &[u8]) -> &[Option<Arc<Remap>>] {
        self.0.get(&(d.as_ptr() as usize)).map_or(&[], |r| r)
    }

    /// The deltas of the images of the file whose data is `d`.
    pub fn deltas(&self, d: &[u8]) -> &[Option<Delta>] {
        self.1.get(&(d.as_ptr() as usize)).map_or(&[], |r| r)
    }
}

/// The pieces of an ARM64 code stream coded against its root's are of a fourth of a solid
/// block's bound (and of [`PIECE_MIN`] at least), and those of the root's stream (a stream each,
/// apart) of the bound: with [`MOST`] of the root's, a piece decodes from a block's bound.
///
/// With a bound of 16 MiB, of Electron 43 to 44 (arm64), its code stream against the root's
/// archives to 14.3 MB whole (`aqr-exe-bench-delta`, zstd -19), of pieces of 4 MiB each against
/// three of 4 MiB 15.0 MB, against two of 8 MiB 15.0 MB, against one of 16 MiB 16.1 MB, of 8 MiB
/// against one of 8 MiB 23.5 MB; but the root's stream is larger in pieces, at zstd -9 by 6.1%,
/// 2.9% and 1.9% in pieces of 4, 8 and 16 MiB (Chrome 156.0.8078.47's). As `aqr -c` archives
/// them, of the arm64 images of Chrome Framework .47 and .49, .49 and .76, and Electron Framework
/// 43 and 44: 90.06, 97.29, 97.35 MB against two of 8 MiB; 88.71, 96.04, 97.75 MB against one of
/// 16 MiB (the first, against three of 4 MiB, 91.56 MB).
const PIECE_SHARE: usize = 4;

/// The least pieces ([`PIECE_SHARE`]).
const PIECE_MIN: usize = 64 << 10;

/// The pieces of the root's stream that a piece is coded against at most.
const MOST: usize = 1;

/// One word in 2^this, by its content, looked up for matches to pick the root's pieces.
const SAMPLE: u32 = 3;

/// The class of the copy of the root's code stream in a formula of an image coded against it,
/// which [`resolve`] makes the root's own.
pub(crate) const REFERENCE: &str = "arm64/root";

/// The class of the code streams of ARM64 images, as `arm64::STREAM_NAMES` makes it.
const CODE: &str = "arm64/CODE";

/// What the code stream of an ARM64 image's largest section, at `va`, is to the others of its
/// tree: the root's, stored in pieces of `root_piece` bytes (`root` the stream), or coded
/// against the root's in pieces of `piece`.
#[derive(Clone)]
pub(crate) struct Delta {
    va: u64,
    root: Arc<Vec<u8>>,
    is_root: bool,
    root_piece: usize,
    piece: usize,
}

/// The remaps of the ARM64 and x86 images of the Mach-O and ELF files among `files`: of those of
/// more than `min` bytes of code (half a solid block's bound), a maximum spanning forest of how
/// alike they are (`Signature`; not at all, of different architectures), each image remapped to
/// its parent's, composed with its parent's own. (Streams over half the bound of a solid block
/// are cut into pieces put with those alike of other streams; shorter ones go one after another
/// as their files do, rarely with their parents': remapped, they cost more.) Of each ARM64 tree,
/// the deltas of its images ([`Delta`]).
pub(crate) fn plan(files: &[&[u8]], threads: usize, min: usize) -> Remaps {
    // each image: its file, its index in the file, its architecture (x86-64, x86-32 or ARM64)
    // and its code sections
    let mut images = Vec::new();
    for (i, d) in files.iter().enumerate() {
        for (arch, code) in code_images(d) {
            let k = match arch {
                Arch::X86(_, k) | Arch::Arm64(k) => k,
                _ => unreachable!("an image of code"),
            };
            let secs: Vec<(u64, &[u8])> = code.iter().map(|&(va, off, size)| (va, &d[off..off + size])).collect();
            if secs.iter().map(|s| s.1.len()).sum::<usize>() > min {
                images.push((i, k, arch, secs));
            }
        }
    }
    if images.len() < 2 {
        return Remaps::default();
    }
    // (x86 code parsed once, for its signature and its matches)
    let parsed = crate::sets::par_map(images.len(), threads, |i| match images[i].2 {
        Arch::X86(is64, _) => Some(x86::remap::Image::new(&images[i].3, is64)),
        _ => None,
    });
    let signatures = crate::sets::par_map(images.len(), threads, |i| match &parsed[i] {
        Some(image) => image.signature(),
        None => arm64::remap::signature(&images[i].3),
    });
    let n = images.len();
    let same = |i: usize, j: usize| match (images[i].2, images[j].2) {
        (Arch::X86(a, _), Arch::X86(b, _)) => a == b,
        (a, b) => std::mem::discriminant(&a) == std::mem::discriminant(&b),
    };
    let rows = crate::sets::par_map(n, threads, |i| {
        (0..n)
            .map(|j| match () {
                _ if i == j => 1.0,
                _ if !same(i, j) => 0.0,
                _ => signatures[i].alike(&signatures[j]),
            })
            .collect()
    });
    let (order, parents) = crate::sets::tree(&rows, MIN_ALIKE);
    let raw = crate::sets::par_map(n, threads, |i| {
        let p = parents[i]?;
        let (a, b) = (order[p], order[i]);
        Some(match (&parsed[a], &parsed[b]) {
            (Some(a), Some(b)) => Remap::X86(x86::remap::Remap::of(a, b)),
            _ => Remap::Arm64(arm64::remap::Remap::new(&images[a].3, &images[b].3)),
        })
    });
    drop(parsed);
    // (in order, parents first)
    let mut composed: Vec<Option<Arc<Remap>>> = Vec::with_capacity(n);
    for (i, r) in raw.into_iter().enumerate() {
        let r = r.map(|r| match parents[i].and_then(|p| composed[p].as_deref()) {
            Some(p) => r.compose(p),
            None => r,
        });
        composed.push(r.filter(|r| !r.is_identity()).map(Arc::new));
    }
    let mut out: HashMap<usize, Vec<Option<Arc<Remap>>>> = HashMap::new();
    for (i, r) in composed.into_iter().enumerate() {
        let Some(r) = r else { continue };
        let (file, k, ..) = &images[order[i]];
        let v = out.entry(files[*file].as_ptr() as usize).or_default();
        v.resize(v.len().max(k + 1), None);
        v[*k] = Some(r);
    }
    // the ARM64 trees: each image of a parent coded against its root, whose code stream (of its
    // largest section) is made here for them
    let root_of = |mut i: usize| {
        while let Some(p) = parents[i] {
            i = p;
        }
        i
    };
    let largest = |i: usize| images[order[i]].3.iter().copied().max_by_key(|s| s.1.len());
    let roots: Vec<usize> = (0..n)
        .filter(|&i| matches!(images[order[i]].2, Arch::Arm64(_)) && parents[i].is_some())
        .map(root_of)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let codes = crate::sets::par_map(roots.len(), threads, |r| {
        let (va, code) = largest(roots[r]).expect("an image of code");
        Arc::new(arm64::encode(code, va).streams()[0].clone())
    });
    let code_of: HashMap<usize, Arc<Vec<u8>>> = roots.into_iter().zip(codes).collect();
    let mut deltas: HashMap<usize, Vec<Option<Delta>>> = HashMap::new();
    for i in (0..n).filter(|&i| matches!(images[order[i]].2, Arch::Arm64(_))) {
        let Some(root) = code_of.get(&root_of(i)) else { continue };
        let ((file, k, ..), (va, _)) = (&images[order[i]], largest(i).expect("an image of code"));
        let v = deltas.entry(files[*file].as_ptr() as usize).or_default();
        v.resize(v.len().max(k + 1), None);
        let piece = (min * 2 / PIECE_SHARE).max(PIECE_MIN);
        let root_piece = (min * 2).max(piece);
        v[*k] = Some(Delta { va, root: root.clone(), is_root: parents[i].is_none(), root_piece, piece });
    }
    Remaps(out, deltas)
}

/// The code stream `code` of a section at `va` of an image of `delta`, in `f`: in pieces apart
/// of the root's, or coded against it in pieces; as it is otherwise.
fn code_stream(f: &mut Formula, code: &[u8], va: u64, delta: Option<&Delta>) -> Node {
    let Some(d) = delta.filter(|d| d.va == va) else { return f.push(CODE, code.to_vec()) };
    if d.is_root {
        if code != &d.root[..] {
            return f.push(CODE, code.to_vec());
        }
        return Node::concat(code.chunks(d.root_piece).map(|p| f.push_apart(CODE, p.to_vec())));
    }
    let size = d.root_piece;
    let refs = arm64::delta::references(&d.root, size, code, d.piece, MOST, SAMPLE);
    let Node::Slice { stream: root, .. } = f.push_apart(REFERENCE, Bytes::shared(d.root.clone())) else {
        unreachable!("a stream's slice")
    };
    let piece = |j: usize| (j * size, (d.root.len() - j * size).min(size));
    let parts = refs.iter().zip(code.chunks(d.piece)).map(|(js, b)| {
        let a: Vec<u8> = js.iter().flat_map(|&j| &d.root[piece(j).0..][..piece(j).1]).copied().collect();
        let st = arm64::delta::encode(&a, b);
        // (where little of it is copied, as it is)
        if js.is_empty() || st[arm64::delta::EXTRA].len() > b.len() / 10 * 9 {
            return f.push(CODE, b.to_vec());
        }
        let reference = Node::concat(js.iter().map(|&j| Node::slice(root, piece(j).0 as u64, piece(j).1 as u64)));
        let inputs = std::iter::once(reference)
            .chain(st.into_iter().zip(arm64::delta::STREAM_NAMES).map(|(s, n)| f.push(format!("arm64-delta/{n}"), s)))
            .collect();
        Node::apply(ARM64_DELTA, Vec::new(), inputs, b.len() as u64)
    });
    let parts: Vec<Node> = parts.collect();
    Node::concat(parts)
}

/// The copies of the roots' code streams in `f` ([`REFERENCE`]) made the roots' own where `f`
/// holds them: each slice of a copy (pieces of the root's stream, one after another) the
/// root's pieces of the same bytes (streams apart). A copy may have been compacted already, its
/// slices moved; one with a slice not of such pieces is left as it is. (The copies are then
/// sliced no more, for [`Formula::compact`] to drop.)
pub(crate) fn resolve(f: &mut Formula) {
    let copies: Vec<usize> = (0..f.streams.len()).filter(|&s| f.streams[s].class == REFERENCE).collect();
    if copies.is_empty() {
        return;
    }
    let hash = |d: &[u8]| xxhash_rust::xxh3::xxh3_64(d);
    let ps: Vec<usize> = (0..f.streams.len()).filter(|&s| f.streams[s].apart && f.streams[s].class == CODE).collect();
    let pieces: HashMap<(usize, u64), usize> =
        ps.iter().map(|&s| ((f.streams[s].data.len(), hash(&f.streams[s].data)), s)).collect();
    let mut lens: Vec<usize> = ps.iter().map(|&s| f.streams[s].data.len()).collect();
    lens.sort_unstable_by(|a, b| b.cmp(a));
    lens.dedup();
    // the pieces of `d`, one after another
    let of = |d: &[u8]| -> Option<Vec<Node>> {
        let (mut out, mut at) = (Vec::new(), 0);
        while at < d.len() {
            let (p, n) = lens.iter().find_map(|&n| {
                let d = d.get(at..at + n)?;
                Some((pieces.get(&(n, hash(d))).copied().filter(|&p| f.streams[p].data[..] == *d)?, n))
            })?;
            out.push(Node::slice(p, 0, n as u64));
            at += n;
        }
        Some(out)
    };
    // (each slice of a copy: the pieces of its bytes, if any)
    let mut to: HashMap<(usize, u64, u64), Option<Vec<Node>>> = HashMap::new();
    f.visit(&mut |n| {
        if let &Node::Slice { stream: s, start: a, len: l } = n
            && copies.contains(&s)
        {
            to.entry((s, a, l)).or_insert_with(|| of(f.streams[s].data.get(a as usize..)?.get(..l as usize)?));
        }
    });
    let left: Vec<usize> = to.iter().filter(|(_, p)| p.is_none()).map(|(k, _)| k.0).collect();
    f.map_slices(&mut |s, a, l| match to.get(&(s, a, l)) {
        Some(Some(ps)) if !left.contains(&s) => Node::concat(ps.clone()),
        _ => Node::slice(s, a, l),
    });
}

/// `d` with each of `sections`, `(virtual address, offset, size, code)` within `d`, through
/// its filter (or a table as its layout tells), and the rest of `d` in a stream of `class`; `None` where none is filtered.
/// The images' code is remapped as `remaps` tells.
/// The sections within a CIL section are filtered within what the CIL filter keeps as it is
/// (where whole within a range of it).
fn filtered(
    d: &[u8],
    class: &'static str,
    mut sections: Vec<(u64, usize, usize, Arch)>,
    remaps: &[Option<Arc<Remap>>],
    deltas: &[Option<Delta>],
    debug: &[crate::dwarf::Debug],
) -> Option<Formula> {
    // In file order, a CIL section before those within it; sections overlapping an earlier
    // one are left in the rest.
    sections.retain(|s| s.2 > 0);
    sections.sort_by_key(|s| (s.1, !matches!(s.3, Arch::Cil(_))));

    let mut f = Formula::default();
    f.push_content(class, Kind::Unknown, Vec::new());
    let mut outputs = symbols(&mut f, d, &mut sections);
    let placed = crate::dwarf::place(&mut f, d, debug);
    if !placed.is_empty() {
        for (off, size, node) in placed {
            sections.push((0, off, size, Arch::Output(outputs.len())));
            outputs.push(Some(node));
        }
        sections.sort_by_key(|s| (s.1, !matches!(s.3, Arch::Cil(_))));
    }
    let (mut rest, mut parts, mut at) = (Vec::new(), Vec::new(), 0);
    for (i, &(va, off, size, arch)) in sections.iter().enumerate() {
        if off < at {
            continue;
        }
        let section = &d[off..off + size];
        let apply = match arch {
            Arch::Output(k) => outputs[k].take().expect("an output placed twice"),
            Arch::Stored(format) => f.push_content("nsis", Kind::Format(format), section.to_vec()),
            // (where the CIL filter turns the section down, those within it are filtered on
            // their own)
            Arch::Cil(meta) => {
                let inner: Vec<_> = sections[i + 1..]
                    .iter()
                    .take_while(|s| s.1 < off + size)
                    .map(|s| (s.0, s.1 - off, s.2, s.3))
                    .collect();
                match cil_apply(&mut f, section, va as u32, meta, &inner) {
                    Some(apply) => apply,
                    None => continue,
                }
            }
            arch => match code_apply(&mut f, section, va, arch, remaps, deltas) {
                Some(apply) => apply,
                None => continue,
            },
        };
        parts.push(Node::slice(0, rest.len() as u64, (off - at) as u64));
        rest.extend_from_slice(&d[at..off]);
        parts.push(apply);
        at = off + size;
    }
    if parts.is_empty() {
        return None;
    }
    parts.push(Node::slice(0, rest.len() as u64, (d.len() - at) as u64));
    rest.extend_from_slice(&d[at..]);
    f.streams[0].data = rest.into();
    f.root = Node::concat(parts);
    Some(f)
}

/// `section` at `va` through the filter of `arch`, x86 or ARM64 (remapped as `remaps` tells),
/// its streams added to `f`.
fn code_apply(
    f: &mut Formula,
    section: &[u8],
    va: u64,
    arch: Arch,
    remaps: &[Option<Arc<Remap>>],
    deltas: &[Option<Delta>],
) -> Option<Node> {
    let size = section.len() as u64;
    Some(match arch {
        Arch::X86(is64, k) if let Some(Remap::X86(r)) = remaps.get(k).and_then(Option::as_deref) => {
            let st = x86::remap::encode(section, va, is64, r)?;
            let params = x86_params(st.base.origin(), st.base.sentinel_size(), is64);
            let streams = st.base.streams().iter().chain(st.extra());
            let inputs = std::iter::zip(streams, x86::remap::STREAM_NAMES)
                .map(|(s, name)| f.push(format!("x86/{}", name.trim_start_matches("ST_")), s.clone()))
                .collect();
            Node::apply_with(X86_REMAP, vec![x86_resource()], params, inputs, size)
        }
        Arch::X86(is64, _) => {
            let st = x86::encode(section, va, is64)?;
            let params = x86_params(st.origin(), st.sentinel_size(), is64);
            let inputs = std::iter::zip(st.streams(), x86::STREAM_NAMES)
                .map(|(s, name)| f.push(format!("x86/{}", name.trim_start_matches("ST_")), s.clone()))
                .collect();
            Node::apply_with(X86, vec![x86_resource()], params, inputs, size)
        }
        Arch::Arm64(k) if let Some(Remap::Arm64(r)) = remaps.get(k).and_then(Option::as_deref) => {
            let st = arm64::remap::encode(section, va, r);
            let delta = deltas.get(k).and_then(Option::as_ref);
            let inputs = std::iter::zip(st.streams(), arm64::remap::STREAM_NAMES)
                .enumerate()
                .map(|(i, (s, name))| match i {
                    0 => code_stream(f, s, va, delta),
                    _ => f.push(format!("arm64/{name}"), s.clone()),
                })
                .collect();
            Node::apply(ARM64_REMAP, va.to_le_bytes().to_vec(), inputs, size)
        }
        Arch::Arm64(k) => {
            let st = arm64::encode(section, va);
            let delta = deltas.get(k).and_then(Option::as_ref);
            let inputs = std::iter::zip(st.streams(), arm64::STREAM_NAMES)
                .enumerate()
                .map(|(i, (s, name))| match i {
                    0 => code_stream(f, s, va, delta),
                    _ => f.push(format!("arm64/{name}"), s.clone()),
                })
                .collect();
            Node::apply(ARM64, va.to_le_bytes().to_vec(), inputs, size)
        }
        Arch::Table(TableKind::Reloc) => {
            // (8 first: blocks padded to 8 bytes are valid padded to 4, with more entries)
            let (align, [rvas, types]) = [3, 2].into_iter().find_map(|a| Some((a, reloc::encode(section, a)?)))?;
            let inputs = [rvas, types].into_iter().zip(reloc::STREAM_NAMES);
            let inputs = inputs.map(|(s, name)| f.push(format!("pe-reloc/{name}"), s)).collect();
            Node::apply(PE_RELOC, vec![align], inputs, size)
        }
        Arch::Table(TableKind::Rising { width, fields }) => {
            let k = width * fields;
            if !section.len().is_multiple_of(k) {
                return None;
            }
            let fl: Vec<_> =
                (0..fields).map(|i| Field { off: i * width, width, big: false, how: How::Delta }).collect();
            aqr_infer_recs::formula::apply_fields(f, "pe/table", section, k, &fl)
        }
        Arch::Cil(_) | Arch::Symbols { .. } | Arch::Output(_) | Arch::Stored(_) => return None,
    })
}

/// The symbol tables among `sections` (`Arch::Symbols`) through `exe-recons/macho-symtab`, each
/// an application of two outputs, the symbol table and the string table, in `f`: their
/// sections become `Arch::Output`s of the outputs returned, each to be placed once. Those that
/// overlap another section (or their own string tables), or that do not fit, are dropped.
fn symbols(f: &mut Formula, d: &[u8], sections: &mut Vec<(u64, usize, usize, Arch)>) -> Vec<Option<Node>> {
    let ranges: Vec<_> = sections
        .iter()
        .flat_map(|&(_, off, size, arch)| {
            let strtab = match arch {
                Arch::Symbols { stroff, strsize, .. } => Some(stroff..stroff + strsize),
                _ => None,
            };
            [Some(off..off + size), strtab].into_iter().flatten()
        })
        .collect();
    let (mut outputs, mut strtabs) = (Vec::new(), Vec::new());
    sections.retain_mut(|(_, off, size, arch)| {
        let Arch::Symbols { ext, stroff, strsize } = *arch else { return true };
        let (sym, str) = (*off..*off + *size, stroff..stroff + strsize);
        let overlaps = |r: &std::ops::Range<usize>| {
            r.start < sym.end && sym.start < r.end || r.start < str.end && str.start < r.end
        };
        if ranges.iter().filter(|r| overlaps(r)).count() != 2 || sym.start < str.end && str.start < sym.end {
            return false;
        }
        let Some((params, st)) = symtab::encode(&d[sym.clone()], &d[str.clone()], ext) else { return false };
        let inputs =
            st.into_iter().zip(symtab::STREAM_NAMES).map(|(s, name)| f.push(format!("macho-sym/{name}"), s)).collect();
        let a = Apply {
            module: MACHO_SYMTAB,
            resources: vec![],
            params,
            inputs,
            len: 0,
            memory_limit: aqr_formula::NO_LIMIT,
        };
        let k = outputs.len();
        outputs.extend(f.split(a, vec![sym.len() as u64, strsize as u64]).into_iter().map(Some));
        *arch = Arch::Output(k);
        strtabs.push((0, stroff, strsize, Arch::Output(k + 1)));
        true
    });
    sections.extend(strtabs);
    sections.sort_by_key(|s| (s.1, !matches!(s.3, Arch::Cil(_))));
    outputs
}

/// `section` at RVA `rva`, its metadata root at `meta`, through the CIL filter, its streams
/// added to `f`; the code `inner` (`(virtual address, offset, size, code)`, the offset within
/// `section`) filtered within its stream `REST` where whole within a range of it.
fn cil_apply(
    f: &mut Formula,
    section: &[u8],
    rva: u32,
    meta: usize,
    inner: &[(u64, usize, usize, Arch)],
) -> Option<Node> {
    let st = cil::encode(section, rva, meta)?;
    let mut inputs: Vec<Node> = std::iter::zip(st.streams(), cil::STREAM_NAMES)
        .map(|(s, name)| f.push(format!("cil/{name}"), s.clone()))
        .collect();
    // REST, with the code within it filtered
    let rest_stream = f.streams.len() - cil::NUM_STREAMS;
    let (mut kept, mut parts, mut inner) = (Vec::new(), Vec::new(), inner.iter().peekable());
    for r in st.rest() {
        let mut at = r.start;
        while let Some(&&(va, off, size, arch)) = inner.peek().filter(|s| s.1 < r.end) {
            inner.next();
            if off < at || off + size > r.end {
                continue;
            }
            let Some(apply) = code_apply(f, &section[off..off + size], va, arch, &[], &[]) else { continue };
            parts.push(Node::slice(rest_stream, kept.len() as u64, (off - at) as u64));
            kept.extend_from_slice(&section[at..off]);
            parts.push(apply);
            at = off + size;
        }
        parts.push(Node::slice(rest_stream, kept.len() as u64, (r.end - at) as u64));
        kept.extend_from_slice(&section[at..r.end]);
    }
    f.streams[rest_stream].data = kept.into();
    f.streams[rest_stream].content = true;
    inputs[0] = Node::concat(parts);
    let params = [rva.to_le_bytes(), (meta as u32).to_le_bytes()].concat();
    Some(Node::apply(CIL, params, inputs, section.len() as u64))
}

/// The resource of [`X86`]: the opcode classes the encoder parses by.
fn x86_resource() -> Resource {
    static R: OnceLock<Resource> = OnceLock::new();
    R.get_or_init(|| Resource::new(x86::Opcodes::BUILTIN.to_resource())).clone()
}

/// The parameters of [`X86`].
fn x86_params(origin: u64, sentinel_size: u8, is64: bool) -> Vec<u8> {
    let mut p = origin.to_le_bytes().to_vec();
    p.extend_from_slice(&[sentinel_size, is64 as u8]);
    p
}
