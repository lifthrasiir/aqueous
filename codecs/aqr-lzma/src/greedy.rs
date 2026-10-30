//! A simple LZMA encoder, greedy: for tests, here and of other crates, where XZ Utils is not to
//! be had. At each position, a repeated match where it is as long as the longest match, else the
//! longest match, else a short repeat, else a literal.

use aqr_common::{crc32, varint};

use crate::lzma::{self, Kind, MAX_LEN, Packet, Props};
use crate::lzma2::{self, Chunk};
use crate::mf::{Mf, lcp};
use crate::{alone, xz};

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

/// A raw LZMA2 stream of `data`: its packets with matches at most `dict` back, properties lc 3,
/// lp 0, pb 2, in LZMA chunks of at most 32 KiB of data (coded in fewer than 64 KiB), the first
/// resetting everything and the others nothing.
pub fn lzma2(data: &[u8], dict: usize) -> Vec<u8> {
    let props = Props { lc: 3, lp: 0, pb: 2 };
    let packets = packets(data, dict);
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut len = 0;
    for (i, pk) in packets.iter().enumerate() {
        len += pk.len as usize;
        let next = packets.get(i + 1).map_or(usize::MAX, |p| p.len as usize);
        if len + next > 32 << 10 || i + 1 == packets.len() {
            let control = if chunks.is_empty() { 0xe0 } else { 0x80 };
            chunks.push(Chunk { control, len, props: (control == 0xe0).then_some(props) });
            len = 0;
        }
    }
    let mut s = lzma2::encode(data, &chunks, &packets).expect("chunks of the packets");
    s.push(0);
    s
}

/// An `.xz` file of `data`: a block of LZMA2 ([`lzma2`], a dictionary of 1 MiB) and its CRC-32.
pub fn xz(data: &[u8]) -> Vec<u8> {
    let flags = [0, 1];
    let mut f = xz::MAGIC.to_vec();
    f.extend(flags);
    f.extend(crc32::hash(&flags).to_le_bytes());
    // the block header: its size, flags (a filter, no sizes), LZMA2 of a dictionary of 1 MiB
    let header = [2, 0, 0x21, 1, 16, 0, 0, 0];
    f.extend(header);
    f.extend(crc32::hash(&header).to_le_bytes());
    let s = lzma2(data, 1 << 20);
    f.extend(&s);
    f.resize(f.len().next_multiple_of(4), 0);
    f.extend(crc32::hash(data).to_le_bytes());
    let mut index = vec![0, 1];
    varint::put(&mut index, (12 + s.len() + 4) as u64);
    varint::put(&mut index, data.len() as u64);
    index.resize(index.len().next_multiple_of(4), 0);
    index.extend(crc32::hash(&index).to_le_bytes());
    f.extend(&index);
    let mut footer = ((index.len() / 4 - 1) as u32).to_le_bytes().to_vec();
    footer.extend(flags);
    f.extend(crc32::hash(&footer).to_le_bytes());
    f.extend(footer);
    f.extend(b"YZ");
    f
}
