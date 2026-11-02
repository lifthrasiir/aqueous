//! DWARF debugging information in Mach-O images (their `__DWARF` segment, as of a dSYM) through
//! aqr-dwarf's modules: Apple's accelerator tables by `dwarf-apple`, from their entries and
//! `.debug_str`, which goes to a stream of its own (`dwarf/str`), placed in the file and taken by
//! the tables.

use aqr_dwarf::Section;
use aqr_formula::{Formula, Kind, Node};

/// Tables shorter than this are left as they are: the module's application, which copies
/// `.debug_str`, would cost more.
const MIN_TABLE: usize = 4 << 10;

/// A section of debugging information of an image of a file: its kind, the image's index, and
/// where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Debug {
    pub section: Section,
    pub image: usize,
    pub off: usize,
    pub size: usize,
}

/// Of the sections of debugging information `debug` of file `d`, the nodes of those aqr-dwarf
/// takes, in `f`, each with where it goes in the file (`(offset, size, node)`). Those overlapping
/// another are left out.
pub(crate) fn place(f: &mut Formula, d: &[u8], debug: &[Debug]) -> Vec<(usize, usize, Node)> {
    let mut placed = Vec::new();
    let mut images: Vec<usize> = debug.iter().map(|s| s.image).collect();
    images.sort_unstable();
    images.dedup();
    for image in images {
        let of = |section: Section| {
            let mut s = debug.iter().filter(|s| s.image == image && s.section == section);
            s.next().filter(|_| s.next().is_none()).copied()
        };
        let Some(strs) = of(Section::Str) else { continue };
        let str_bytes = &d[strs.off..strs.off + strs.size];
        let mut str_node = None;
        for kind in [Section::AppleNames, Section::AppleTypes, Section::AppleNamespaces, Section::AppleObjc] {
            let Some(t) = of(kind).filter(|t| t.size >= MIN_TABLE) else { continue };
            let node = str_node
                .get_or_insert_with(|| {
                    let node = f.push_as("dwarf/str", Kind::Text, str_bytes.to_vec());
                    placed.push((strs.off, strs.size, node.clone()));
                    node
                })
                .clone();
            if let Some(apply) = aqr_dwarf::formula::apple(f, &d[t.off..t.off + t.size], str_bytes, node) {
                placed.push((t.off, t.size, apply));
            }
        }
        // (`.debug_str` alone in a stream of its own is no gain)
        if placed.last().is_some_and(|p| p.0 == strs.off) {
            placed.pop();
            f.streams.pop();
        }
    }
    placed.sort_by_key(|p| p.0);
    let overlaps = |a: &(usize, usize, Node), b: &(usize, usize, Node)| a.0 + a.1 > b.0;
    let mut kept: Vec<(usize, usize, Node)> = Vec::new();
    for p in placed {
        if kept.last().is_none_or(|k| !overlaps(k, &p)) {
            kept.push(p);
        }
    }
    kept
}
