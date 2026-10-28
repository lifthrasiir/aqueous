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

pub const IMAGE: Module = Module { name: "fits-recons-image", version: 1 };
pub const RICE: Module = Module { name: "fits-recons-rice", version: 1 };
pub const TABLE: Module = Module { name: "fits-recons-table", version: 1 };

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
    let p = Pixels { width: usize::try_from(*axes.first()?).ok()?, bitpix: i8::try_from(h.int("BITPIX")?).ok()? };
    p.valid().then_some(p)
}

fn pixels_node(f: &mut Formula, d: &[u8], p: Pixels) -> Option<Vec<Node>> {
    let c = image::encode(&p.numbers(d), p.shape(), None);
    // (zstd at a low level stands for what the archive does with the data as it is)
    if c.model.len() + c.raw.len() >= zstd::bulk::compress(d, 3).map_or(d.len(), |z| z.len()) {
        return None;
    }
    let inputs =
        vec![f.push_as("fits/model", Kind::Compressed, c.model), f.push_as("fits/raw", Kind::Compressed, c.raw)];
    Some(vec![Node::apply(IMAGE, p.to_bytes(), inputs, d.len() as u64)])
}

fn rice(f: &mut Formula, d: &[u8], l: &Layout) -> Option<Vec<Node>> {
    let p = tiled::split(d, l)?;
    if !p.rebuilt.iter().any(|&r| r) {
        return None;
    }
    let spans = tiled::spans(l, &p.rows)?;
    let coded = tiled::pixels_kept(d, l, &spans, &p.rebuilt, &mut vec![0; p.image.len()]);
    let known: Vec<bool> = coded.iter().map(|&c| !c).collect();
    let shape = image::Shape { width: l.width(), bits: l.rice.bits(), signed: l.rice.bytepix > 1 };
    let c = image::encode(&p.image, shape, Some(&known));
    let codes = tiled::encode_codes(l, &p.rebuilt, &p.image, &p.codes);
    let inputs = vec![
        f.push_as("fits/rows", Kind::Table, p.rows),
        f.push("fits/residue", p.residue),
        f.push("fits/rebuilt", module::to_bitmap(&p.rebuilt)),
        f.push_as("fits/codes", Kind::Compressed, codes),
        f.push_as("fits/model", Kind::Compressed, c.model),
        f.push_as("fits/raw", Kind::Compressed, c.raw),
    ];
    Some(vec![Node::apply(RICE, l.to_bytes(), inputs, d.len() as u64)])
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
                IMAGE => decode_image(&a.params, inputs, a.len),
                RICE => decode_rice(&a.params, inputs, a.len),
                TABLE => decode_table(&a.params, inputs, a.len),
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
                    _ => d.extend((v as f32 * 0.37).to_be_bytes()),
                }
            }
        }
        d
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
