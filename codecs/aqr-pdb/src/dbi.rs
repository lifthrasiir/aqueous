//! What the DBI stream (stream 3) tells of the modules: each module's stream and the bytes of
//! its symbols and lines in it.

fn u16le(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(*d.get(at..)?.first_chunk()?))
}

fn u32le(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(*d.get(at..)?.first_chunk()?))
}

/// A module's stream, and the bytes of what it holds from its start: its symbols (from the
/// signature), its C11 lines and its C13 lines, one after another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Module {
    pub stream: usize,
    pub symbols: usize,
    pub c11: usize,
    pub c13: usize,
}

/// The modules of DBI stream `d` that have streams, in order; `None` where the stream does not
/// parse.
pub fn modules(d: &[u8]) -> Option<Vec<Module>> {
    // the header: its signature (-1), version, ..., the size of the modules' records at 24
    if u32le(d, 0)? != u32::MAX {
        return None;
    }
    let size = usize::try_from(u32le(d, 24)?).ok()?;
    let end = 64usize.checked_add(size)?;
    if end > d.len() {
        return None;
    }
    let mut out = Vec::new();
    let mut q = 64;
    while q < end {
        let stream = u16le(d, q + 34)?;
        let (symbols, c11, c13) = (u32le(d, q + 36)? as usize, u32le(d, q + 40)? as usize, u32le(d, q + 44)? as usize);
        if stream != u16::MAX {
            out.push(Module { stream: stream as usize, symbols, c11, c13 });
        }
        // the module's name and object's name, NUL-terminated, then padding to 4
        let mut at = q + 64;
        for _ in 0..2 {
            at += d.get(at..end)?.iter().position(|&c| c == 0)? + 1;
        }
        q = at.next_multiple_of(4);
    }
    Some(out)
}
