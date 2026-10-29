//! aqr-fsb as formulas (see aqr-formula): an FSB5 bank as its head and its samples' data, each a
//! stream of its own (FORMAT.md §7), so that the samples banks share are stored once and the
//! samples of a codec are compressed together. No module.

use aqr_formula::{Formula, Node};

use crate::fsb5::{Codec, Layout};

pub use crate::fsb5::{extent, sample_sums, starts};

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

/// The formula of the FSB5 bank `input`, or None where it does not start with one. What
/// follows the bank is kept as it is. The same whatever `threads` is.
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    let l = Layout::of(input)?;
    let mut f = Formula::default();
    let mut parts = vec![f.push("fsb5/head", input[..l.head].to_vec())];
    parts.extend(l.samples.iter().map(|s| f.push(class(l.codec), input[s.clone()].to_vec())));
    if l.len() < input.len() {
        parts.push(f.push("fsb5/tail", input[l.len()..].to_vec()));
    }
    f.root = Node::concat(parts);
    Some(f)
}
