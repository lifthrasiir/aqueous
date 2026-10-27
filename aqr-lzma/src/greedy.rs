//! A simple LZMA encoder, greedy: for tests, here and of other crates, where XZ Utils is not to
//! be had. At each position, a repeated match where it is as long as the longest match, else the
//! longest match, else a short repeat, else a literal.

use crate::alone;
use crate::lzma::{self, Kind, MAX_LEN, Packet, Props};
use crate::mf::{Mf, lcp};

/// The packets of `data` with matches at most `dict` back.
pub fn packets(data: &[u8], dict: usize) -> Vec<Packet> {
    let mut mf = Mf::new(data, dict);
    let mut out = vec![];
    let (mut pos, mut rep0) = (0, 0usize);
    let mut pairs = vec![];
    while pos < data.len() {
        mf.advance(pos);
        let cap = (MAX_LEN as usize).min(data.len() - pos);
        let n = mf.longest(pos, 16, cap, cap);
        mf.pairs(pos, cap, &mut pairs);
        let r = if rep0 < pos { lcp(data, pos - rep0 - 1, pos, cap) } else { 0 };
        let pk = if r >= 2 && r >= n {
            Packet { kind: Kind::Rep0, len: r as u16, dist: rep0 as u32 }
        } else if n >= 2
            && let Some(&(l, c)) = pairs.last()
        {
            rep0 = pos - c as usize - 1;
            Packet { kind: Kind::Match, len: l as u16, dist: rep0 as u32 }
        } else if r >= 1 {
            Packet::SHORT_REP
        } else {
            Packet::LIT
        };
        out.push(pk);
        pos += pk.len as usize;
    }
    out
}

/// A `.lzma` file of `data`: properties lc 3, lp 0, pb 2, a dictionary of 1 MiB, the size
/// unknown and the end marker.
pub fn alone(data: &[u8]) -> Vec<u8> {
    const DICT: u32 = 1 << 20;
    let props = Props { lc: 3, lp: 0, pb: 2 };
    let mut f = vec![props.byte()];
    f.extend(DICT.to_le_bytes());
    f.extend(u64::MAX.to_le_bytes());
    debug_assert_eq!(f.len(), alone::HEADER);
    f.extend(lzma::encode(data, props, &packets(data, DICT as usize), true));
    f
}
