//! aqr-fsb as formulas (see aqr-formula): an FSB5 bank as its head and its samples' data, each a
//! stream of its own (FORMAT.md §7), so that the samples banks share are stored once and the
//! samples of a codec are compressed together. Vorbis samples whose setup the database has are
//! rebuilt by aqr-vorbis's module from the record of their packets' values (§8).

use aqr_formula::{Formula, Node};
use aqr_vorbis::setup::Ident;

use crate::fsb5::{CHUNK_VORBIS, Codec, Header, Layout};
use crate::setups;

pub use crate::fsb5::{extent, sample_sums, starts};

/// FMOD's Vorbis block sizes.
const BLOCKSIZES: [usize; 2] = [256, 2048];

/// The class of the streams of samples of `codec`.
fn class(codec: Codec) -> &'static str {
    match codec {
        Codec::Pcm16 => "fsb5/pcm16",
        Codec::ImaAdpcm => "fsb5/ima-adpcm",
        Codec::Mpeg => "fsb5/mpeg",
        Codec::Vorbis => "fsb5/vorbis",
        Codec::Other(_) => "fsb5/data",
    }
}

/// A Vorbis sample's packets as aqr-vorbis's node and the bytes it takes, with the setup the
/// database gives its CRC-32 (a row, else each candidate of the search in turn).
fn vorbis(f: &mut Formula, d: &[u8], h: &Header, data: &[u8]) -> Option<(Node, usize)> {
    let crc = u32::from_le_bytes(h.chunk(d, CHUNK_VORBIS)?.get(..4)?.try_into().ok()?);
    let db = setups::database().ok()?;
    let ident = Ident { channels: h.channels, blocksizes: BLOCKSIZES };
    db.lookup(crc).into_iter().find_map(|c| {
        let setup = db.assemble(c.skeleton, &c.ends).ok()?;
        aqr_vorbis::formula::packets(f, &setup, ident, data)
    })
}

/// The formula of the FSB5 bank `input`, or None where it does not start with one. What
/// follows the bank is kept as it is. The same whatever `threads` is.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    let l = Layout::of(input)?;
    // each sample's formula, made apart (its root, and its streams)
    let samples = aqr_common::pool::par_map(l.samples.len(), threads, |i| {
        let data = &input[l.samples[i].clone()];
        let mut f = Formula::default();
        let made = if l.codec == Codec::Vorbis { vorbis(&mut f, input, &l.headers[i], data) } else { None };
        f.root = match made {
            Some((node, used)) if used < data.len() => {
                Node::concat([node, f.push("fsb5/vorbis-end", data[used..].to_vec())])
            }
            Some((node, _)) => node,
            None => f.push(class(l.codec), data.to_vec()),
        };
        f
    });
    let mut f = Formula::default();
    let mut parts = vec![f.push("fsb5/head", input[..l.head].to_vec())];
    for s in samples {
        parts.push(f.embed(s));
    }
    if l.len() < input.len() {
        parts.push(f.push("fsb5/tail", input[l.len()..].to_vec()));
    }
    f.root = Node::concat(parts);
    Some(f)
}
