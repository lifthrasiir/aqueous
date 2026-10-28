//! Tile-compressed images (the FITS Standard 4.0, §10.1): a binary table of a row per tile,
//! whose `COMPRESSED_DATA` field points to the tile's stream in the heap. Of the algorithms,
//! `RICE_1` is taken ([`crate::rice`]).
//!
//! The data unit is taken apart into the table's rows as they are, the heap without the tiles
//! that are rebuilt (the residue), and the image: the pixels of the tiles, rebuilt or not, and
//! the codes of the rebuilt tiles' blocks (FORMAT.md, "fits-recons-rice").

use crate::fits::Header;
use crate::rice::{self, Params};
use crate::table;
use aqr_common::arith::{ArithDecoder, ArithEncoder, Bit};
use aqr_common::varint;

/// The layout of a tile-compressed image's data unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub rice: Params,
    /// `ZNAXISn` and `ZTILEn`, as many of each.
    pub dims: Vec<u64>,
    pub tile: Vec<u64>,
    /// Bytes of a row of the table, and rows (one per tile).
    pub row: u64,
    pub rows: u64,
    /// The offset of the `COMPRESSED_DATA` field in a row, and whether it is a `Q` descriptor.
    pub field: u64,
    pub q: bool,
    /// The heap's offset in the data unit, and the data unit's bytes.
    pub heap: u64,
    pub len: u64,
}

/// Where a tile's stream is in the data unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub len: usize,
}

impl Layout {
    /// The layout of the data unit of header `h` (of `len` bytes), where it is a
    /// tile-compressed image of `RICE_1` (or its alias `RICE_ONE`) this crate takes.
    pub fn from_header(h: &Header, len: u64) -> Option<Layout> {
        if h.str("XTENSION")?.trim() != "BINTABLE" || h.logical("ZIMAGE") != Some(true) {
            return None;
        }
        if !matches!(h.str("ZCMPTYPE")?.trim(), "RICE_1" | "RICE_ONE") {
            return None;
        }
        let dims = h.axes("ZNAXIS")?;
        if dims.is_empty() || dims.contains(&0) {
            return None;
        }
        let tile: Vec<u64> = (1..=dims.len())
            .map(|i| match h.int(&format!("ZTILE{i}")) {
                Some(t) => u64::try_from(t).ok().filter(|&t| t > 0),
                None => Some(if i == 1 { dims[0] } else { 1 }),
            })
            .collect::<Option<_>>()?;
        // the algorithm's parameters, by name, else their defaults
        let mut rice = Params { bytepix: 4, block: 32 };
        for i in 1..=99 {
            let Some(name) = h.str(&format!("ZNAME{i}")) else { break };
            let v = h.int(&format!("ZVAL{i}"))?;
            match name.trim().to_ascii_uppercase().as_str() {
                "BLOCKSIZE" => rice.block = u32::try_from(v).ok()?,
                "BYTEPIX" => rice.bytepix = u8::try_from(v).ok()?,
                _ => {}
            }
        }
        if !rice.valid() {
            return None;
        }
        let fields = table::fields(h)?;
        let f = fields.iter().find(|f| f.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case("COMPRESSED_DATA")))?;
        if !matches!(f.ty, b'P' | b'Q') || f.repeat != 1 {
            return None;
        }
        let axes = h.axes("NAXIS")?;
        let &[row, rows] = &axes[..] else { return None };
        let l = Layout {
            rice,
            heap: h.int("THEAP").map_or(Some(row * rows), |t| u64::try_from(t).ok())?,
            dims,
            tile,
            row,
            rows,
            field: f.at as u64,
            q: f.ty == b'Q',
            len,
        };
        (l.valid() && l.tiles() == Some(rows)).then_some(l)
    }

    fn valid(&self) -> bool {
        self.rice.valid()
            && !self.dims.is_empty()
            && self.dims.len() == self.tile.len()
            && self.dims.iter().chain(&self.tile).all(|&d| d > 0)
            && self.pixels().is_some_and(|n| n < 1 << 40)
            && self.tile.iter().try_fold(1u64, |a, &t| a.checked_mul(t)).is_some_and(|n| n < 1 << 32)
            && self.row.checked_mul(self.rows).is_some_and(|t| t <= self.heap && self.field + self.desc() <= self.row)
            && self.heap <= self.len
    }

    /// Pixels of the image.
    pub fn pixels(&self) -> Option<u64> {
        self.dims.iter().try_fold(1u64, |a, &d| a.checked_mul(d))
    }

    /// Tiles of the image.
    pub fn tiles(&self) -> Option<u64> {
        self.dims.iter().zip(&self.tile).try_fold(1u64, |a, (&d, &t)| a.checked_mul(d.div_ceil(t)))
    }

    fn desc(&self) -> u64 {
        if self.q { 16 } else { 8 }
    }

    /// The image's width (`ZNAXIS1`), its rows being all the others.
    pub fn width(&self) -> usize {
        self.dims[0] as usize
    }

    /// Where tile `i`'s stream is in the data unit, from the table `rows`; None where it is not
    /// in the heap.
    pub fn span(&self, rows: &[u8], i: usize) -> Option<Span> {
        let at = i * self.row as usize + self.field as usize;
        let (len, off) = if self.q {
            let d = rows.get(at..at + 16)?;
            (u64::from_be_bytes(d[..8].try_into().unwrap()), u64::from_be_bytes(d[8..].try_into().unwrap()))
        } else {
            let d = rows.get(at..at + 8)?;
            (
                u32::from_be_bytes(d[..4].try_into().unwrap()) as u64,
                u32::from_be_bytes(d[4..].try_into().unwrap()) as u64,
            )
        };
        let start = self.heap.checked_add(off)?;
        (start.checked_add(len)? <= self.len).then_some(Span { start: start as usize, len: len as usize })
    }

    /// The runs of tile `i`'s pixels in the image, in its order: (offset, pixels).
    pub fn runs(&self, i: u64, out: &mut Vec<(usize, usize)>) {
        out.clear();
        let n = self.dims.len();
        // the tile's start and size along each axis
        let mut lo = vec![0u64; n];
        let mut size = vec![0u64; n];
        let mut rest = i;
        for k in 0..n {
            let count = self.dims[k].div_ceil(self.tile[k]);
            lo[k] = rest % count * self.tile[k];
            rest /= count;
            size[k] = self.tile[k].min(self.dims[k] - lo[k]);
        }
        // every run along axis 1, the other axes in order
        let mut at = vec![0u64; n];
        loop {
            let mut off = 0u64;
            for k in (0..n).rev() {
                off = off * self.dims[k] + lo[k] + at[k];
            }
            out.push((off as usize, size[0] as usize));
            let mut k = 1;
            while k < n {
                at[k] += 1;
                if at[k] < size[k] {
                    break;
                }
                at[k] = 0;
                k += 1;
            }
            if k == n {
                break;
            }
        }
    }

    /// The layout as bytes (params of `fits-recons-rice`): varints of bytes per pixel, pixels per
    /// block, axes, then each axis's size and tile size, the row's bytes, rows, the field's
    /// offset, whether it is `Q`, the heap's offset and the data unit's bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::new();
        varint::put(&mut v, self.rice.bytepix as u64);
        varint::put(&mut v, self.rice.block as u64);
        varint::put(&mut v, self.dims.len() as u64);
        for (&d, &t) in self.dims.iter().zip(&self.tile) {
            varint::put(&mut v, d);
            varint::put(&mut v, t);
        }
        for x in [self.row, self.rows, self.field, self.q as u64, self.heap, self.len] {
            varint::put(&mut v, x);
        }
        v
    }

    /// The layout of [`Layout::to_bytes`], if valid and nothing follows; consumes them.
    pub fn from_bytes(b: &[u8], at: &mut usize) -> Option<Layout> {
        let mut num = || varint::get(b, at).ok();
        let bytepix = u8::try_from(num()?).ok()?;
        let block = u32::try_from(num()?).ok()?;
        let n = num()?;
        if !(1..=999).contains(&n) {
            return None;
        }
        let (mut dims, mut tile) = (Vec::new(), Vec::new());
        for _ in 0..n {
            dims.push(num()?);
            tile.push(num()?);
        }
        let (row, rows, field, q, heap, len) = (num()?, num()?, num()?, num()?, num()?, num()?);
        if q > 1 {
            return None;
        }
        let l = Layout { rice: Params { bytepix, block }, dims, tile, row, rows, field, q: q == 1, heap, len };
        (l.valid() && l.tiles() == Some(rows) && usize::try_from(len).is_ok()).then_some(l)
    }
}

/// A data unit taken apart (see the module).
#[derive(Debug, Default)]
pub struct Parts {
    /// The table's rows.
    pub rows: Vec<u8>,
    /// The bytes after the rows, but for the rebuilt tiles' streams.
    pub residue: Vec<u8>,
    /// Whether each tile is rebuilt.
    pub rebuilt: Vec<bool>,
    /// The image's pixels, in [`Params::bits`] bits: those of a tile not rebuilt are of its
    /// stream (zero where it is not one: [`pixels_kept`]).
    pub image: Vec<u32>,
    /// The codes of the blocks of the rebuilt tiles, in order.
    pub codes: Vec<u8>,
}

/// The tiles' spans, if they are in order of their offsets and apart; tiles of no stream
/// (zero bytes) are left out of the order.
pub fn spans(l: &Layout, rows: &[u8]) -> Option<Vec<Span>> {
    let spans: Vec<Span> = (0..l.rows as usize).map(|i| l.span(rows, i)).collect::<Option<_>>()?;
    let mut end = (l.row * l.rows) as usize;
    for s in spans.iter().filter(|s| s.len > 0) {
        if s.start < end {
            return None;
        }
        end = s.start + s.len;
    }
    Some(spans)
}

/// Data unit `d` of layout `l`, taken apart. None where the tiles are not in order in the
/// heap (or the image is too large to hold).
pub fn split(d: &[u8], l: &Layout) -> Option<Parts> {
    if d.len() as u64 != l.len {
        return None;
    }
    let table = (l.row * l.rows) as usize;
    let rows = d[..table].to_vec();
    let spans = spans(l, &rows)?;
    let mut image = vec![0u32; usize::try_from(l.pixels()?).ok()?];
    let mut p = Parts { rebuilt: vec![false; spans.len()], ..Parts::default() };
    let mut runs = Vec::new();
    let mut at = table;
    for (i, s) in spans.iter().enumerate() {
        l.runs(i as u64, &mut runs);
        let n: usize = runs.iter().map(|r| r.1).sum();
        let stream = &d[s.start..s.start + s.len];
        let tile = (s.len > 0).then(|| rice::decode(stream, n, l.rice)).flatten();
        let ok = tile.as_ref().is_some_and(|t| rice::encode(&t.pixels, &t.codes, l.rice).as_deref() == Some(stream));
        if let Some(t) = &tile {
            put_tile(&mut image, &runs, &t.pixels);
        }
        if ok {
            p.rebuilt[i] = true;
            p.codes.extend(&tile.unwrap().codes);
            p.residue.extend(&d[at..s.start]);
            at = s.start + s.len;
        }
    }
    p.residue.extend(&d[at..]);
    p.rows = rows;
    p.image = image;
    Some(p)
}

fn put_tile(image: &mut [u32], runs: &[(usize, usize)], pixels: &[u32]) {
    let mut from = 0;
    for &(off, len) in runs {
        image[off..off + len].copy_from_slice(&pixels[from..from + len]);
        from += len;
    }
}

fn get_tile(image: &[u32], runs: &[(usize, usize)], out: &mut Vec<u32>) {
    out.clear();
    for &(off, len) in runs {
        out.extend_from_slice(&image[off..off + len]);
    }
}

/// Puts into `image` (zero) the pixels of the tiles not rebuilt that are of their streams, as
/// [`split`] does, from the data unit `d` laid out with the rows and the residue (its
/// rebuilt tiles' streams not there yet). Returns which pixels are of the model: those of
/// the rebuilt tiles.
pub fn pixels_kept(d: &[u8], l: &Layout, spans: &[Span], rebuilt: &[bool], image: &mut [u32]) -> Vec<bool> {
    let mut coded = vec![false; image.len()];
    let mut runs = Vec::new();
    for (i, s) in spans.iter().enumerate() {
        l.runs(i as u64, &mut runs);
        if rebuilt[i] {
            for &(off, len) in &runs {
                coded[off..off + len].fill(true);
            }
            continue;
        }
        let n: usize = runs.iter().map(|r| r.1).sum();
        if s.len > 0
            && let Some(t) = rice::decode(&d[s.start..s.start + s.len], n, l.rice)
        {
            put_tile(image, &runs, &t.pixels);
        }
    }
    coded
}

/// The data unit of layout `l` from its rows, its residue, which tiles are rebuilt, the image
/// (all its pixels) and the codes of the rebuilt tiles' blocks; also the spans.
pub fn layout(l: &Layout, rows: &[u8], residue: &[u8], rebuilt: &[bool]) -> Option<(Vec<u8>, Vec<Span>)> {
    let table = (l.row * l.rows) as usize;
    if rows.len() != table || rebuilt.len() as u64 != l.rows {
        return None;
    }
    let spans = spans(l, rows)?;
    let mut d = Vec::with_capacity(l.len as usize);
    d.extend_from_slice(rows);
    let mut from = 0;
    for (s, &r) in spans.iter().zip(rebuilt) {
        if !r {
            continue;
        }
        let gap = s.start - d.len();
        d.extend_from_slice(residue.get(from..from + gap)?);
        from += gap;
        d.resize(s.start + s.len, 0);
    }
    d.extend_from_slice(residue.get(from..)?);
    (d.len() as u64 == l.len).then_some((d, spans))
}

/// Writes into `d` (of [`layout`]) the streams of the rebuilt tiles from `image` and `codes`.
pub fn rebuild(d: &mut [u8], l: &Layout, spans: &[Span], rebuilt: &[bool], image: &[u32], codes: &[u8]) -> Option<()> {
    let (mut runs, mut px) = (Vec::new(), Vec::new());
    let mut c = 0;
    for (i, s) in spans.iter().enumerate() {
        if !rebuilt[i] {
            continue;
        }
        l.runs(i as u64, &mut runs);
        get_tile(image, &runs, &mut px);
        let nb = l.rice.blocks(px.len());
        let stream = rice::encode(&px, codes.get(c..c + nb)?, l.rice)?;
        c += nb;
        if stream.len() != s.len {
            return None;
        }
        d[s.start..s.start + s.len].copy_from_slice(&stream);
    }
    (c == codes.len()).then_some(())
}

/// Calls `f` with each block of the rebuilt tiles: its differences, in order.
fn blocks(l: &Layout, rebuilt: &[bool], image: &[u32], mut f: impl FnMut(&[u32]) -> Option<()>) -> Option<()> {
    let (mut runs, mut px, mut z) = (Vec::new(), Vec::new(), Vec::new());
    for (i, _) in rebuilt.iter().enumerate().filter(|r| *r.1) {
        l.runs(i as u64, &mut runs);
        get_tile(image, &runs, &mut px);
        rice::differences(&px, l.rice, &mut z);
        for b in z.chunks(l.rice.block as usize) {
            f(b)?;
        }
    }
    Some(())
}

/// Codes, as 5 bits of a tree per context.
const CODE_TREE: usize = 32;

/// The codes of the rebuilt tiles' blocks coded, each in the context of the code that codes
/// its block the shortest ([`rice::shortest`]). The image is known when they are decoded.
pub fn encode_codes(l: &Layout, rebuilt: &[bool], image: &[u32], codes: &[u8]) -> Vec<u8> {
    let mut ac = ArithEncoder::new(Vec::new());
    let mut m = vec![[Bit::default(); CODE_TREE]; CODE_TREE];
    let mut c = codes.iter();
    blocks(l, rebuilt, image, |b| {
        let code = *c.next()? as usize;
        let tree = &mut m[rice::shortest(b, l.rice) as usize];
        let mut node = 1;
        for i in (0..5).rev() {
            let bit = (code >> i) & 1 != 0;
            ac.encode(bit, tree[node].p1());
            tree[node].update(bit);
            node = node * 2 + bit as usize;
        }
        Some(())
    });
    ac.finish()
}

/// The codes of [`encode_codes`].
pub fn decode_codes(l: &Layout, rebuilt: &[bool], image: &[u32], coded: &[u8]) -> Option<Vec<u8>> {
    let mut ac = ArithDecoder::new(coded);
    let mut m = vec![[Bit::default(); CODE_TREE]; CODE_TREE];
    let mut codes = Vec::new();
    blocks(l, rebuilt, image, |b| {
        let tree = &mut m[rice::shortest(b, l.rice) as usize];
        let mut node = 1;
        for _ in 0..5 {
            let bit = ac.decode(tree[node].p1());
            tree[node].update(bit);
            node = node * 2 + bit as usize;
        }
        codes.push((node - CODE_TREE) as u8);
        Some(())
    })?;
    (!ac.overrun()).then_some(codes)
}
