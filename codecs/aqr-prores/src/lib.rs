//! Apple ProRes (SMPTE RDD 36) recompression: a frame is parsed into its headers and the
//! quantized coefficients of its slices (`frame`, `coef`), which a context model codes
//! (`model`, `codec`), and written again from them bit for bit. `formula` is the entry point
//! (see aqr-formula), with the feature `encode`; `module` is the code of the module, which
//! `FORMAT.md` specifies.
//!
//! ProRes frames delimit themselves (a big-endian size, then `icpf`), so they are found in any
//! container (QuickTime, MXF, Matroska) without reading it: `find` scans for them.

// The code of the module alone uses only part of the rest.
#![cfg_attr(not(feature = "encode"), allow(unused))]
pub mod codec;
pub mod coef;
#[cfg(feature = "encode")]
pub mod formula;
pub mod frame;
pub mod model;
pub mod module;
pub mod plane;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub &'static str);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Offsets of the frames (their size fields) in `d` that parse: a frame found is skipped whole,
/// anything else byte by byte.
pub fn find(d: &[u8]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut at = 4;
    if d.len() < at {
        return out;
    }
    while let Some(i) = memchr::memmem::find(&d[at..], b"icpf").map(|i| i + at) {
        let start = i - 4;
        if let Some(n) = frame::plausible(&d[start..]) {
            out.push(start);
            at = start + n + 4;
        } else {
            at = i + 1;
        }
        if at >= d.len() {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn find_in_short_data() {
        for d in [&b""[..], b"abc", b"\0\0\0\x10icpf", b"icpf"] {
            assert!(super::find(d).is_empty());
        }
    }
}
