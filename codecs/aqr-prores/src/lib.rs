//! Apple ProRes (SMPTE RDD 36) recompression, so far the bitstream alone: a frame is parsed
//! into its headers and the quantized coefficients of its slices (`frame`, `coef`), and
//! written again from them bit for bit.
//!
//! ProRes frames delimit themselves (a big-endian size, then `icpf`), so they are found in any
//! container (QuickTime, MXF, Matroska) without reading it: `find` scans for them.

pub mod coef;
pub mod frame;

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
