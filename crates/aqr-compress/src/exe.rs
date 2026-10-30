//! Executables: aqr-exe's filters on each executable section, the x86 disfilter or the ARM64
//! filter, the rest of the file sliced in between (§4.2, example 1). PE images, and Mach-O
//! images, thin or in a universal file (whose images of other architectures are kept with
//! the rest). Of a .NET image, the section holding the metadata goes through the CIL filter,
//! the native code of a ReadyToRun image within what that keeps as it is. The rest of the file,
//! and what the CIL filter keeps as it is, are contents (`Stream::content`): scanned in turn,
//! e.g. for the images among the resources. Of a PE image, its tables of known layouts outside
//! the code are taken too: the base relocations by `exe-recons/pe-reloc`, the tables of exports,
//! imports and runtime functions by `recs-fields`, their numbers as deltas. Of a 64-bit Mach-O
//! image, the symbol table and its string table by `exe-recons/macho-symtab`.
//!
//! The ARM64 and x86 images of the Mach-O files of an archive alike (other versions of one
//! program) are planned beforehand ([`plan`]) as a forest, each image's targets (of BL and
//! ADRP; of calls, jumps and RIP-relative addresses) remapped to its parent's address space, as
//! its parent's are to its own, by `exe-recons/arm64-remap` and `exe-recons/x86-remap`; so that
//! their code streams match where the code moved, for the pieces of streams of a class put
//! together (aqr-formula's `merge_plan`).

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Arc, OnceLock};

use aqr_decompress::{ARM64, ARM64_REMAP, CIL, MACHO_SYMTAB, PE_RELOC, X86, X86_REMAP};
use aqr_exe::arch::{arm64, cil, x86};
use aqr_exe::format::macho;
use aqr_exe::format::pe::{TableKind, locate_code, tables};
use aqr_exe::{reloc, symtab};
use aqr_formula::{Apply, Formula, Kind, Node, Resource};
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
    filtered(d, "pe", sections, &[])
}

/// The formula of Mach-O file `d`, its images remapped as `remaps` tells (by image).
pub(crate) fn macho(d: &[u8], remaps: &[Option<Arc<Remap>>]) -> Option<Formula> {
    let images = macho::images(d)?;
    let sections = images.iter().enumerate().flat_map(|(k, i)| {
        let arch = match i.cputype {
            _ if i.is_x86() => Some(Arch::X86(i.is64(), k)),
            macho::CPU_TYPE_ARM64 => Some(Arch::Arm64(k)),
            _ => None,
        };
        arch.into_iter().flat_map(|a| i.sections.iter().map(move |&(va, off, size)| (va, off, size, a)))
    });
    let symbols = images.iter().filter_map(|i| i.symtab).map(|s| {
        let arch = Arch::Symbols { ext: s.ext, stroff: s.stroff, strsize: s.strsize };
        (0, s.symoff, s.nsyms as usize * 16, arch)
    });
    let symbols = symbols.filter(|s| matches!(s.3, Arch::Symbols { strsize, .. } if s.2 + strsize >= MIN_SYMBOLS));
    filtered(d, "macho", sections.chain(symbols).collect(), remaps)
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

/// The remaps of the images of Mach-O files, by where each file's data is: of each image of
/// the file, its remap where it has one.
#[derive(Default)]
pub(crate) struct Remaps(HashMap<usize, Vec<Option<Arc<Remap>>>>);

impl Remaps {
    /// The remaps of the images of the file whose data is `d`.
    pub fn of(&self, d: &[u8]) -> &[Option<Arc<Remap>>] {
        self.0.get(&(d.as_ptr() as usize)).map_or(&[], |r| r)
    }
}

/// The remaps of the ARM64 and x86 images of the Mach-O files among `files`: of those of more
/// than `min` bytes of code, a maximum spanning forest of how alike they are (`Signature`; not
/// at all, of different architectures), each image remapped to its parent's, composed with its
/// parent's own. (Streams over half the
/// bound of a solid block are cut into pieces put with those alike of other streams; shorter
/// ones go one after another as their files do, rarely with their parents': remapped, they
/// cost more.)
pub(crate) fn plan(files: &[&[u8]], threads: usize, min: usize) -> Remaps {
    // each image: its file, its index in the file, its architecture (x86-64, x86-32 or ARM64)
    // and its code sections
    let mut images = Vec::new();
    for (i, d) in files.iter().enumerate() {
        for (k, image) in macho::images(d).unwrap_or_default().iter().enumerate() {
            let arch = match image.cputype {
                macho::CPU_TYPE_ARM64 => Arch::Arm64(k),
                _ if image.is_x86() => Arch::X86(image.is64(), k),
                _ => continue,
            };
            let secs: Vec<(u64, &[u8])> =
                image.sections.iter().map(|&(va, off, size)| (va, &d[off..off + size])).collect();
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
    Remaps(out)
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
) -> Option<Formula> {
    // In file order, a CIL section before those within it; sections overlapping an earlier
    // one are left in the rest.
    sections.retain(|s| s.2 > 0);
    sections.sort_by_key(|s| (s.1, !matches!(s.3, Arch::Cil(_))));

    let mut f = Formula::default();
    f.push_content(class, Kind::Unknown, Vec::new());
    let mut outputs = symbols(&mut f, d, &mut sections);
    let (mut rest, mut parts, mut at) = (Vec::new(), Vec::new(), 0);
    for (i, &(va, off, size, arch)) in sections.iter().enumerate() {
        if off < at {
            continue;
        }
        let section = &d[off..off + size];
        let apply = match arch {
            Arch::Output(k) => outputs[k].take().expect("an output placed twice"),
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
            arch => match code_apply(&mut f, section, va, arch, remaps) {
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
fn code_apply(f: &mut Formula, section: &[u8], va: u64, arch: Arch, remaps: &[Option<Arc<Remap>>]) -> Option<Node> {
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
            let inputs = std::iter::zip(st.streams(), arm64::remap::STREAM_NAMES)
                .map(|(s, name)| f.push(format!("arm64/{name}"), s.clone()))
                .collect();
            Node::apply(ARM64_REMAP, va.to_le_bytes().to_vec(), inputs, size)
        }
        Arch::Arm64(_) => {
            let st = arm64::encode(section, va);
            let inputs = std::iter::zip(st.streams(), arm64::STREAM_NAMES)
                .map(|(s, name)| f.push(format!("arm64/{name}"), s.clone()))
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
        Arch::Cil(_) | Arch::Symbols { .. } | Arch::Output(_) => return None,
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
            let Some(apply) = code_apply(f, &section[off..off + size], va, arch, &[]) else { continue };
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
