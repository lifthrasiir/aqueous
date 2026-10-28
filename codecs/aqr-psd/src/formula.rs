//! aqr-psd as a module of entry formulas (see aqr-formula): [`decode`] on four inputs,
//! the head, the skeleton, the embedded files and the model stream, and no parameters. See
//! FORMAT.md, *Formula module*.
//!
//! Embedded files coded by a recompressor (PSD/PSB, PNG, JPEG) are not decoded by the module:
//! their bytes are the third input, which the formula fills with the children's formulas.

use crate::codec::{self, Par};
use crate::psd::Compression;
use aqr_formula::{Apply, Formula, Kind, Module, Node, Region};

pub use crate::module::decode;

pub const MODULE: Module = Module { name: "psd-recons", version: 1 };

/// The formula giving `input`, or `None` where it is not coded: not a PSD/PSB
/// that can be parsed, or no pixel data or embedded file to take out.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    encode_at(input, 0, &Par::new(threads))
}

/// What the parts of `input` are, where it can be parsed (whether [`encode`] codes it or not),
/// in file order: the pixel data of the layers' channels, of the composite image and of the
/// patterns, whatever their depth (ZIP data as zlib streams of pixels); the files embedded in
/// smart objects and the JPEG thumbnail; text (XMP, the text engine's data); and the structure
/// (layer records with their blocks, the other resources and global blocks) as tables. Not told:
/// the header and the color mode data, the rest of the blocks of embedded files (whose files of
/// other types may be anywhere in them), and `Layr`/`Lr16`/`Lr32` blocks (whose layers' pixel
/// data is told).
pub fn regions(input: &[u8]) -> Option<Vec<Region>> {
    let psd = crate::psd::parse(input).ok()?;
    let pixels = |span: std::ops::Range<usize>| {
        let comp = Compression::from_u16(u16::from_be_bytes(input.get(span.start..span.start + 2)?.try_into().ok()?));
        let span = span.start + 2..span.end;
        Some(match comp {
            Compression::Zip | Compression::ZipPred => {
                Region { span, kind: Kind::Format("zlib"), contents: Kind::Pixels }
            }
            _ => Region { span, kind: Kind::Pixels, contents: Kind::Unknown },
        })
    };
    let mut out: Vec<Region> =
        psd.layer_info.layers.iter().flat_map(|l| &l.channels).filter_map(|c| pixels(c.span.clone())).collect();
    for f in crate::psd::linked(&psd, input) {
        let name = match &f.file_type {
            b"8BPB" | b"8BPS" => "psd",
            b"png " | b"PNGf" => "png",
            b"JPEG" => "jpeg",
            _ => continue,
        };
        out.push(Region { span: f.data, kind: Kind::Format(name), contents: Kind::Unknown });
    }
    out.extend(pixels(psd.composite.clone()));
    let part = |span: std::ops::Range<usize>, kind: Kind| Region { span, kind, contents: Kind::Unknown };
    for r in &psd.resources {
        let d = &input[r.data.clone()];
        out.push(match r.id {
            // a JPEG after a header of 28 bytes, its format 1
            1033 | 1036 if d.len() > 28 && d[..4] == [0, 0, 0, 1] => {
                part(r.data.start + 28..r.data.end, Kind::Format("jpeg"))
            }
            1060 => part(r.data.clone(), Kind::Text),
            _ => part(r.data.clone(), Kind::Table),
        });
    }
    out.extend(psd.layer_info.layers.iter().map(|l| part(l.record.clone(), Kind::Table)));
    out.extend(psd.global_mask.clone().map(|m| part(m, Kind::Table)));
    for t in &psd.global_tags {
        out.extend(match &t.key {
            b"lnk2" | b"lnkD" | b"lnk3" | b"Layr" | b"Lr16" | b"Lr32" => None,
            b"Txt2" => Some(part(t.data.clone(), Kind::Text)),
            b"Patt" | b"Pat2" | b"Pat3" => Some(part(t.data.clone(), Kind::Pixels)),
            _ => Some(part(t.data.clone(), Kind::Table)),
        });
    }
    out.retain(|r| !r.span.is_empty());
    out.sort_by_key(|r| r.span.start);
    // (spans of a well-formed file do not overlap; of another, the first is kept)
    let mut end = 0;
    out.retain(|r| {
        let keep = r.span.start >= end;
        if keep {
            end = r.span.end;
        }
        keep
    });
    Some(out)
}

/// [`encode`] for a file `depth` deep in the embedded files of another.
pub(crate) fn encode_at(input: &[u8], depth: usize, par: &Par) -> Option<Formula> {
    let parts =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| codec::formula_parts(input, depth, par))).ok()??;
    let mut f = Formula::default();
    let head = f.push("psd/head", parts.head);
    let skeleton = f.push("psd/skeleton", parts.skeleton);
    let embedded = Node::concat(parts.embedded.into_iter().map(|child| f.embed(child)).collect::<Vec<_>>());
    let model = f.push("psd/model", parts.model);
    f.root = Node::apply(MODULE, Vec::new(), vec![head, skeleton, embedded, model], input.len() as u64);
    Some(f)
}

/// Runs the modules a formula of this module has in it (this one, aqr-png and aqr-jpeg).
pub fn apply(a: &Apply, inputs: &[&[u8]], threads: usize) -> aqr_formula::Result<Vec<u8>> {
    let err = |e: &dyn std::fmt::Debug| aqr_formula::Error::new(format!("{}: {e:?}", a.module));
    if a.module == MODULE {
        decode(&a.params, inputs, a.len, threads).map_err(|e| err(&e))
    } else if a.module == aqr_png::formula::MODULE {
        aqr_png::formula::decode(&a.params, inputs, a.len, threads).map_err(|e| err(&e))
    } else if a.module == aqr_jpeg::formula::MODULE {
        aqr_jpeg::formula::decode(&a.params, inputs, a.len, threads).map_err(|e| err(&e))
    } else {
        Err(aqr_formula::Error::new(format!("no module {}", a.module)))
    }
}
