//! PSD/PSB structure: where each part of the file lies, without interpreting more than needed.
//!
//! All spans are absolute byte ranges into the file. Bytes that are not inside any span of
//! the structure are padding or junk the parser skipped.

use std::fmt;
use std::ops::Range;

pub type Span = Range<usize>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn err<T>(s: impl Into<String>) -> Result<T> {
    Err(Error(s.into()))
}

/// Big-endian reader over a window of the file, with absolute positions.
pub(crate) struct Rd<'a> {
    pub d: &'a [u8],
    /// Start of the window; padding is relative to it.
    pub base: usize,
    pub p: usize,
    pub end: usize,
}

impl<'a> Rd<'a> {
    pub fn new(d: &'a [u8]) -> Self {
        Rd { d, base: 0, p: 0, end: d.len() }
    }
    pub fn left(&self) -> usize {
        self.end - self.p
    }
    pub fn skip(&mut self, n: usize) -> Result<Span> {
        if n > self.left() {
            return err(format!("truncated at {} (+{n})", self.p));
        }
        self.p += n;
        Ok(self.p - n..self.p)
    }
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self.skip(n)?;
        Ok(&self.d[s])
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.bytes(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    pub fn i32(&mut self) -> Result<i32> {
        Ok(self.u32()? as i32)
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.bytes(8)?.try_into().unwrap()))
    }
    pub fn key(&mut self) -> Result<[u8; 4]> {
        Ok(self.bytes(4)?.try_into().unwrap())
    }
    /// u32 in PSD, u64 in PSB.
    pub fn len(&mut self, psb: bool) -> Result<usize> {
        let n = if psb { self.u64()? } else { self.u32()? as u64 };
        usize::try_from(n).or_else(|_| err("length overflow"))
    }
    /// A reader over the next `n` bytes; this one moves past them.
    pub fn sub(&mut self, n: usize) -> Result<Rd<'a>> {
        let s = self.skip(n)?;
        Ok(Rd { d: self.d, base: s.start, p: s.start, end: s.end })
    }
    pub fn rect(&mut self) -> Result<Rect> {
        Ok(Rect { top: self.i32()?, left: self.i32()?, bottom: self.i32()?, right: self.i32()? })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rect {
    pub top: i32,
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
}

impl Rect {
    pub fn width(&self) -> u64 {
        (self.right as i64 - self.left as i64).max(0) as u64
    }
    pub fn height(&self) -> u64 {
        (self.bottom as i64 - self.top as i64).max(0) as u64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Raw,
    Rle,
    Zip,
    ZipPred,
    Other(u16),
}

impl Compression {
    pub fn from_u16(c: u16) -> Self {
        match c {
            0 => Compression::Raw,
            1 => Compression::Rle,
            2 => Compression::Zip,
            3 => Compression::ZipPred,
            c => Compression::Other(c),
        }
    }
    pub fn name(&self) -> String {
        match self {
            Compression::Raw => "raw".into(),
            Compression::Rle => "rle".into(),
            Compression::Zip => "zip".into(),
            Compression::ZipPred => "zippred".into(),
            Compression::Other(c) => format!("comp{c}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Resource {
    pub sig: [u8; 4],
    pub id: u16,
    /// The whole block, header and padding included.
    pub span: Span,
    pub data: Span,
}

/// Tagged block ("additional layer information").
#[derive(Debug, Clone)]
pub struct Tag {
    pub sig: [u8; 4],
    pub key: [u8; 4],
    /// Header and data, without padding.
    pub span: Span,
    pub data: Span,
}

#[derive(Debug, Clone)]
pub struct Channel {
    pub id: i16,
    /// Size of the channel's pixel rectangle.
    pub width: u64,
    pub height: u64,
    /// Compression field and data.
    pub span: Span,
}

impl Channel {
    pub fn compression(&self, d: &[u8]) -> Option<Compression> {
        (self.span.len() >= 2)
            .then(|| Compression::from_u16(u16::from_be_bytes([d[self.span.start], d[self.span.start + 1]])))
    }
}

#[derive(Debug, Clone, Default)]
pub struct Mask {
    pub rect: Rect,
    pub default_color: u8,
    pub flags: u8,
    /// Rectangle of the real user mask (channel -3), when present.
    pub real_rect: Option<Rect>,
}

#[derive(Debug, Clone)]
pub struct Layer {
    pub rect: Rect,
    pub blend: [u8; 4],
    pub opacity: u8,
    pub clipping: u8,
    pub flags: u8,
    pub name: Vec<u8>,
    pub mask: Option<Mask>,
    pub channels: Vec<Channel>,
    pub tags: Vec<Tag>,
    /// The layer record, its tags included.
    pub record: Span,
}

impl Layer {
    pub fn hidden(&self) -> bool {
        self.flags & 2 != 0
    }
    pub fn tag(&self, key: &[u8; 4]) -> Option<&Tag> {
        self.tags.iter().find(|t| &t.key == key)
    }
}

#[derive(Debug, Clone, Default)]
pub struct LayerInfo {
    /// Negative layer count in the file: the first alpha channel is the composite's
    /// transparency.
    pub merged_alpha: bool,
    pub layers: Vec<Layer>,
    /// Count, records and channel data.
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Psd {
    pub psb: bool,
    pub channels: u16,
    pub height: u64,
    pub width: u64,
    pub depth: u16,
    pub mode: u16,
    pub color_mode: Span,
    pub resources: Vec<Resource>,
    pub resource_section: Span,
    pub layer_section: Span,
    /// Layers, from the layer info section or from a `Layr`/`Lr16`/`Lr32` global tag.
    pub layer_info: LayerInfo,
    pub global_mask: Option<Span>,
    pub global_tags: Vec<Tag>,
    /// Compression field and data of the composite image, to the end of the file.
    pub composite: Span,
}

impl Psd {
    /// Bytes per row of a plane `w` pixels wide.
    pub fn row_bytes(&self, w: u64) -> u64 {
        (w * self.depth as u64).div_ceil(8)
    }
    pub fn composite_compression(&self, d: &[u8]) -> Compression {
        let s = self.composite.start;
        Compression::from_u16(u16::from_be_bytes([d[s], d[s + 1]]))
    }
}

/// Keys whose length is 64-bit in PSB.
pub fn long_key(k: &[u8; 4]) -> bool {
    matches!(
        k,
        b"LMsk" | b"Lr16" | b"Lr32" | b"Layr" | b"Mt16" | b"Mt32" | b"Mtrn" | b"Alph" | b"FMsk"
            | b"lnk2" | b"FEid" | b"FXid" | b"PxSD"
    )
}

fn is_sig(d: &[u8]) -> bool {
    d.len() >= 4 && (&d[..4] == b"8BIM" || &d[..4] == b"8B64")
}

/// Tagged blocks until the reader is exhausted. Padding between blocks is inconsistent among
/// writers, so the next block is looked for at the unpadded, 2-aligned and 4-aligned
/// positions (relative to the enclosing section); trailing bytes that start no block are left
/// alone.
fn tags(rd: &mut Rd, psb: bool) -> Result<Vec<Tag>> {
    let mut out = Vec::new();
    loop {
        let Some(q) = [1, 2, 4]
            .iter()
            .map(|&a| (rd.p - rd.base).next_multiple_of(a) + rd.base)
            .find(|&q| q < rd.end && is_sig(&rd.d[q..rd.end]))
        else {
            return Ok(out);
        };
        rd.p = q;
        let sig = rd.key()?;
        let key = rd.key()?;
        let n = if psb && long_key(&key) { rd.len(true)? } else { rd.u32()? as usize };
        let data = rd.skip(n)?;
        out.push(Tag { sig, key, span: q..data.end, data });
    }
}

fn layer_info(rd: &mut Rd, psb: bool) -> Result<LayerInfo> {
    let start = rd.p;
    if rd.left() == 0 {
        return Ok(LayerInfo { span: start..start, ..Default::default() });
    }
    let count = rd.u16()? as i16;
    let mut layers = Vec::with_capacity(count.unsigned_abs() as usize);
    let mut lens = Vec::new();
    for _ in 0..count.unsigned_abs() {
        let rs = rd.p;
        let rect = rd.rect()?;
        let nc = rd.u16()? as usize;
        let mut channels = Vec::with_capacity(nc);
        for _ in 0..nc {
            let id = rd.u16()? as i16;
            lens.push(rd.len(psb)?);
            channels.push(Channel { id, width: rect.width(), height: rect.height(), span: 0..0 });
        }
        if rd.bytes(4)? != b"8BIM" {
            return err(format!("bad blend signature at {}", rd.p - 4));
        }
        let blend = rd.key()?;
        let opacity = rd.u8()?;
        let clipping = rd.u8()?;
        let flags = rd.u8()?;
        rd.u8()?;
        let el = rd.u32()? as usize;
        let mut ex = rd.sub(el)?;
        let ml = ex.u32()? as usize;
        let mut m = ex.sub(ml)?;
        let mask = if ml >= 18 {
            let rect = m.rect()?;
            let default_color = m.u8()?;
            let flags = m.u8()?;
            // The real user mask header comes first, and only with a -3 channel; the
            // specification puts the mask parameters (flags bit 4) first, but writers don't.
            let mut real_rect = None;
            if ml >= 36 && channels.iter().any(|c| c.id == -3) {
                m.skip(2)?;
                real_rect = Some(m.rect()?);
            }
            Some(Mask { rect, default_color, flags, real_rect })
        } else {
            None
        };
        if let Some(mk) = &mask {
            for c in &mut channels {
                let r = match (c.id, mk.real_rect) {
                    (-2, _) => mk.rect,
                    (-3, Some(r)) => r,
                    _ => continue,
                };
                (c.width, c.height) = (r.width(), r.height());
            }
        }
        let bl = ex.u32()? as usize;
        ex.skip(bl)?;
        let nl = ex.u8()? as usize;
        let name = ex.bytes(nl)?.to_vec();
        let np = (1 + nl).next_multiple_of(4) - 1 - nl;
        ex.skip(np.min(ex.left()))?;
        let tags = tags(&mut ex, psb)?;
        layers.push(Layer {
            rect,
            blend,
            opacity,
            clipping,
            flags,
            name,
            mask,
            channels,
            tags,
            record: rs..rd.p,
        });
    }
    let mut lens = lens.into_iter();
    for l in &mut layers {
        for c in &mut l.channels {
            c.span = rd.skip(lens.next().unwrap())?;
        }
    }
    Ok(LayerInfo { merged_alpha: count < 0, layers, span: start..rd.p })
}

pub fn parse(d: &[u8]) -> Result<Psd> {
    let mut rd = Rd::new(d);
    if rd.bytes(4)? != b"8BPS" {
        return err("not 8BPS");
    }
    let ver = rd.u16()?;
    if ver != 1 && ver != 2 {
        return err(format!("version {ver}"));
    }
    let psb = ver == 2;
    rd.skip(6)?;
    let channels = rd.u16()?;
    let height = rd.u32()? as u64;
    let width = rd.u32()? as u64;
    let depth = rd.u16()?;
    let mode = rd.u16()?;

    let n = rd.u32()? as usize;
    let color_mode = rd.skip(n)?;

    let n = rd.u32()? as usize;
    let rs_start = rd.p - 4;
    let mut res = rd.sub(n)?;
    let mut resources = Vec::new();
    while res.left() >= 12 {
        let s = res.p;
        let sig = res.key()?;
        let id = res.u16()?;
        let nl = res.u8()? as usize;
        res.skip(nl + (nl + 1) % 2)?;
        let dl = res.u32()? as usize;
        // Some writers get the length slightly wrong; trust the next signature within 2 bytes.
        let next = res.p + dl + dl % 2;
        let end = [0isize, -1, -2, 1, 2]
            .iter()
            .map(|&o| next.wrapping_add_signed(o))
            .find(|&q| q >= res.p && (q == res.end || res.d.get(q..q + 4) == Some(b"8BIM")))
            .unwrap_or(next);
        let data = res.p..(res.p + dl).min(end);
        res.skip(end - res.p)?;
        resources.push(Resource { sig, id, span: s..end, data });
    }
    let resource_section = rs_start..rd.p;

    let ls_start = rd.p;
    let n = rd.len(psb)?;
    let mut lm = rd.sub(n)?;
    let mut info = LayerInfo { span: lm.p..lm.p, ..Default::default() };
    let mut global_mask = None;
    let mut global_tags = Vec::new();
    if n > 0 {
        let ln = lm.len(psb)?;
        let mut li = lm.sub(ln)?;
        info = layer_info(&mut li, psb)?;
        if lm.left() >= 4 {
            let s = lm.p;
            let gl = lm.u32()? as usize;
            lm.skip(gl.min(lm.left()))?;
            global_mask = Some(s..lm.p);
        }
        global_tags = tags(&mut lm, psb)?;
        if info.layers.is_empty()
            && let Some(t) = global_tags.iter().find(|t| matches!(&t.key, b"Layr" | b"Lr16" | b"Lr32"))
        {
            let mut r = Rd { d, base: t.data.start, p: t.data.start, end: t.data.end };
            info = layer_info(&mut r, psb)?;
        }
    }
    let layer_section = ls_start..rd.p;
    if rd.left() < 2 {
        return err("no composite image");
    }
    Ok(Psd {
        psb,
        channels,
        height,
        width,
        depth,
        mode,
        color_mode,
        resources,
        resource_section,
        layer_section,
        layer_info: info,
        global_mask,
        global_tags,
        composite: rd.p..d.len(),
    })
}

/// A file embedded in a smart object (`lnk2`, `lnkD` or `lnk3` block).
#[derive(Debug, Clone)]
pub struct Linked {
    pub file_type: [u8; 4],
    pub data: Span,
}

/// Leading bytes of the embedded file types whose data can be found.
fn file_magic(ft: &[u8; 4]) -> Option<&'static [u8]> {
    match ft {
        b"8BPB" | b"8BPS" => Some(b"8BPS"),
        b"JPEG" => Some(&[0xff, 0xd8, 0xff]),
        b"png " | b"PNGf" => Some(&[0x89, b'P', b'N', b'G']),
        _ => None,
    }
}

/// Embedded files of the smart objects, with known file types, in file order. The data is
/// found by its leading bytes after the entry's header (past an optional descriptor that is
/// not parsed).
pub fn linked(psd: &Psd, d: &[u8]) -> Vec<Linked> {
    let mut out = Vec::new();
    for t in psd.global_tags.iter().filter(|t| matches!(&t.key, b"lnk2" | b"lnkD" | b"lnk3")) {
        let mut p = t.data.start;
        while t.data.end - p >= 8 {
            let n = u64::from_be_bytes(d[p..p + 8].try_into().unwrap());
            let Some(end) = usize::try_from(n).ok().and_then(|n| (p + 8).checked_add(n)).filter(|&e| e <= t.data.end) else {
                break;
            };
            let mut rd = Rd { d, base: p + 8, p: p + 8, end };
            let head = (|| -> Result<([u8; 4], usize)> {
                rd.skip(8)?; // kind, version
                let idl = rd.u8()? as usize;
                rd.skip(idl)?;
                let nl = rd.u32()? as usize;
                rd.skip(nl.checked_mul(2).ok_or(Error("name".into()))?)?;
                let ft = rd.key()?;
                rd.skip(4)?;
                let dl = rd.u64()?;
                Ok((ft, usize::try_from(dl).map_err(|_| Error("length".into()))?))
            })();
            if let Ok((ft, dl)) = head
                && let Some(magic) = file_magic(&ft)
            {
                let from = rd.p;
                let found = d[from..end]
                    .windows(magic.len())
                    .position(|w| w == magic)
                    .map(|k| from + k)
                    .filter(|&s| s + dl <= end && dl > 0);
                if let Some(s) = found {
                    out.push(Linked { file_type: ft, data: s..s + dl });
                }
            }
            p = (end - t.data.start).next_multiple_of(4) + t.data.start;
        }
    }
    out
}
