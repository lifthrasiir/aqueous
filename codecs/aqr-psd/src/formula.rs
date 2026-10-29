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

pub const MODULE: Module = Module::new("psd-recons", 1).with_memory(memory);

/// What the module holds at most besides its inputs and the file (the output, made at its size),
/// for the head (the spans and groups, read as `codec::get_head` does, with sizes for their
/// structures on 32 bits: 96 bytes a span, 32 a group, and the vectors of a span's planes, rows
/// not reproduced, a group's planes and equal planes, the last grown by pushes: three times) and:
/// - the stream's chunk: the decoding tables of the 402 contexts and the unused one (3 KiB
///   each), the old ones held while the new are made, with the chunk's frequencies (1 KiB
///   each), at most as many as the stream has bits;
/// - the group in hand, the largest: per plane its row, the coder's four rows and the hint's if
///   hinted, a row a PackBits plane for the sink's previous one, each the width and a vector's
///   header, then a byte buffer of the width grown to twice (three times in all), and the
///   plane's slots (192 bytes);
/// - for a hinted group, the file's layers as parsed up to the composite (rebuilt here from the
///   skeleton, the spans zeroed) and the renderer: its lists (512 bytes a layer, 16 for each of
///   at most three operations, kept three times), the rows of the canvas a group level needs (4
///   bytes a pixel, a level for each group divider and one more), five row buffers of the widest
///   channel (three times, as they grow), the clipping row, and each channel of a drawn layer
///   that is not RLE or raw as a plane (`rows::PlaneRows::channel`), whole: those of no
///   compression field (shorter than two bytes) at their size. A channel of ZIP is inflated
///   whole with its tokens and per-block statistics, which the compressed bytes do not bound
///   but by the worst case (a block of 10 bits, 1.7 KiB): where there is one, the estimate is
///   None.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let (&[], &[head, skeleton, _, model]) = (u.params, u.contents) else { return None };
    let total = usize::try_from(u.output()).ok().filter(|&t| t as u64 <= u32::MAX as u64)?;
    let h = Head::parse(head, total)?;
    let n_spans = h.spans.len() as u64;
    let mut peak = (0u64, 0u64);
    for g in &h.groups {
        let (bytes, buffers) = g.memory(&h, skeleton)?;
        peak = (peak.0.max(bytes), peak.1.max(buffers));
    }
    // (the stream's chunk: 402 tables of the model and the unused one)
    let used = (crate::model::TABLES as u64).min(8 * model.len() as u64) + 1;
    let player = 2 * used * (2048 + 1024) + used * (1024 + 16) + 3 * 12 * 512 + 2 * crate::model::TABLES as u64;
    let state = h.bytes + n_spans + 4 * n_spans + player + peak.0;
    Some(u.whole(state, 32 + peak.1))
}

/// The head stream's parts that take memory.
struct Head {
    spans: Vec<HeadSpan>,
    groups: Vec<HeadGroup>,
    /// Bytes of the structures made of it.
    bytes: u64,
}

struct HeadSpan {
    start: usize,
    end: usize,
    w: usize,
    h: usize,
    kind: u8,
}

struct HeadGroup {
    /// The span of the first plane.
    span: usize,
    planes: usize,
    hinted: bool,
    /// The planes of PackBits spans that are written.
    rle: usize,
}

/// The number of items a vector holds, made with capacity `n` up to `1 << 16` and then grown
/// by pushes: with room for three times that, the old blocks being held.
fn grown(n: u64) -> u64 {
    if n <= 1 << 16 { n } else { 3 * n }
}

impl Head {
    fn parse(src: &[u8], total: usize) -> Option<Head> {
        let mut at = 0;
        let num = |at: &mut usize| -> Option<usize> { usize::try_from(aqr_common::varint::get(src, at).ok()?).ok() };
        let byte = |at: &mut usize| -> Option<u8> {
            let b = *src.get(*at)?;
            *at += 1;
            Some(b)
        };
        let n = num(&mut at)?;
        let mut spans: Vec<HeadSpan> = Vec::new();
        let (mut pos, mut bytes) = (0usize, 0u64);
        for _ in 0..n {
            let gap = num(&mut at)?;
            let len = num(&mut at)?;
            let start = pos.checked_add(gap)?;
            let end = start.checked_add(len).filter(|&e| e <= total)?;
            pos = end;
            let kind = byte(&mut at)?;
            let (w, h) = match kind {
                0 | 2 | 4 => {
                    let planes = num(&mut at)?;
                    if planes > 64 {
                        return None;
                    }
                    let (w, h) = (num(&mut at)?, num(&mut at)?);
                    if kind != 0 {
                        // (the encoder: a tag, and up to two numbers)
                        match byte(&mut at)? {
                            0 => {
                                num(&mut at)?;
                                num(&mut at)?;
                            }
                            1 | 2 => {
                                num(&mut at)?;
                            }
                            _ => return None,
                        }
                        for _ in 0..planes {
                            num(&mut at)?;
                        }
                        let ne = num(&mut at)?;
                        for _ in 0..ne {
                            num(&mut at)?;
                            let bl = num(&mut at)?;
                            at = at.checked_add(bl).filter(|&a| a <= src.len())?;
                        }
                        bytes += 4 * planes as u64 + 12 * grown(ne as u64);
                    }
                    (w, h)
                }
                16 => (0, 0),
                32 => {
                    let of = num(&mut at)?;
                    let s = spans.get(of)?;
                    (s.w, s.h)
                }
                _ => return None,
            };
            spans.push(HeadSpan { start, end, w, h, kind });
        }
        bytes += 96 * grown(n as u64) + 8 * n as u64;
        let ng = num(&mut at)?;
        let mut groups = Vec::new();
        for _ in 0..ng {
            let np = num(&mut at)?;
            let flags = byte(&mut at)?;
            if np == 0 || np > 64 || flags > 3 {
                return None;
            }
            let (mut first, mut rle) = (usize::MAX, 0);
            for _ in 0..np {
                let s = num(&mut at)?;
                num(&mut at)?;
                num(&mut at)?;
                let sp = spans.get(s)?;
                if first == usize::MAX {
                    first = s;
                }
                rle += matches!(sp.kind, 2 | 4) as usize;
            }
            // (planes 8 bytes, equal planes 8, the latter pushed: room for twice, three times)
            bytes += 32 * 3 + 8 * np as u64 + 3 * 8 * (np as u64 * 2 + 4);
            groups.push(HeadGroup { span: first, planes: np, hinted: flags & 2 != 0, rle });
        }
        bytes += 32 * grown(ng as u64);
        (at == src.len()).then_some(Head { spans, groups, bytes })
    }
}

impl HeadGroup {
    /// The memory of decoding the group, and its large allocations: see [`memory`].
    fn memory(&self, h: &Head, skeleton: &[u8]) -> Option<(u64, u64)> {
        let (n, w) = (self.planes as u64, h.spans[self.span].w as u64);
        // (the rows, the coder's four, the hint's if hinted, and a sink's previous row if PackBits)
        let rows =
            (5 + self.hinted as u64) * n * (w + 12) + self.rle as u64 * (w + 12) + 3 * (w + w / 64 + 8) + n * 192;
        let large = |bytes: u64| (bytes >= aqr_formula::PAGE) as u64;
        let mut buffers = ((6 + self.hinted as u64) * n + self.rle as u64 + 1) * large(w);
        let mut bytes = rows;
        if self.hinted {
            let (b, l) = self.hint(h, skeleton)?;
            bytes += b;
            buffers += l;
        }
        Some((bytes, buffers))
    }

    /// The memory of the hint of a hinted group, from the layers: see [`memory`].
    fn hint(&self, h: &Head, skeleton: &[u8]) -> Option<(u64, u64)> {
        use crate::psd::Compression;
        let split = h.spans[self.span].start;
        // (the file up to the composite: the skeleton in the gaps, the spans zero)
        let mut prefix = vec![0u8; split];
        let (mut prev, mut at) = (0, 0);
        for s in h.spans.iter().take_while(|s| s.start <= split) {
            let gap = prefix.get_mut(prev..s.start)?;
            gap.copy_from_slice(skeleton.get(at..at + gap.len())?);
            at += gap.len();
            prev = s.end;
            if s.start == split {
                break;
            }
        }
        let psd = crate::psd::parse(&prefix).ok()?;
        let layers = &psd.layer_info.layers;
        if psd.depth != 8 || psd.mode != 3 {
            return None;
        }
        let l = layers.len() as u64;
        let (mut dividers, mut widest, mut full, mut full_n) = (0u64, 0u64, 0u64, 0u64);
        for ly in layers {
            let section = ly
                .tag(b"lsct")
                .and_then(|t| prefix.get(t.data.start..t.data.start + 4))
                .map_or(0, |b| u32::from_be_bytes(b.try_into().unwrap()));
            dividers += (section == 3) as u64;
            let find = |id: i16| ly.channels.iter().position(|c| c.id == id);
            let (Some(r), Some(g), Some(b)) = (find(0), find(1), find(2)) else { continue };
            if ly.rect.width() == 0 || ly.rect.height() == 0 || matches!(section, 1..=3) {
                continue;
            }
            let mask = ly.mask.as_ref().filter(|m| m.flags & 2 == 0).and_then(|_| find(-2));
            for j in [Some(r), Some(g), Some(b), find(-1), mask].into_iter().flatten() {
                let c = &ly.channels[j];
                widest = widest.max(c.width);
                match c.compression(&prefix) {
                    Some(Compression::Zip | Compression::ZipPred) => return None,
                    None => {
                        full += c.width * c.height;
                        full_n += 1;
                    }
                    Some(_) => {}
                }
            }
        }
        let psd_bytes = std::mem::size_of::<crate::psd::Psd>() as u64
            + layers
                .iter()
                .map(|ly| {
                    (std::mem::size_of::<crate::psd::Layer>() + ly.name.len() + ly.channels.len() * 32) as u64
                        + 3 * (ly.tags.len() * std::mem::size_of::<crate::psd::Tag>()) as u64
                })
                .sum::<u64>()
            + 3 * (psd.resources.len() * std::mem::size_of::<crate::psd::Resource>()) as u64
            + 3 * (psd.global_tags.len() * std::mem::size_of::<crate::psd::Tag>()) as u64;
        let w = h.spans[self.span].w as u64;
        let levels = dividers + 1;
        let renderer = 5 * l + 512 * l + 3 * 16 * 3 * l + levels * (4 * w + 20) + 3 * 20 * levels + 5 * 3 * widest + w;
        Some((psd_bytes + renderer + full, full_n + levels + 8))
    }
}

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
