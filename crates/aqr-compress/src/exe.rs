//! Executables: aqr-exe's filters on each executable section, the x86 disfilter or the ARM64
//! filter, the rest of the file sliced in between (§4.2, example 1). PE images, and Mach-O
//! images, thin or in a universal file (whose images of other architectures are kept with
//! the rest). Of a .NET image, the section holding the metadata goes through the CIL filter,
//! the native code of a ReadyToRun image within what that keeps as it is. The rest of the file,
//! and what the CIL filter keeps as it is, are contents (`Stream::content`): scanned in turn,
//! e.g. for the images among the resources.

use std::io::Cursor;
use std::sync::OnceLock;

use aqr_decompress::{ARM64, CIL, X86};
use aqr_exe::arch::{arm64, cil, x86};
use aqr_exe::format::{macho, pe::locate_code};
use aqr_formula::{Formula, Kind, Node, Resource};

/// The code a section holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arch {
    /// x86-64 if true, else x86-32.
    X86(bool),
    Arm64,
    /// .NET metadata and IL, the metadata root at this offset (the virtual address is the
    /// section's RVA).
    Cil(usize),
}

pub(crate) fn pe(d: &[u8]) -> Option<Formula> {
    let code = locate_code(&mut Cursor::new(d)).ok()?;
    let arch = match code.machine {
        0x14c | 0x8664 => Some(Arch::X86(code.is64)),
        0xaa64 => Some(Arch::Arm64),
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
    filtered(d, "pe", sections)
}

pub(crate) fn macho(d: &[u8]) -> Option<Formula> {
    let images = macho::images(d)?;
    let sections = images.iter().flat_map(|i| {
        let arch = match i.cputype {
            _ if i.is_x86() => Some(Arch::X86(i.is64())),
            macho::CPU_TYPE_ARM64 => Some(Arch::Arm64),
            _ => None,
        };
        arch.into_iter().flat_map(|a| i.sections.iter().map(move |&(va, off, size)| (va, off, size, a)))
    });
    filtered(d, "macho", sections.collect())
}

/// `d` with each of `sections`, `(virtual address, offset, size, code)` within `d`, through
/// its filter, and the rest of `d` in a stream of `class`; `None` where none is filtered.
/// The sections within a CIL section are filtered within what the CIL filter keeps as it is
/// (where whole within a range of it).
fn filtered(d: &[u8], class: &'static str, mut sections: Vec<(u64, usize, usize, Arch)>) -> Option<Formula> {
    // In file order, a CIL section before those within it; sections overlapping an earlier
    // one are left in the rest.
    sections.retain(|s| s.2 > 0);
    sections.sort_by_key(|s| (s.1, !matches!(s.3, Arch::Cil(_))));

    let mut f = Formula::default();
    f.push_content(class, Kind::Unknown, Vec::new());
    let (mut rest, mut parts, mut at) = (Vec::new(), Vec::new(), 0);
    for (i, &(va, off, size, arch)) in sections.iter().enumerate() {
        if off < at {
            continue;
        }
        let section = &d[off..off + size];
        let apply = match arch {
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
            arch => match code_apply(&mut f, section, va, arch) {
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

/// `section` at `va` through the filter of `arch`, x86 or ARM64, its streams added to `f`.
fn code_apply(f: &mut Formula, section: &[u8], va: u64, arch: Arch) -> Option<Node> {
    let size = section.len() as u64;
    Some(match arch {
        Arch::X86(is64) => {
            let st = x86::encode(section, va, is64)?;
            let params = x86_params(st.origin(), st.sentinel_size(), is64);
            let inputs = std::iter::zip(st.streams(), x86::STREAM_NAMES)
                .map(|(s, name)| f.push(format!("x86/{}", name.trim_start_matches("ST_")), s.clone()))
                .collect();
            Node::apply_with(X86, vec![x86_resource()], params, inputs, size)
        }
        Arch::Arm64 => {
            let st = arm64::encode(section, va);
            let inputs = std::iter::zip(st.streams(), arm64::STREAM_NAMES)
                .map(|(s, name)| f.push(format!("arm64/{name}"), s.clone()))
                .collect();
            Node::apply(ARM64, va.to_le_bytes().to_vec(), inputs, size)
        }
        Arch::Cil(_) => return None,
    })
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
            let Some(apply) = code_apply(f, &section[off..off + size], va, arch) else { continue };
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
