//! Executables: aqr-exe's x86 disfilter on each executable section, the rest of the file
//! sliced in between (§4.2, example 1). PE images, and Mach-O images of x86 or x86-64, thin or
//! in a universal file (whose other architectures are kept with the rest).

use std::io::Cursor;

use aqr_decompress::X86;
use aqr_exe::arch::x86;
use aqr_exe::format::{macho, pe::locate_code};
use aqr_formula::{Formula, Node};

pub(crate) fn pe(d: &[u8]) -> Option<Formula> {
    let code = locate_code(&mut Cursor::new(d)).ok()?;
    let sections = code.sections.iter().filter_map(|&(va, off, size)| {
        let off = usize::try_from(off).ok().filter(|&o| o < d.len())?;
        Some((va, off, size.min(d.len() - off), code.is64))
    });
    filtered(d, "pe", sections.collect())
}

pub(crate) fn macho(d: &[u8]) -> Option<Formula> {
    let images = macho::images(d)?;
    let sections = images
        .iter()
        .filter(|i| i.is_x86())
        .flat_map(|i| i.sections.iter().map(|&(va, off, size)| (va, off, size, i.is64())));
    filtered(d, "macho", sections.collect())
}

/// `d` with each of `sections`, `(virtual address, offset, size, 64-bit)` within `d`, through
/// the disfilter, and the rest of `d` in a stream of `class`; `None` where none is filtered.
fn filtered(d: &[u8], class: &'static str, mut sections: Vec<(u64, usize, usize, bool)>) -> Option<Formula> {
    // In file order; sections overlapping an earlier one are left in the rest.
    sections.retain(|s| s.2 > 0);
    sections.sort_by_key(|s| s.1);

    let mut f = Formula::default();
    f.push(class, Vec::new());
    let (mut rest, mut parts, mut at) = (Vec::new(), Vec::new(), 0);
    for (va, off, size, is64) in sections {
        if off < at {
            continue;
        }
        let section = &d[off..off + size];
        let Some(st) = x86::encode(section.to_vec(), va, is64) else { continue };
        let params = x86_params(st.origin(), st.sentinel_size(), is64);
        parts.push(Node::slice(0, rest.len() as u64, (off - at) as u64));
        rest.extend_from_slice(&d[at..off]);
        let inputs = std::iter::zip(st.streams(), x86::STREAM_NAMES)
            .map(|(s, name)| f.push(format!("x86/{}", name.trim_start_matches("ST_")), s.clone()))
            .collect();
        parts.push(Node::apply(X86, params, inputs, size as u64));
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
