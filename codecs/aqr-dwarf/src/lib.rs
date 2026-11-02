//! DWARF debugging information rebuilt bit for bit (`FORMAT.md`): of each section, what follows
//! from the others derived, and the rest split into streams for the archive to compress.
//! `formula` is the entry point (see aqr-formula).

// The code of the modules alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]
pub mod apple;
#[cfg(feature = "encode")]
pub mod formula;
pub mod info;
pub mod module;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[cold]
pub fn corrupt() -> Error {
    Error("corrupt input")
}

/// Streams packed into an input: the varints of the lengths of all but the last, then the
/// streams one after another, the last taking what is left.
pub fn pack(streams: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(streams.iter().map(|s| s.len() + 4).sum());
    if let Some((_, init)) = streams.split_last() {
        for s in init {
            aqr_common::varint::put(&mut out, s.len() as u64);
        }
    }
    for s in streams {
        out.extend_from_slice(s);
    }
    out
}

/// The `n` streams of an input packed by [`pack`].
pub fn unpack(d: &[u8], n: usize) -> Result<Vec<&[u8]>> {
    let mut at = 0;
    let mut lens = Vec::with_capacity(n);
    for _ in 1..n {
        let l = aqr_common::varint::get(d, &mut at).map_err(|_| corrupt())?;
        lens.push(usize::try_from(l).map_err(|_| corrupt())?);
    }
    let mut out = Vec::with_capacity(n);
    for l in lens {
        let s = d.get(at..at.checked_add(l).ok_or_else(corrupt)?).ok_or_else(corrupt)?;
        out.push(s);
        at += l;
    }
    if n > 0 {
        out.push(&d[at..]);
    } else if at != d.len() {
        return Err(corrupt());
    }
    Ok(out)
}

/// A section of DWARF (or of Apple's tables beside it), as ELF (`.debug_info`) and Mach-O
/// (`__debug_info`, cut to 16 bytes) name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Info,
    Abbrev,
    Str,
    StrOffsets,
    LineStr,
    Line,
    Addr,
    Aranges,
    Ranges,
    Rnglists,
    Loc,
    Loclists,
    Names,
    Frame,
    AppleNames,
    AppleTypes,
    AppleNamespaces,
    AppleObjc,
}

impl Section {
    /// The section named `name`, as ELF or Mach-O names it; `None` for any other.
    pub fn by_name(name: &[u8]) -> Option<Section> {
        let name = name.strip_prefix(b"__").or_else(|| name.strip_prefix(b"."))?;
        Some(match name {
            b"debug_info" => Section::Info,
            b"debug_abbrev" => Section::Abbrev,
            b"debug_str" => Section::Str,
            b"debug_str_offsets" | b"debug_str_offs" => Section::StrOffsets,
            b"debug_line_str" => Section::LineStr,
            b"debug_line" => Section::Line,
            b"debug_addr" => Section::Addr,
            b"debug_aranges" => Section::Aranges,
            b"debug_ranges" => Section::Ranges,
            b"debug_rnglists" => Section::Rnglists,
            b"debug_loc" => Section::Loc,
            b"debug_loclists" => Section::Loclists,
            b"debug_names" => Section::Names,
            b"debug_frame" => Section::Frame,
            b"apple_names" => Section::AppleNames,
            b"apple_types" => Section::AppleTypes,
            b"apple_namespac" | b"apple_namespaces" => Section::AppleNamespaces,
            b"apple_objc" => Section::AppleObjc,
            _ => return None,
        })
    }
}
