//! Synthetic frames for tests: random sparse coefficients, in any size.

use crate::coef::Tail;
use crate::frame::{self, Frame, Header, Picture, Slice};

/// A frame of `w` x `h` (4:2:2 or 4:4:4) with sparse random coefficients from `seed`.
pub fn synthetic(w: u16, h: u16, chroma: u8, interlace: u8, seed: u64) -> Vec<u8> {
    let mut rng = seed;
    let mut next = move || {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (rng >> 33) as u32
    };
    let mut raw = vec![0u8; 20];
    raw[..2].copy_from_slice(&20u16.to_be_bytes());
    raw[4..8].copy_from_slice(b"test");
    raw[8..10].copy_from_slice(&w.to_be_bytes());
    raw[10..12].copy_from_slice(&h.to_be_bytes());
    raw[12] = chroma << 6 | interlace << 2;
    let header = Header::new(raw).unwrap();
    let rows = (h as usize).div_ceil(16 * header.pictures());
    let pictures = (0..header.pictures())
        .map(|_| {
            let mut ph = vec![8 << 3, 0, 0, 0, 0, 0, 0, 1 << 4];
            let ws = frame::widths(&header, &ph);
            ph[5..7].copy_from_slice(&((ws.len() * rows) as u16).to_be_bytes());
            let slices = (0..ws.len() * rows)
                .map(|i| {
                    let mbs = ws[i % ws.len()];
                    let comps = std::array::from_fn(|k| {
                        let blocks = if k == 0 || chroma == 3 { 4 * mbs } else { 2 * mbs };
                        let c = (0..blocks * 64)
                            .map(|j| match next() % 8 {
                                _ if j % 64 == 0 => (next() % 4000) as i16 - 2000,
                                0 => (next() % 600) as i16 - 300,
                                1 => (next() % 6) as i16 - 3,
                                _ => 0,
                            })
                            .collect();
                        (c, Tail { zero_bytes: (next() % 50 == 0) as u32, empty: false })
                    });
                    let q = 1 + (next() % 3) as u8;
                    Slice { header: vec![6 << 3, q, 0, 0, 0, 0], mbs, comps, alpha: Vec::new(), sizes: [0; 3] }
                })
                .collect();
            Picture { header: ph, slices }
        })
        .collect();
    Frame { header, pictures, trailer: vec![0; (next() % 3) as usize * 16] }.write()
}
