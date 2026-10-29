//! aqr-mp3 as a module of entry formulas (see aqr-formula). The formula is one application of
//! [`MODULE`] on ten input slots, the streams of the coded form ([`crate::codec::Coded`])
//! stored raw, and no parameters. FORMAT.md specifies it.

use crate::codec::{self};
use crate::par;
use aqr_formula::{Formula, Kind, Module, Node, Resource};
use std::sync::OnceLock;

use crate::module::SLOTS;
pub use crate::module::{Error, decode};

pub const MODULE: Module = Module::new("mp3-recons", 5).with_memory(memory);

/// The memory of the module: the inputs and the file; the rANS words, the raw bits and the
/// tail bits, raw granules and gaps copied (padded); the frames (about 124 bytes each on 32 bits,
/// one for 4 bytes of the headers at least) and the runs and junk (of the segments); the
/// reservoir (1 MiB, 2 MiB at most); and the tables the resource expands to (about 21 times the
/// built-in one's 131 KB, measured as WebAssembly).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[segs, _, hdr, _, tail, raw_gr, gaps, _, words, raw] = u.inputs else { return None };
    let &[res] = u.resources else { return None };
    let copies = words + raw + tail + raw_gr + gaps + 4 * 64;
    let frames = 31 * hdr + 24 * segs;
    Some(u.whole(copies + frames + (2 << 20) + 24 * res, 10))
}

/// Classes of the slots, and their kinds: the side information, the rANS words and the raw
/// bits do not compress.
const CLASSES: [(&str, Kind); SLOTS] = [
    ("mp3/segs", Kind::Unknown),
    ("mp3/junk", Kind::Unknown),
    ("mp3/headers", Kind::Unknown),
    ("mp3/crc", Kind::Unknown),
    ("mp3/tailbits", Kind::Unknown),
    ("mp3/rawgr", Kind::Unknown),
    ("mp3/gaps", Kind::Unknown),
    ("mp3/side", Kind::Compressed),
    ("mp3/rans", Kind::Compressed),
    ("mp3/rawbits", Kind::Compressed),
];

/// The formula of `input`: `None` where the coded form is not smaller (or on a failure).
/// `threads` threads give the same formula.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    let c = std::panic::catch_unwind(|| {
        let c = if threads > 1 && input.len() >= par::PAR_MIN { par::encode(input) } else { codec::encode(input) };
        let n: usize = c.streams.iter().map(Vec::len).sum::<usize>() + c.words.len() + c.raw.len();
        (n + 64 < input.len()).then_some(c)
    })
    .ok()??;
    let mut f = Formula::default();
    let mut slots: Vec<Vec<u8>> = c.streams;
    slots.push(c.words);
    slots.push(c.raw);
    let inputs = slots.into_iter().zip(CLASSES).map(|(d, (class, kind))| f.push_as(class, kind, d)).collect();
    f.root = Node::apply_with(MODULE, vec![resource()], Vec::new(), inputs, input.len() as u64);
    Some(f)
}

/// The module's resource: the built-in frequency tables, uncompressed.
pub fn resource() -> Resource {
    static R: OnceLock<Resource> = OnceLock::new();
    R.get_or_init(|| Resource::new(codec::builtin_raw().clone())).clone()
}
