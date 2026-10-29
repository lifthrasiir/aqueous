//! aqr-fits as modules of entry formulas (see aqr-formula): a FITS file is its HDUs one after
//! another, each its header (a slice of the headers' stream, text), its data and its padding.
//! The data of an integer or float image is `fits-recons-image`'s, of a tile-compressed image
//! of Rice tiles `fits-recons-rice`'s, the rows of a binary table `fits-recons-table`'s (its
//! heap as it is), and any other data as it is (FORMAT.md).

use aqr_formula::{Formula, Kind, Module, Node};

use crate::fits::{self, Hdu, Header};
use crate::image;
use crate::module::{self, Pixels};
use crate::table;
use crate::tiled::{self, Layout};

pub use crate::module::{image as decode_image, rice as decode_rice, table as decode_table};

pub const IMAGE: Module = Module::new("fits-recons-image", 1).with_memory(image_memory);
pub const RICE: Module = Module::new("fits-recons-rice", 1).with_memory(rice_memory);
pub const TABLE: Module = Module::new("fits-recons-table", 1).with_memory(table_memory);

/// The memory of `fits-recons-table`: the planes and the rows, and the columns (a varint and a
/// byte of the params at least each, 12 bytes on 32 bits, their vector doubling).
fn table_memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    Some(u.whole(3 * 12 * u.params.len() as u64 / 2, 1))
}

/// Bytes of the prediction model's tables ([`image`]'s `Model`, a `Bit` 4 bytes, a bias entry 16
/// on 32 bits): 64 × 48 + 396 × 2 bits and 729 entries; its two rows of residuals' lengths are
/// counted apart.
const PREDICT_MODEL: u64 = 4 * (64 * 48 + 12 * 33 * 2) + 16 * 729;

/// Bytes of the pyramid's models (`pyramid::Models`): (216 + 24 + 24) × 64 + 26 × 24 × 1024 bits
/// of 4 bytes.
const PYRAMID_MODELS: u64 = 4 * (264 * 64 + 26 * 24 * 1024);

/// What the pyramid decoder (`pyramid::decode`) holds at most besides its models, of an image
/// of `n` pixels, `w` wide: the values of the first level (8 bytes each), its bands (three
/// quarters of that and the next level's values, a quarter), the bands of the next level kept
/// as the parent (a quarter in all), and the list of border values (at most `2 (w + h)`, 16
/// bytes each on 32 bits, the vector grown by pushes, its old blocks held: three times). Each
/// level's are allocated anew, the coarser ones freed in blocks too small for the finer: all of
/// them at most, a third more than the first level's.
fn pyramid_state(n: u64, w: u64) -> u64 {
    let border = n.min(2 * (w + n.div_ceil(w)));
    4 * 8 * (n + n + n / 4) / 3 + 48 * border
}

/// The memory of `fits-recons-image`, from the pixels `n` (the output over a pixel's bytes): the
/// numbers (4 bytes each) and the copies of the model and the raw bits (`Coded`); then by the
/// mode, the prediction model with the rows of residuals' lengths (a byte each, two rows of
/// the width plus 2), or the pyramid's state (`pyramid_state`) and models. The output is made
/// from the numbers, after.
fn image_memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let p = Pixels::from_bytes(u.params)?;
    let [model, raw] = u.inputs[..] else { return None };
    let n = u.output() / p.bytes() as u64;
    let w = p.width as u64;
    let state = match p.mode {
        image::Mode::Predict => PREDICT_MODEL + 2 * (w + 2),
        image::Mode::Pyramid => PYRAMID_MODELS + pyramid_state(n, w),
    };
    Some(u.whole(4 * n + model + raw + state, 16))
}

/// The memory of `fits-recons-rice`, from the layout: with `P` pixels, `R` tiles, `T` pixels
/// in the largest tile and `U` runs in it, `K` codes at most (a block of `block` pixels each and
/// one more per tile) and `S` bytes in the longest stream (told by the rows). Throughout, the
/// bitmap of the rebuilt tiles (`R`) and the spans (8 bytes a tile, collected without a size
/// hint: three times); the image (4 `P`), which pixels are coded (`P`), the copies of the
/// model and the raw bits (`Coded`); then by phase, the largest of
/// - tiles not rebuilt decoded into the image: the runs (8 bytes each, grown by pushes: three
///   times) and the tile (4 `T` pixels and `T` codes at most);
/// - the pixels decoded: the pixels not known (`P`, with prediction), and by the mode the
///   prediction model with the rows of lengths, or the pyramid's (`pyramid_state`) with its
///   models;
/// - the codes decoded: the codes (three times `K`), the runs, and the block's pixels and
///   differences (4 bytes each, grown by extensions: three times a vector of a tile);
/// - the tiles' streams written: the same codes and runs, the pixels (three times 4 `T`), the
///   differences (4 `T`) and the stream written by pushes (three times `S`).
fn rice_memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let mut at = 0;
    let l = Layout::from_bytes(u.params, &mut at)?;
    let &[mode] = &u.params[at..] else { return None };
    let mode = image::Mode::from_byte(mode)?;
    let &[rows, _, _, _, model, raw] = &u.inputs[..] else { return None };
    let table = u.contents.first()?;
    if table.len() as u64 != rows {
        return None;
    }
    let (p, r) = (l.pixels()?, l.rows);
    let w = l.width() as u64;
    let size = |k: usize| l.tile[k].min(l.dims[k]);
    let t: u64 = (0..l.dims.len()).map(size).product();
    let runs: u64 = (1..l.dims.len()).map(size).product();
    let k = p / l.rice.block as u64 + r;
    let s = (0..r as usize).map(|i| l.span(table, i).map_or(0, |s| s.len as u64)).max().unwrap_or(0);
    let base = r + 3 * 8 * r + 4 * p + p + model + raw;
    let tiles = 3 * 8 * runs;
    let decoding = match mode {
        image::Mode::Predict => p + PREDICT_MODEL + 2 * (w + 2),
        image::Mode::Pyramid => PYRAMID_MODELS + pyramid_state(p, w),
    };
    let peak =
        [tiles + 5 * t, decoding, p + 3 * k + tiles + 2 * 3 * 4 * t, p + 3 * k + tiles + 3 * 4 * t + 4 * t + 3 * s];
    Some(u.whole(base + peak.into_iter().max()?, 24))
}

/// Data smaller than this is kept as it is.
const MIN_DATA: usize = 2048;

/// Whether `d` starts like a FITS file.
pub fn starts(d: &[u8]) -> bool {
    fits::starts(d)
}

/// The end of the FITS file at the start of `d`: of its last whole HDU.
pub fn extent(d: &[u8]) -> Option<usize> {
    fits::parse(d).map(|(_, end)| end)
}

/// The streams the HDUs' parts go to, by their index in the formula.
struct Streams {
    header: usize,
    padding: usize,
    raw: usize,
    text: usize,
}

impl Streams {
    fn slice(f: &mut Formula, stream: usize, d: &[u8]) -> Option<Node> {
        if d.is_empty() {
            return None;
        }
        let s = &mut f.streams[stream].data;
        let start = s.len() as u64;
        s.extend_from_slice(d);
        Some(Node::slice(stream, start, d.len() as u64))
    }
}

/// The formula of FITS file `input`, or None where it does not start with a primary header.
/// It is the same whatever `threads` is (one is used).
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    let (hdus, end) = fits::parse(input)?;
    let mut f = Formula::default();
    let s = Streams {
        header: index(f.push_as("fits/header", Kind::Text, Vec::new())),
        padding: index(f.push("fits/padding", Vec::new())),
        raw: index(f.push("fits/data", Vec::new())),
        text: index(f.push_as("fits/text", Kind::Text, Vec::new())),
    };
    let mut parts = Vec::new();
    for h in &hdus {
        parts.extend(Streams::slice(&mut f, s.header, &input[h.header.clone()]));
        parts.extend(data(&mut f, &s, input, h));
        parts.extend(Streams::slice(&mut f, s.padding, &input[h.data.end..h.end]));
    }
    parts.extend(Streams::slice(&mut f, s.raw, &input[end..]));
    f.root = Node::concat(parts);
    Some(f)
}

fn index(n: Node) -> usize {
    match n {
        Node::Slice { stream, .. } => stream,
        _ => unreachable!(),
    }
}

/// The nodes of HDU `h`'s data.
fn data(f: &mut Formula, s: &Streams, input: &[u8], h: &Hdu) -> Vec<Node> {
    let d = &input[h.data.clone()];
    let hd = Header::new(&input[h.header.clone()]);
    if d.len() >= MIN_DATA {
        let coded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(l) = Layout::from_header(&hd, d.len() as u64) {
                rice(f, d, &l)
            } else if let Some(p) = pixels(&hd) {
                pixels_node(f, d, p)
            } else if xtension(&hd).as_deref() == Some("BINTABLE") {
                bintable(f, s, d, &hd)
            } else {
                None
            }
        }));
        if let Ok(Some(n)) = coded {
            return n;
        }
    }
    let stream = if xtension(&hd).as_deref() == Some("TABLE") { s.text } else { s.raw };
    Streams::slice(f, stream, d).into_iter().collect()
}

fn xtension(h: &Header) -> Option<String> {
    h.str("XTENSION").map(|x| x.trim().to_string())
}

/// The pixels of a primary or `IMAGE` HDU's data, if `fits-image` takes them.
fn pixels(h: &Header) -> Option<Pixels> {
    if xtension(h).is_some_and(|x| x != "IMAGE") || h.logical("GROUPS") == Some(true) {
        return None;
    }
    if h.int("PCOUNT").unwrap_or(0) != 0 || h.int("GCOUNT").unwrap_or(1) != 1 {
        return None;
    }
    let axes = h.axes("NAXIS")?;
    let width = usize::try_from(*axes.first()?).ok()?;
    let p = Pixels { width, bitpix: i8::try_from(h.int("BITPIX")?).ok()?, mode: image::Mode::Predict, shift: 0 };
    p.valid().then_some(p)
}

fn pixels_node(f: &mut Formula, d: &[u8], mut p: Pixels) -> Option<Vec<Node>> {
    let pixels = p.pixels(d);
    p.shift = p.zero_bits(&pixels);
    let c;
    (p.mode, c) = image::encode_best(&p.numbers(&pixels), p.shape(), None);
    let coded = c.model.len() + c.raw.len();
    // (zstd at a low level stands for what the archive does with the data as it is)
    if !module::within(d.len() as u64, coded) || coded >= zstd::bulk::compress(d, 3).map_or(d.len(), |z| z.len()) {
        return None;
    }
    let inputs =
        vec![f.push_as("fits/model", Kind::Compressed, c.model), f.push_as("fits/raw", Kind::Compressed, c.raw)];
    Some(vec![Node::apply(IMAGE, p.to_bytes(), inputs, d.len() as u64)])
}

fn rice(f: &mut Formula, d: &[u8], l: &Layout) -> Option<Vec<Node>> {
    // (Rice codes a constant block of 32 pixels in 5 bits: a header claiming far more pixels
    // than that is not taken, nor its image held)
    if l.pixels()? > 64 * d.len() as u64 + (1 << 20) {
        return None;
    }
    let p = tiled::split(d, l)?;
    if !p.rebuilt.iter().any(|&r| r) {
        return None;
    }
    let spans = tiled::spans(l, &p.rows)?;
    let coded = tiled::pixels_kept(d, l, &spans, &p.rebuilt, &mut vec![0; p.image.len()]);
    let known = (!coded.iter().all(|&c| c)).then(|| coded.iter().map(|&c| !c).collect::<Vec<_>>());
    let shape = image::Shape { width: l.width(), bits: l.rice.bits(), signed: l.rice.bytepix > 1 };
    let (mode, c) = image::encode_best(&p.image, shape, known.as_deref());
    if !module::within(p.image.len() as u64, c.model.len() + c.raw.len()) {
        return None;
    }
    let codes = tiled::encode_codes(l, &p.rebuilt, &p.image, &p.codes);
    let inputs = vec![
        f.push_as("fits/rows", Kind::Table, p.rows),
        f.push("fits/residue", p.residue),
        f.push("fits/rebuilt", module::to_bitmap(&p.rebuilt)),
        f.push_as("fits/codes", Kind::Compressed, codes),
        f.push_as("fits/model", Kind::Compressed, c.model),
        f.push_as("fits/raw", Kind::Compressed, c.raw),
    ];
    let mut params = l.to_bytes();
    params.push(mode as u8);
    Some(vec![Node::apply(RICE, params, inputs, d.len() as u64)])
}

/// A binary table's rows through `fits-table`, its heap as it is.
fn bintable(f: &mut Formula, s: &Streams, d: &[u8], h: &Header) -> Option<Vec<Node>> {
    let axes = h.axes("NAXIS")?;
    let &[row, n] = &axes[..] else { return None };
    let (row, n) = (usize::try_from(row).ok()?, usize::try_from(n).ok()?);
    let len = row.checked_mul(n)?;
    if n < 2 || len < MIN_DATA || len > d.len() {
        return None;
    }
    let fields = table::fields(h)?;
    let rows = &d[..len];
    let cols = table::columns(rows, row, &fields);
    let planes = table::split(rows, row, &cols);
    let mut nodes = vec![Node::apply(
        TABLE,
        module::table_params(n, &cols),
        vec![f.push_as("fits/planes", Kind::Table, planes)],
        len as u64,
    )];
    nodes.extend(Streams::slice(f, s.raw, &d[len..]));
    Some(nodes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aqr_formula::Apply;

    fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
        f.eval(&|a: &Apply, inputs: &[&[u8]]| {
            let r = match a.module {
                m if m == IMAGE => decode_image(&a.params, inputs, a.len),
                m if m == RICE => decode_rice(&a.params, inputs, a.len),
                m if m == TABLE => decode_table(&a.params, inputs, a.len),
                m => panic!("{m}"),
            };
            r.map_err(|e| aqr_formula::Error::new(e.0))
        })
    }

    fn card(s: &str) -> Vec<u8> {
        let mut c = s.as_bytes().to_vec();
        c.resize(fits::CARD, b' ');
        c
    }

    fn header(cards: &[String]) -> Vec<u8> {
        let mut h: Vec<u8> = cards.iter().flat_map(|c| card(c)).collect();
        h.extend(card("END"));
        h.resize(h.len().next_multiple_of(fits::BLOCK), b' ');
        h
    }

    fn kv(k: &str, v: impl std::fmt::Display) -> String {
        format!("{k:<8}= {v:>20}")
    }

    fn pad(d: &mut Vec<u8>) {
        d.resize(d.len().next_multiple_of(fits::BLOCK), 0);
    }

    /// A smooth image with noise, of `bitpix`.
    fn image_data(w: usize, h: usize, bitpix: i32) -> Vec<u8> {
        let mut rng = aqr_common::rng::Rng::new(3);
        let mut d = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let v = (x * 7 + y * 3) as f64 + rng.below(20) as f64;
                match bitpix {
                    8 => d.push(v as u8),
                    16 => d.extend((v as i16 - 300).to_be_bytes()),
                    32 => d.extend((v as i32 * 1000).to_be_bytes()),
                    // (of 9 bits of mantissa, as SDSS's frames)
                    _ => d.extend(((v as f32 * 0.37).to_bits() & !0x3fff).to_be_bytes()),
                }
            }
        }
        d
    }

    /// An image of a plane, which codes to almost nothing, still evaluates (it is left to the
    /// archive where the modules would take it for corrupt).
    #[test]
    fn plane_image() {
        let (w, h) = (1024, 1024);
        let mut d = header(&[kv("SIMPLE", "T"), kv("BITPIX", 16), kv("NAXIS", 2), kv("NAXIS1", w), kv("NAXIS2", h)]);
        for y in 0..h {
            for x in 0..w {
                d.extend(((x * 3 + y * 5) as i16).to_be_bytes());
            }
        }
        pad(&mut d);
        let f = encode(&d, 1).unwrap();
        assert!(eval(&f).unwrap() == d);
    }

    #[test]
    fn images_and_tables() {
        let (w, h) = (300, 40);
        let mut d = Vec::new();
        for (i, bitpix) in [16, 8, 32, -32].into_iter().enumerate() {
            let first = if i == 0 { kv("SIMPLE", "T") } else { kv("XTENSION", "'IMAGE   '") };
            d.extend(header(&[first, kv("BITPIX", bitpix), kv("NAXIS", 2), kv("NAXIS1", w), kv("NAXIS2", h)]));
            d.extend(image_data(w, h, bitpix));
            pad(&mut d);
        }
        // a binary table of a counter, a float and a string, with a heap
        let n = 500i32;
        d.extend(header(&[
            kv("XTENSION", "'BINTABLE'"),
            kv("BITPIX", 8),
            kv("NAXIS", 2),
            kv("NAXIS1", 4 + 8 + 3),
            kv("NAXIS2", n),
            kv("PCOUNT", 10),
            kv("GCOUNT", 1),
            kv("TFIELDS", 3),
            kv("TFORM1", "'J       '"),
            kv("TFORM2", "'D       '"),
            kv("TFORM3", "'3A      '"),
        ]));
        for r in 0..n {
            d.extend((r * 3).to_be_bytes());
            d.extend((r as f64 * 0.25).to_be_bytes());
            d.extend(b"abc");
        }
        d.extend(b"0123456789");
        pad(&mut d);
        d.extend(b"trailing");
        let f = encode(&d, 1).unwrap();
        let applied = f.applies().iter().map(|a| a.module).collect::<Vec<_>>();
        assert!(applied.contains(&IMAGE) && applied.contains(&TABLE), "{applied:?}");
        assert!(eval(&f).unwrap() == d);
    }
}
