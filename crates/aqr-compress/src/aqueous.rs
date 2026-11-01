//! Aqueous archives in archives: the blocks taken as they are, not decompressed. The module and
//! resource blocks are those of the outer archive, which the formula pushes as they are stored
//! (the spec's ModuleBytes and ResourceBytes): a module block the same as the outer archive's
//! own block of a module is shared with it, and any other is a resource block of its data as it
//! is. The contents of uncompressed data blocks (as of an archive at level 0) are contents of
//! their own; the rest of the archive is one stream as it is.

use aqr_common::varint;
use aqr_formula::aqr::{FOOTER_MAGIC, HEADER_MAGIC};
use aqr_formula::{Formula, Kind, Node, Resource};

/// Whether `d` starts as an unencrypted standalone archive does.
pub fn starts(d: &[u8]) -> bool {
    d.starts_with(&HEADER_MAGIC)
}

/// Whether `f` has a formula of an archive in it ([`archive`]).
pub fn is(f: &Formula) -> bool {
    f.streams.iter().any(|s| s.class == STREAM)
}

/// The class of the stream of the bytes of an archive as they are.
const STREAM: &str = "aqr";

/// A part of an archive in the formula ([`archive`]).
enum Part {
    /// Bytes of the archive as they are.
    Raw(std::ops::Range<usize>),
    /// The data of an uncompressed data block, a content.
    Content(std::ops::Range<usize>),
    Node(Node),
}

/// The formula of archive `d`, if it has any block taken apart; `None` for any other data.
/// The blocks are read as far as they are well formed, from the header to the footer, and
/// whatever is not taken apart is in the formula as it is, so that anything starting with the
/// header is rebuilt.
pub fn archive(d: &[u8]) -> Option<Formula> {
    if !starts(d) {
        return None;
    }
    let mut parts = Vec::new();
    let mut at = HEADER_MAGIC.len();
    let mut raw = 0;
    while at < d.len() && !d[at..].starts_with(&FOOTER_MAGIC) {
        let start = at;
        let Ok(ty) = varint::get(d, &mut at) else { break };
        let width = 1 << (ty % 4);
        let Some(size) = d.get(at..at + width) else { break };
        let size = u64::from_le_bytes(std::array::from_fn(|i| size.get(i).copied().unwrap_or(0)));
        at += width;
        let Some(end) = usize::try_from(size).ok().and_then(|n| at.checked_add(n)).filter(|&e| e <= d.len()) else {
            break;
        };
        let data = at..end;
        at = end + (end - start) % 2;
        // (what is taken apart, up to where)
        let (part, end) = match ty {
            // modules in full (block 60 is a shorthand block)
            0x61..=0x63 => (module(&d[data.clone()]).map(Part::Node), data.end),
            0x64..=0x67 => {
                (Resource::stored(&d[data.clone()]).ok().map(|r| Part::Node(Node::ResourceBytes(r))), data.end)
            }
            // uncompressed data blocks, `0010 0css` (leaf) and `0100 00ss` (non-leaf, checked):
            // the data before the checksum
            0x20..=0x27 | 0x40..=0x43 => {
                let checked = ty & 0x40 != 0 || ty & 0x04 != 0;
                let end = data.end.saturating_sub(if checked { 8 } else { 0 }).max(data.start);
                ((end > data.start).then(|| Part::Content(data.start..end)), end)
            }
            _ => (None, data.end),
        };
        if let Some(p) = part {
            parts.push(Part::Raw(raw..data.start));
            parts.push(p);
            raw = end;
        }
    }
    parts.push(Part::Raw(raw..d.len()));
    if !parts.iter().any(|p| !matches!(p, Part::Raw(_))) {
        return None;
    }
    // the bytes as they are in a stream of their own, the first
    let whole: Vec<u8> = parts
        .iter()
        .filter_map(|p| if let Part::Raw(r) = p { Some(&d[r.clone()]) } else { None })
        .collect::<Vec<_>>()
        .concat();
    let mut f = Formula::default();
    f.push(STREAM, whole);
    let mut nodes = Vec::new();
    let mut at = 0;
    for p in parts {
        match p {
            Part::Raw(r) => {
                nodes.push(Node::slice(0, at, r.len() as u64));
                at += r.len() as u64;
            }
            Part::Content(r) => nodes.push(f.push_content("aqr/content", Kind::Unknown, d[r].to_vec())),
            Part::Node(n) => nodes.push(n),
        }
    }
    f.root = Node::concat(nodes);
    Some(f)
}

/// The node of the data of a module block `data` as stored: the outer archive's own block of
/// the module where it is the same, else a resource block of it; `None` where it is not
/// checksummed Zstandard data.
fn module(data: &[u8]) -> Option<Node> {
    let own = aqr_formula::aqr::module_of_block_data(data)
        .and_then(|w| aqr_decompress::known(&w))
        .filter(|&m| aqr_decompress::built(m).is_some_and(|k| k.block == data));
    match own {
        Some(m) => Some(Node::ModuleBytes(m, data.into())),
        None => Resource::stored(data).ok().map(Node::ResourceBytes),
    }
}
