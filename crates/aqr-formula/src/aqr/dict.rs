//! Dictionaries (spec, "Dictionary block"): trained for the blocks of a batch alike, of a class
//! and kind of stream compressed the same way, where they are many and small enough for one to
//! pay (docs/xform-design.md §3: the gain of a dictionary vanishes with blocks of 16 MB, and is
//! most of what per-file blocks lose to solid ones).
//!
//! A dictionary is trained (fastCover, its parameters fixed) on pieces of the blocks, then each
//! block is compressed with it, with its entropy tables and as raw content (the spec's note on
//! fast strategies, which reuse the tables of the dictionary even where a block's own would be
//! smaller), at the level the block was compressed at alone. A block takes whichever of those
//! and of no dictionary is smallest, and the dictionary is kept only where the blocks taking it
//! save more than its block takes.

use std::collections::HashMap;
use std::ffi::c_void;

use zstd::zstd_safe::zstd_sys;
use zstd::zstd_safe::{CCtx, CDict, DDict};

use super::{Decoded, Encoder, How, StreamKey, external_with, invalid, key_how};
use crate::{Error, Result};

aqr_common::zopt_namespace!("dict", "Zstandard dictionaries trained for blocks alike (aqr-formula)");

aqr_common::zopt! {
    /// Blocks alike fewer than this are not worth a dictionary.
    pub static MIN_BLOCKS: Opt<usize> = Opt::value("dict:min-blocks", 8, "fewest blocks alike that a dictionary is trained for");

    /// Blocks longer than this take no dictionary: they gain next to nothing (1.20x -> 1.12x of
    /// solid with 1 MiB blocks of rustdoc's HTML, 1.010x -> 1.009x with 16 MiB ones).
    pub static BLOCK_MAX: Opt<usize> = Opt::size("dict:block-max", 4 << 20, "blocks longer take no dictionary");

    /// A dictionary is about this share of the blocks it is trained for, within its bounds.
    static SHARE: Opt<usize> = Opt::value("dict:share", 128, "a dictionary is 1/N of the blocks it is trained for");
    static SIZE_MIN: Opt<usize> = Opt::size("dict:size-min", 16 << 10, "smallest dictionary");
    static SIZE_MAX: Opt<usize> = Opt::size("dict:size-max", 1 << 20, "largest dictionary");

    /// The most bytes of samples a dictionary is trained on (fastCover takes about 0.2 s for a
    /// dictionary of the samples of a few tens of MB), taken evenly from the blocks.
    static TRAIN_MAX: Opt<usize> = Opt::size("dict:train-max", 32 << 20, "most bytes of samples a dictionary is trained on");

    /// Samples are pieces of the blocks of at most this many bytes: the blocks are of the streams
    /// of several files one after another, whose bounds the archive encoder does not know.
    static PIECE: Opt<usize> = Opt::size("dict:piece", 8 << 10, "bytes of a piece of a block taken as a sample");

    /// fastCover's segment and d-mer sizes, fixed: tuned on the corpora of `scripts/zstd_dict_*`,
    /// k was 530-1874 and d 6 or 8.
    static K: Opt<u32> = Opt::value("dict:k", 1024, "fastCover's segment size");
    static D: Opt<u32> = Opt::value("dict:d", 8, "fastCover's d-mer size");
}

/// The magic number and the ID of a Zstandard dictionary (RFC 8878 §5) put in front of the
/// data of a dictionary block for libzstd, which takes them away again (any nonzero ID).
const DICT_MAGIC: [u8; 4] = [0x37, 0xa4, 0x30, 0xec];
const DICT_ID: u32 = 1;

/// A block that a batch writes, of a stream of the batch: its data and how it is compressed,
/// alike with those of the same `class` (its stream's class and kind).
pub struct Candidate<'a> {
    pub class: (&'a str, crate::Kind),
    pub data: &'a [u8],
    pub how: How,
}

/// The dictionaries of a batch and the blocks compressed with them.
#[derive(Default)]
pub struct Planned {
    /// The data of each dictionary block (with entropy tables), compressed.
    pub blocks: Vec<Vec<u8>>,
    /// By the key of a block ([`key_how`]): its dictionary (among `blocks`) and its
    /// compressed data.
    pub used: HashMap<StreamKey, (usize, Vec<u8>)>,
}

impl Planned {
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }
}

impl Encoder {
    /// The dictionaries for `blocks` (each compressed already, [`Encoder::precompress`]):
    /// those of each class of `dict:min-blocks` or more of at most `dict:block-max` bytes, where it
    /// saves more than its block takes.
    pub(super) fn dictionaries(&self, blocks: &[Candidate]) -> Result<Planned> {
        let mut out = Planned::default();
        if !self.trains {
            return Ok(out);
        }
        let (block_max, min_blocks) = (BLOCK_MAX.get(), MIN_BLOCKS.get());
        let mut classes: Vec<(Class, Vec<&[u8]>)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for c in blocks {
            if c.how == How::Light || c.data.is_empty() || c.data.len() > block_max {
                continue;
            }
            if !seen.insert(key_how(&[c.data], c.how)) {
                continue;
            }
            let class = (c.class.0, c.class.1, c.how);
            match classes.iter_mut().find(|g| g.0 == class) {
                Some(g) => g.1.push(c.data),
                None => classes.push((class, vec![c.data])),
            }
        }
        for ((_, _, how), data) in classes {
            if data.len() < min_blocks {
                continue;
            }
            let Some(trained) = self.trained(&data, how)? else { continue };
            let index = out.blocks.len();
            let mut used = Vec::new();
            let mut saved = 0usize;
            for (d, c) in data.iter().zip(trained.compressed) {
                if let Some((alone, with)) = c {
                    // (the block's dictionary index as well)
                    let with_len = with.len() + aqr_common::varint::len(index as u64);
                    if with_len < alone {
                        saved += alone - with_len;
                        used.push((key_how(&[d], how), with));
                    }
                }
            }
            if saved > trained.block.len() + 8 {
                out.blocks.push(trained.block);
                out.used.extend(used.into_iter().map(|(k, c)| (k, (index, c))));
            }
        }
        Ok(out)
    }

    /// A dictionary trained for `data` (blocks alike, compressed as `how` tells), and each
    /// block's compressed size alone and its smallest compressed data with the dictionary
    /// (none where it was not compressed alone). None where training fails.
    fn trained(&self, data: &[&[u8]], how: How) -> Result<Option<Trained>> {
        let total: usize = data.iter().map(|d| d.len()).sum();
        let size = (total / SHARE.get()).clamp(SIZE_MIN.get(), SIZE_MAX.get());
        let level = self.base_level();
        let Some(dict) = train(data, size, level) else { return Ok(None) };
        // (the tables' length as the decoder parses it)
        let prefix = DICT_MAGIC.len() + 4;
        let Some(header) = dict.get(prefix..).and_then(tables_len) else { return Ok(None) };
        let content = &dict[prefix + header..];

        // the level and size of each block alone
        let alone: Vec<Option<(i32, usize)>> = {
            let (streams, levels) = (self.streams.borrow(), self.levels.borrow());
            data.iter()
                .map(|d| {
                    let k = key_how(&[d], how);
                    match (streams.get(&k), levels.get(&k)) {
                        (Some(Some(c)), Some(&l)) => Some((l, c.len())),
                        _ => None,
                    }
                })
                .collect()
        };
        let mut cdicts: HashMap<i32, CDict<'static>> = HashMap::new();
        for &(l, _) in alone.iter().flatten() {
            if let std::collections::hash_map::Entry::Vacant(e) = cdicts.entry(l) {
                let c = CDict::try_create(&dict, l).ok_or_else(|| Error::new("zstd: a dictionary not taken"))?;
                e.insert(c);
            }
        }
        // in chunks of blocks, each on a context of its own (one that may refer to `content`)
        let chunks = (self.threads * 4).min(data.len()).max(1);
        let per = data.len().div_ceil(chunks);
        let made: Vec<Result<Vec<WithDict>>> = aqr_common::pool::par_map(data.len().div_ceil(per), self.threads, |j| {
            let mut c = CCtx::create();
            let range = j * per..((j + 1) * per).min(data.len());
            range
                .map(|i| {
                    let Some((l, alone)) = alone[i] else { return Ok(None) };
                    let d = &[data[i]];
                    let tables = external_with(&mut c, d, l, 1, alone, With::Tables(&cdicts[&l]))?.0;
                    let raw = external_with(&mut c, d, l, 1, alone, With::Raw(content))?.0;
                    let best = if raw.len() < tables.len() { raw } else { tables };
                    Ok(Some((alone, best)))
                })
                .collect()
        });
        let mut compressed = Vec::with_capacity(data.len());
        for m in made {
            compressed.extend(m?);
        }
        let block = self.always(&dict[DICT_MAGIC.len() + 4..])?;
        Ok(Some(Trained { block, compressed }))
    }
}

/// What blocks are alike by: their stream's class and kind, and how they are compressed.
type Class<'a> = (&'a str, crate::Kind, How);

/// A block's compressed size alone and its data compressed with a dictionary, where it was
/// compressed alone.
type WithDict = Option<(usize, Vec<u8>)>;

/// A dictionary trained for blocks: its block's data, and of each block its compressed size
/// alone and its data compressed with the dictionary.
struct Trained {
    block: Vec<u8>,
    compressed: Vec<WithDict>,
}

/// A dictionary for a compression ([`external_with`]).
#[derive(Clone, Copy)]
pub enum With<'a> {
    None,
    Tables(&'a CDict<'static>),
    Raw(&'a [u8]),
}

/// A Zstandard dictionary (with its magic number and ID) of at most `size` bytes trained on
/// pieces of `data` for `level`, or none where there are too few to train on.
fn train(data: &[&[u8]], size: usize, level: i32) -> Option<Vec<u8>> {
    let total: usize = data.iter().map(|d| d.len()).sum();
    // every `skip`-th piece of the blocks', from the first
    let (piece, train_max) = (PIECE.get(), TRAIN_MAX.get());
    let pieces = total.div_ceil(piece);
    let skip = total.div_ceil(train_max).max(1);
    let mut samples = Vec::with_capacity(total.min(train_max + piece));
    let mut sizes = Vec::with_capacity(pieces / skip + data.len());
    let mut n = 0;
    for d in data {
        for p in d.chunks(piece) {
            if n % skip == 0 {
                samples.extend_from_slice(p);
                sizes.push(p.len());
            }
            n += 1;
        }
    }
    let params = zstd_sys::ZDICT_fastCover_params_t {
        k: K.get(),
        d: D.get(),
        f: 0,
        steps: 0,
        nbThreads: 1,
        splitPoint: 1.0,
        accel: 0,
        shrinkDict: 0,
        shrinkDictMaxRegression: 0,
        zParams: zstd_sys::ZDICT_params_t { compressionLevel: level, notificationLevel: 0, dictID: DICT_ID },
    };
    let mut dict = vec![0u8; size];
    let n = unsafe {
        zstd_sys::ZDICT_trainFromBuffer_fastCover(
            dict.as_mut_ptr() as *mut c_void,
            dict.len(),
            samples.as_ptr() as *const c_void,
            sizes.as_ptr(),
            sizes.len() as u32,
            params,
        )
    };
    if unsafe { zstd_sys::ZDICT_isError(n) } != 0 {
        return None;
    }
    dict.truncate(n);
    Some(dict)
}

/// A dictionary block's decoded data, as Zstandard takes it.
pub enum Dictionary {
    /// Raw content.
    Raw(Vec<u8>),
    /// With entropy tables and repeat offsets.
    Tables(DDict<'static>),
}

impl Dictionary {
    /// The dictionary of a dictionary block's decoded data `d`, with entropy tables or not.
    pub fn read(d: Vec<u8>, tables: bool) -> Decoded<Dictionary> {
        if !tables {
            if d.len() < 8 {
                return invalid("a dictionary of less than 8 bytes of content");
            }
            return Ok(Dictionary::Raw(d));
        }
        // (the entropy tables' length parsed, the repeat offsets checked against the content; the
        // tables themselves are checked by libzstd as it loads them)
        let Some(header) = tables_len(&d) else { return invalid("bad dictionary entropy tables") };
        let content = d.len() - header;
        if content < 8 {
            return invalid("a dictionary of less than 8 bytes of content");
        }
        let reps = &d[header - 12..header];
        for r in reps.chunks(4) {
            let r = u32::from_le_bytes(r.try_into().unwrap()) as usize;
            if r == 0 || r > content {
                return invalid("a dictionary repeat offset out of its content");
            }
        }
        let full = [&DICT_MAGIC[..], &DICT_ID.to_le_bytes(), &d].concat();
        match DDict::try_create(&full) {
            Some(d) => Ok(Dictionary::Tables(d)),
            None => invalid("bad dictionary entropy tables"),
        }
    }
}

/// The length of the entropy tables and repeat offsets at the start of `d` (RFC 8878 §5's
/// `Entropy_Tables` and `Repeat_Offsets`), as their parse determines it; None where they do not
/// parse or `d` ends within them. Only the lengths are parsed: whether the tables are of use is
/// libzstd's to tell as it loads them.
///
/// (Not `ZDICT_getDictHeaderSize`, which loads the tables for compression: it links libzstd's
/// compressor, about 260 KB of the self-extracting stub, into every program decoding archives.)
fn tables_len(d: &[u8]) -> Option<usize> {
    // Huffman_Tree_Description (§4.2.1): a header byte, then the weights, 4 bits each where it
    // is 128 or more (the header less 127 of them), else FSE-compressed in as many bytes
    let header = *d.first()? as usize;
    let mut at = 1 + if header >= 128 { (header - 127).div_ceil(2) } else { header };
    // the FSE_Table_Descriptions (§4.1.1) of offsets, match lengths and literals lengths, of at
    // most so many symbols (libzstd's MaxOff, MaxML and MaxLL)
    for max_symbol in [31, 52, 35] {
        at += fse_table_len(d.get(at..)?, max_symbol)?;
    }
    at += 12;
    (at <= d.len()).then_some(at)
}

/// The length in bytes of the FSE_Table_Description at the start of `d` (RFC 8878 §4.1.1), of
/// symbols up to `max_symbol`; None where it does not parse or `d` ends within it.
fn fse_table_len(d: &[u8], max_symbol: usize) -> Option<usize> {
    let mut bit = 0usize;
    // `n` bits from `bit` on, least significant first (zero past the end: the length is checked
    // at the end)
    let peek = |bit: usize, n: usize| -> u32 {
        (0..n).fold(0, |v, i| {
            let b = bit + i;
            v | (d.get(b / 8).map_or(0, |&x| (x >> (b % 8)) as u32 & 1) << i)
        })
    };
    let log = peek(0, 4) as usize + 5;
    if log > 15 {
        return None;
    }
    bit += 4;
    let mut remaining = (1i32 << log) + 1;
    let mut threshold = 1i32 << log;
    let mut bits = log + 1;
    let mut symbol = 0;
    while remaining > 1 {
        if symbol > max_symbol {
            return None;
        }
        // a value of `bits` bits, or one bit fewer for the smallest
        let max = 2 * threshold - 1 - remaining;
        let low = peek(bit, bits - 1) as i32;
        let value = if low < max {
            bit += bits - 1;
            low
        } else {
            let v = peek(bit, bits) as i32;
            bit += bits;
            if v >= threshold { v - max } else { v }
        };
        let probability = value - 1;
        remaining -= probability.abs();
        symbol += 1;
        if probability == 0 {
            // zeros repeated, by 2-bit flags as long as they are 3
            loop {
                let repeat = peek(bit, 2) as usize;
                bit += 2;
                symbol += repeat;
                if repeat < 3 {
                    break;
                }
            }
        }
        if remaining <= 1 {
            break;
        }
        while remaining < threshold {
            bits -= 1;
            threshold >>= 1;
        }
    }
    let len = bit.div_ceil(8);
    (remaining == 1 && symbol <= max_symbol + 1 && len <= d.len()).then_some(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Blocks of words alike, for dictionaries to be trained on.
    fn blocks(n: usize, seed: u32) -> Vec<Vec<u8>> {
        let words = ["archive", "dictionary", "entropy", "table", "offset", "repeat", "literal", "match", "zstd"];
        let mut x = seed;
        (0..n)
            .map(|_| {
                let mut b = Vec::new();
                while b.len() < 3000 {
                    x = x.wrapping_mul(1103515245).wrapping_add(12345);
                    b.extend_from_slice(words[(x >> 16) as usize % words.len()].as_bytes());
                    b.extend_from_slice(format!(" {} ", (x >> 8) % 1000).as_bytes());
                }
                b
            })
            .collect()
    }

    /// libzstd's length of a dictionary's header (its magic number, ID, entropy tables and
    /// repeat offsets), where it takes it.
    fn libzstd(dict: &[u8]) -> Option<usize> {
        let n = unsafe { zstd_sys::ZDICT_getDictHeaderSize(dict.as_ptr() as *const c_void, dict.len()) };
        (unsafe { zstd_sys::ZDICT_isError(n) } == 0).then_some(n)
    }

    #[test]
    fn tables_len_is_libzstds() {
        let mut checked = 0;
        for (seed, level) in [(1, 1), (2, 3), (3, 9), (4, 19)] {
            let data = blocks(200, seed);
            let data: Vec<&[u8]> = data.iter().map(|b| &b[..]).collect();
            let dict = train(&data, 16 << 10, level).unwrap();
            assert_eq!(tables_len(&dict[8..]).map(|n| n + 8), libzstd(&dict), "level {level}");
            // and of the tables changed byte by byte, wherever libzstd takes them
            let end = libzstd(&dict).unwrap();
            let mut x = seed;
            for i in 8..end {
                for _ in 0..4 {
                    x = x.wrapping_mul(1103515245).wrapping_add(12345);
                    let mut d = dict[..end + 64].to_vec();
                    d[i] ^= 1 << ((x >> 16) % 8);
                    if let Some(n) = libzstd(&d) {
                        assert_eq!(tables_len(&d[8..]).map(|m| m + 8), Some(n), "byte {i} of level {level}");
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 100, "{checked}");
    }
}
