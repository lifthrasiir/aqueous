//! Executables: aqr-exe's filters on each executable section, the x86 disfilter or the ARM64
//! filter, the rest of the file sliced in between (§4.2, example 1). PE images, and Mach-O
//! images, thin or in a universal file (whose images of other architectures are kept with
//! the rest).

use std::io::Cursor;

use aqr_decompress::{ARM64, X86};
use aqr_exe::arch::{arm64, x86};
use aqr_exe::format::{macho, pe::locate_code};
use aqr_formula::{Formula, Node};

/// The code a section holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arch {
    /// x86-64 if true, else x86-32.
    X86(bool),
    Arm64,
}

pub(crate) fn pe(d: &[u8]) -> Option<Formula> {
    let code = locate_code(&mut Cursor::new(d)).ok()?;
    let arch = match code.machine {
        0x14c | 0x8664 => Arch::X86(code.is64),
        0xaa64 => Arch::Arm64,
        _ => return None,
    };
    let sections = code.sections.iter().filter_map(|&(va, off, size)| {
        let off = usize::try_from(off).ok().filter(|&o| o < d.len())?;
        Some((va, off, size.min(d.len() - off), arch))
    });
    filtered(d, "pe", sections.collect())
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
fn filtered(d: &[u8], class: &'static str, mut sections: Vec<(u64, usize, usize, Arch)>) -> Option<Formula> {
    // In file order; sections overlapping an earlier one are left in the rest.
    sections.retain(|s| s.2 > 0);
    sections.sort_by_key(|s| s.1);

    let mut f = Formula::default();
    f.push(class, Vec::new());
    let (mut rest, mut parts, mut at) = (Vec::new(), Vec::new(), 0);
    for (va, off, size, arch) in sections {
        if off < at {
            continue;
        }
        let section = &d[off..off + size];
        let apply = match arch {
            Arch::X86(is64) => {
                let Some(st) = x86::encode(section.to_vec(), va, is64) else { continue };
                let params = x86_params(st.origin(), st.sentinel_size(), is64);
                let inputs = std::iter::zip(st.streams(), x86::STREAM_NAMES)
                    .map(|(s, name)| f.push(format!("x86/{}", name.trim_start_matches("ST_")), s.clone()))
                    .collect();
                Node::apply(X86, params, inputs, size as u64)
            }
            Arch::Arm64 => {
                let st = arm64::encode(section, va);
                let inputs = std::iter::zip(st.streams(), arm64::STREAM_NAMES)
                    .map(|(s, name)| f.push(format!("arm64/{name}"), s.clone()))
                    .collect();
                Node::apply(ARM64, va.to_le_bytes().to_vec(), inputs, size as u64)
            }
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
    f.streams[0].data = rest;
    f.root = Node::concat(parts);
    Some(f)
}

/// The parameters of [`X86`].
fn x86_params(origin: u64, sentinel_size: u8, is64: bool) -> Vec<u8> {
    let mut p = origin.to_le_bytes().to_vec();
    p.extend_from_slice(&[sentinel_size, is64 as u8]);
    p
}
