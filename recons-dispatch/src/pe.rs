//! PE images: exe-recons' x86 disfilter on each executable section, the rest of the image
//! sliced in between (§4.2, example 1).

use std::io::Cursor;

use exe_recons::arch::x86;
use exe_recons::format::pe::locate_code;
use recons_formula::{Formula, Node};

use crate::native::{X86, x86_params};
use crate::{Options, verified};

pub(crate) fn pe(d: &[u8], opt: &Options) -> Option<Formula> {
    let code = locate_code(&mut Cursor::new(d)).ok()?;
    // In file order, cut to the file; sections overlapping an earlier one are left in the rest.
    let mut sections: Vec<(u64, usize, usize)> = code
        .sections
        .iter()
        .filter_map(|&(va, off, size)| {
            let off = usize::try_from(off).ok().filter(|&o| o < d.len())?;
            Some((va, off, size.min(d.len() - off))).filter(|s| s.2 > 0)
        })
        .collect();
    sections.sort_by_key(|s| s.1);

    let mut f = Formula::default();
    f.push("pe", Vec::new());
    let (mut rest, mut parts, mut at) = (Vec::new(), Vec::new(), 0);
    for (va, off, size) in sections {
        if off < at {
            continue;
        }
        let section = &d[off..off + size];
        let Some(st) = x86::encode(section.to_vec(), va, code.is64) else { continue };
        let params = x86_params(st.origin(), st.sentinel_size(), code.is64);
        let inputs: Vec<&[u8]> = st.streams().iter().map(|s| &s[..]).collect();
        if !verified(opt, X86, &params, &inputs, section) {
            continue;
        }
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
