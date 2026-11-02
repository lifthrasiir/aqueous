//! PDB files (the MSF container of Microsoft's debugging information) rebuilt bit for bit
//! (`FORMAT.md`): the container (`msf`), its streams one after another for the archive to
//! compress, the modules' symbol records (`symbols`) by field. `formula` is the entry point (see aqr-formula).

// The code of the modules alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]
pub mod dbi;
#[cfg(feature = "encode")]
pub mod formula;
pub mod module;
pub mod msf;
pub mod symbols;

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

/// The `n` streams (at least one) of an input packed by [`pack`].
pub fn unpack(d: &[u8], n: usize) -> Result<Vec<&[u8]>> {
    let mut at = 0;
    let mut out = Vec::with_capacity(n);
    for _ in 1..n {
        let l = aqr_common::varint::get(d, &mut at).map_err(|_| corrupt())?;
        out.push(usize::try_from(l).map_err(|_| corrupt())?);
    }
    let mut parts = Vec::with_capacity(n);
    for l in out {
        parts.push(d.get(at..at.checked_add(l).ok_or_else(corrupt)?).ok_or_else(corrupt)?);
        at += l;
    }
    parts.push(d.get(at..).ok_or_else(corrupt)?);
    Ok(parts)
}

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
