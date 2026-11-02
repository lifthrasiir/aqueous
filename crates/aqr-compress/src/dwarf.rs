//! DWARF debugging information in Mach-O images (their `__DWARF` segment, as of a dSYM) and ELF
//! images (their `.debug_*` sections not compressed) through aqr-dwarf's modules:
//! `.debug_info` by `dwarf-info`, from its DIEs, `.debug_abbrev` and `.debug_str`; Apple's
//! accelerator tables by `dwarf-apple`, from their entries and `.debug_str`. The sections the
//! modules take as inputs go to streams of their own (`dwarf/str`, `dwarf/abbrev`), placed in the
//! file and taken by the applications.

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

/// The sections that the applications take as inputs (`.debug_str`, `.debug_abbrev`), each in
/// a stream of its own placed in the file once, as an application first takes it.
struct Inputs<'a> {
    d: &'a [u8],
    placed: Vec<(usize, usize, Node)>,
    nodes: Vec<(Section, Node)>,
}

impl Inputs<'_> {
    /// The node of section `s` (empty where there is none) of stream `class`.
    fn node(&mut self, f: &mut Formula, kind: Section, s: Option<Debug>, class: &'static str, as_: Kind) -> Node {
        if let Some((_, n)) = self.nodes.iter().find(|n| n.0 == kind) {
            return n.clone();
        }
        let node = match s {
            Some(s) => {
                let node = f.push_as(class, as_, self.d[s.off..s.off + s.size].to_vec());
                self.placed.push((s.off, s.size, node.clone()));
                node
            }
            None => f.push_as(class, as_, Vec::new()),
        };
        self.nodes.push((kind, node.clone()));
        node
    }
}

/// Of the sections of debugging information `debug` of file `d`, the nodes of those aqr-dwarf
/// takes, in `f`, each with where it goes in the file (`(offset, size, node)`). Those overlapping
/// another are left out.
pub(crate) fn place(f: &mut Formula, d: &[u8], debug: &[Debug]) -> Vec<(usize, usize, Node)> {
    let mut placed = Vec::new();
    let mut images: Vec<usize> = debug.iter().map(|s| s.image).collect();
    images.sort_unstable();
    images.dedup();
    let bytes = |s: Option<Debug>| s.map_or(&[][..], |s| &d[s.off..s.off + s.size]);
    for image in images {
        let of = |section: Section| {
            let mut s = debug.iter().filter(|s| s.image == image && s.section == section);
            s.next().filter(|_| s.next().is_none()).copied()
        };
        let mut inputs = Inputs { d, placed: Vec::new(), nodes: Vec::new() };
        let (strs, abbrev) = (of(Section::Str), of(Section::Abbrev));
        if let (Some(info), Some(_)) = (of(Section::Info), abbrev) {
            let nodes = |f: &mut Formula| {
                let a = inputs.node(f, Section::Abbrev, abbrev, "dwarf/abbrev", Kind::Table);
                (a, inputs.node(f, Section::Str, strs, "dwarf/str", Kind::Text))
            };
            if let Some(apply) = aqr_dwarf::formula::info(f, bytes(Some(info)), bytes(abbrev), bytes(strs), nodes) {
                inputs.placed.push((info.off, info.size, apply));
            }
        }
        if strs.is_some() {
            for kind in [Section::AppleNames, Section::AppleTypes, Section::AppleNamespaces, Section::AppleObjc] {
                let Some(t) = of(kind).filter(|t| t.size >= MIN_TABLE) else { continue };
                let node = |f: &mut Formula| inputs.node(f, Section::Str, strs, "dwarf/str", Kind::Text);
                if let Some(apply) = aqr_dwarf::formula::apple(f, bytes(Some(t)), bytes(strs), node) {
                    inputs.placed.push((t.off, t.size, apply));
                }
            }
        }
        placed.append(&mut inputs.placed);
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
