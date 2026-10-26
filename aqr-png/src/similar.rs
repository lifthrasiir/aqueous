//! Which PNG files are alike, as variants of a picture with layers shown or hidden are: most
//! of their image data the same at the same places, so that one codes well against the other
//! ([`crate::formula::encode_set`]).

use aqr_flate::inflate::{self, Sink};

use crate::png::{self, Ihdr};

/// A PNG file's image as far as likeness goes: its header, and a hash of each 256-byte segment
/// of its decompressed data.
pub struct Signature {
    header: Ihdr,
    segments: Vec<u64>,
}

impl Signature {
    /// That of the IDAT image of `file` (its first zlib stream), if it has one that inflates.
    pub fn of(file: &[u8]) -> Option<Signature> {
        let (chunks, _) = png::chunks(file)?;
        let header = Ihdr::parse(chunks.first().filter(|c| &c.kind == b"IHDR")?.data)?;
        let idat: Vec<u8> = chunks.iter().filter(|c| &c.kind == b"IDAT").flat_map(|c| c.data.iter().copied()).collect();
        // as much as the image takes, as far as the stream may hold (a header may be wrong)
        let len = crate::model::Geometry::new(&header).map_or(0, |g| g.len);
        let mut data = Collect(Vec::with_capacity(len.min(idat.len().saturating_mul(1032))));
        inflate::parse_head(idat.get(2..)?, usize::MAX, &mut data).ok()?;
        Some(Signature { header, segments: data.0.chunks(256).map(hash).collect() })
    }

    /// What signatures of images that can be coded against each other share: the geometry.
    pub fn key(&self) -> (u32, u32, u8, u8, u8) {
        let h = &self.header;
        (h.width, h.height, h.depth, h.color, h.interlace)
    }

    /// The fraction of the segments that are the same at the same places (0 for images of
    /// another geometry).
    pub fn alike(&self, other: &Signature) -> f64 {
        if self.key() != other.key() {
            return 0.0;
        }
        let same = self.segments.iter().zip(&other.segments).filter(|(a, b)| a == b).count();
        same as f64 / self.segments.len().max(other.segments.len()).max(1) as f64
    }
}

/// A 64-bit hash of a segment, for telling segments apart.
fn hash(d: &[u8]) -> u64 {
    let mix = |h: u64, w: u64| (h ^ w).wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(31);
    let words = d.chunks_exact(8);
    let rest = words.remainder();
    let mut h = words.fold(d.len() as u64, |h, w| mix(h, u64::from_le_bytes(w.try_into().unwrap())));
    if !rest.is_empty() {
        let mut b = [0u8; 8];
        b[..rest.len()].copy_from_slice(rest);
        h = mix(h, u64::from_le_bytes(b));
    }
    h
}

/// A sink keeping the data.
struct Collect(Vec<u8>);

impl Sink for Collect {
    fn lit(&mut self, b: u8) {
        self.0.push(b);
    }

    fn mat(&mut self, len: u16, dist: u16, _: bool) {
        // (the parser checks the distance)
        let (len, dist) = (len as usize, dist as usize);
        let from = self.0.len() - dist;
        if dist >= len {
            self.0.extend_from_within(from..from + len);
        } else {
            for i in 0..len {
                self.0.push(self.0[from + i]);
            }
        }
    }

    fn stored(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
}
