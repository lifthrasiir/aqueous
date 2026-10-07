//! Byte breakdown of PSD/PSB files by section, to see where the bytes go.
//!
//! `psdstat [-v] FILE...` prints, over all files, the bytes of each category and (for pixel
//! data) the decoded size. `-v` adds one line per file. Bytes not covered by any parsed
//! structure are counted as `unparsed`, so every file sums to its size.

use std::collections::BTreeMap;
use std::process::ExitCode;

#[derive(Default, Clone, Copy)]
struct Cell {
    bytes: u64,
    raw: u64,
    count: u64,
}

#[derive(Default)]
struct Stats {
    cells: BTreeMap<String, Cell>,
}

impl Stats {
    fn add(&mut self, cat: impl Into<String>, bytes: u64, raw: u64) {
        let c = self.cells.entry(cat.into()).or_default();
        c.bytes += bytes;
        c.raw += raw;
        c.count += 1;
    }
    fn merge(&mut self, o: &Stats) {
        for (k, v) in &o.cells {
            let c = self.cells.entry(k.clone()).or_default();
            c.bytes += v.bytes;
            c.raw += v.raw;
            c.count += v.count;
        }
    }
    fn total(&self) -> u64 {
        self.cells.values().map(|c| c.bytes).sum()
    }
}

struct Fail(String);
type R<T> = Result<T, Fail>;
fn err<T>(s: impl Into<String>) -> R<T> {
    Err(Fail(s.into()))
}

struct Rd<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn new(d: &'a [u8]) -> Self {
        Rd { d, p: 0 }
    }
    fn left(&self) -> usize {
        self.d.len() - self.p
    }
    fn bytes(&mut self, n: usize) -> R<&'a [u8]> {
        if n > self.left() {
            return err(format!("truncated at {} (+{n})", self.p));
        }
        let s = &self.d[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> R<u8> {
        Ok(self.bytes(1)?[0])
    }
    fn u16(&mut self) -> R<u16> {
        Ok(u16::from_be_bytes(self.bytes(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> R<u32> {
        Ok(u32::from_be_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> R<i32> {
        Ok(self.u32()? as i32)
    }
    fn u64(&mut self) -> R<u64> {
        Ok(u64::from_be_bytes(self.bytes(8)?.try_into().unwrap()))
    }
    /// u32 in PSD, u64 in PSB.
    fn len(&mut self, psb: bool) -> R<usize> {
        let n = if psb { self.u64()? } else { self.u32()? as u64 };
        usize::try_from(n).or_else(|_| err("length overflow"))
    }
    fn sub(&mut self, n: usize) -> R<Rd<'a>> {
        Ok(Rd::new(self.bytes(n)?))
    }
}

struct Doc {
    psb: bool,
    channels: u16,
    width: u64,
    height: u64,
    depth: u16,
}

impl Doc {
    fn row_bytes(&self, w: u64) -> u64 {
        (w * self.depth as u64).div_ceil(8)
    }
}

const COMPRESSION: [&str; 4] = ["raw", "rle", "zip", "zippred"];

fn comp_name(c: u16) -> String {
    COMPRESSION.get(c as usize).map_or_else(|| format!("comp{c}"), |s| s.to_string())
}

/// Decoded length of PackBits data, or None if it is malformed.
fn packbits_len(mut d: &[u8]) -> Option<u64> {
    let mut n = 0u64;
    while let [h, rest @ ..] = d {
        let h = *h as i8;
        if h >= 0 {
            let k = h as usize + 1;
            if rest.len() < k {
                return None;
            }
            n += k as u64;
            d = &rest[k..];
        } else if h != -128 {
            if rest.is_empty() {
                return None;
            }
            n += (1 - h as i64) as u64;
            d = &rest[1..];
        } else {
            d = rest;
        }
    }
    Some(n)
}

/// Keys whose length is 64-bit in PSB.
fn long_key(k: &[u8]) -> bool {
    matches!(
        k,
        b"LMsk" | b"Lr16" | b"Lr32" | b"Layr" | b"Mt16" | b"Mt32" | b"Mtrn" | b"Alph" | b"FMsk"
            | b"lnk2" | b"FEid" | b"FXid" | b"PxSD"
    )
}

fn key_str(k: &[u8]) -> String {
    k.iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '?' }).collect()
}

fn is_sig(d: &[u8]) -> bool {
    d.len() >= 4 && (&d[..4] == b"8BIM" || &d[..4] == b"8B64")
}

/// Tagged blocks (additional layer information) until the reader is exhausted. Padding
/// between blocks is inconsistent among writers, so the next block is looked for at the
/// unpadded, 2-aligned and 4-aligned positions.
fn tagged_blocks(rd: &mut Rd, doc: &Doc, prefix: &str, st: &mut Stats) -> R<()> {
    loop {
        // Find the next signature.
        let start = rd.p;
        let mut found = false;
        for align in [1, 2, 4] {
            let q = start.next_multiple_of(align);
            if q <= rd.d.len() && is_sig(&rd.d[q..]) {
                if q > start {
                    st.add(format!("{prefix}/pad"), (q - start) as u64, 0);
                }
                rd.p = q;
                found = true;
                break;
            }
        }
        if !found {
            if rd.left() > 0 {
                let rest = rd.left();
                if rd.d[rd.p..].iter().all(|&b| b == 0) && rest < 4 {
                    st.add(format!("{prefix}/pad"), rest as u64, 0);
                } else {
                    st.add(format!("{prefix}/unparsed"), rest as u64, 0);
                }
                rd.p = rd.d.len();
            }
            return Ok(());
        }
        let hstart = rd.p;
        rd.bytes(4)?;
        let key = rd.bytes(4)?;
        let n = if doc.psb && long_key(key) { rd.u64()? as usize } else { rd.u32()? as usize };
        let mut body = rd.sub(n)?;
        let head = (rd.p - hstart - n) as u64;
        let k = key_str(key);
        st.add(format!("{prefix}/tag-hdr"), head, 0);
        match key {
            b"Lr16" | b"Lr32" | b"Layr" => layer_info(&mut body, doc, st)?,
            b"lnk2" | b"lnkD" | b"lnk3" => linked_files(&mut body, doc, prefix, st)?,
            _ => st.add(format!("{prefix}/tag/{k}"), n as u64, 0),
        }
    }
}

/// Embedded (smart object) files, categorized by declared file type.
fn linked_files(rd: &mut Rd, _doc: &Doc, prefix: &str, st: &mut Stats) -> R<()> {
    while rd.left() >= 8 {
        let start = rd.p;
        let n = rd.u64()? as usize;
        let mut e = rd.sub(n)?;
        let ty = |e: &mut Rd| -> R<(String, u64)> {
            let _kind = e.bytes(4)?;
            let _ver = e.u32()?;
            let idl = e.u8()? as usize;
            e.bytes(idl)?;
            let nl = e.u32()? as usize;
            e.bytes(nl * 2)?;
            let ft = key_str(e.bytes(4)?);
            e.bytes(4)?;
            let dl = e.u64()?;
            Ok((ft, dl))
        };
        let (ft, dl) = ty(&mut e).unwrap_or(("?".into(), 0));
        let ft = ft.trim().to_string();
        let ft = if ft.is_empty() { "none".into() } else { ft };
        let total = (rd.p - start) as u64;
        let dl = dl.min(total);
        st.add(format!("{prefix}/lnk/{ft}"), dl, 0);
        st.add(format!("{prefix}/lnk/meta"), total - dl, 0);
        // Entries are padded to 4.
        let pad = (rd.p.next_multiple_of(4) - rd.p).min(rd.left());
        if pad > 0 {
            rd.bytes(pad)?;
            st.add(format!("{prefix}/lnk/meta"), pad as u64, 0);
        }
    }
    if rd.left() > 0 {
        st.add(format!("{prefix}/lnk/meta"), rd.left() as u64, 0);
    }
    Ok(())
}

struct ChanRef {
    id: i16,
    len: usize,
    w: u64,
    h: u64,
}

/// Layer info: layer records followed by channel image data.
fn layer_info(rd: &mut Rd, doc: &Doc, st: &mut Stats) -> R<()> {
    if rd.left() == 0 {
        return Ok(());
    }
    let count = (rd.u16()? as i16).unsigned_abs() as usize;
    st.add("layer/records", 2, 0);
    st.add("layer/count", 0, count as u64);
    let mut chans: Vec<ChanRef> = Vec::new();
    for _ in 0..count {
        let s = rd.p;
        let top = rd.i32()?;
        let left = rd.i32()?;
        let bottom = rd.i32()?;
        let right = rd.i32()?;
        let lw = (right as i64 - left as i64).max(0) as u64;
        let lh = (bottom as i64 - top as i64).max(0) as u64;
        let nc = rd.u16()? as usize;
        let first = chans.len();
        for _ in 0..nc {
            let id = rd.u16()? as i16;
            let len = rd.len(doc.psb)?;
            chans.push(ChanRef { id, len, w: lw, h: lh });
        }
        rd.bytes(4 + 4 + 4)?; // sig, blend key, opacity/clip/flags/filler
        let el = rd.u32()? as usize;
        let mut ex = rd.sub(el)?;
        // Mask data.
        let ml = ex.u32()? as usize;
        let mut m = ex.sub(ml)?;
        if ml >= 16 {
            let t = m.i32()? as i64;
            let l = m.i32()? as i64;
            let b = m.i32()? as i64;
            let r = m.i32()? as i64;
            let (mw, mh) = ((r - l).max(0) as u64, (b - t).max(0) as u64);
            // The real user mask (-3) rectangle comes later, after flags; approximate it.
            let mut real = (mw, mh);
            if ml >= 36 {
                m.bytes(1)?; // default color
                let flags = m.u8()?;
                if flags & 0x10 != 0 {
                    let pf = m.u8()?;
                    for bit in [1u8, 2, 4, 8] {
                        if pf & bit != 0 {
                            m.bytes(if bit & 5 != 0 { 1 } else { 8 })?;
                        }
                    }
                }
                if m.left() >= 18 {
                    m.bytes(2)?;
                    let t = m.i32()? as i64;
                    let l = m.i32()? as i64;
                    let b = m.i32()? as i64;
                    let r = m.i32()? as i64;
                    real = ((r - l).max(0) as u64, (b - t).max(0) as u64);
                }
            }
            for c in &mut chans[first..] {
                match c.id {
                    -2 => (c.w, c.h) = (mw, mh),
                    -3 => (c.w, c.h) = real,
                    _ => {}
                }
            }
        }
        let bl = ex.u32()? as usize;
        ex.bytes(bl)?;
        let nl = ex.u8()? as usize;
        ex.bytes(nl)?;
        let np = (1 + nl).next_multiple_of(4) - 1 - nl;
        ex.bytes(np.min(ex.left()))?;
        st.add("layer/records", (ex.p + (rd.p - s - el)) as u64, 0);
        tagged_blocks(&mut ex, doc, "layer", st)?;
    }
    for c in &chans {
        let kind = match c.id {
            -1 => "alpha",
            -2 | -3 => "mask",
            _ => "color",
        };
        let mut d = rd.sub(c.len)?;
        if c.len < 2 {
            st.add("layer/chan/empty", c.len as u64, 0);
            continue;
        }
        let comp = d.u16()?;
        let raw = doc.row_bytes(c.w) * c.h;
        let cat = format!("layer/chan/{kind}/{}", comp_name(comp));
        if comp == 1 {
            let cb = if doc.psb { 4 } else { 2 };
            let counts = (c.h as usize * cb).min(d.left());
            d.bytes(counts)?;
            st.add(format!("layer/chan/{kind}/rle-counts"), (2 + counts) as u64, 0);
            if packbits_len(&d.d[d.p..]) != Some(raw) {
                st.add("layer/chan/rle-mismatch", 0, 1);
            }
            st.add(cat, d.left() as u64, raw);
        } else {
            st.add(cat, c.len as u64, raw);
        }
    }
    let rest = rd.left();
    if rest > 0 {
        let cat = if rd.d[rd.p..].iter().all(|&b| b == 0) { "layer/pad" } else { "layer/unparsed" };
        st.add(cat, rest as u64, 0);
        rd.p = rd.d.len();
    }
    Ok(())
}

fn resource_cat(id: u16) -> &'static str {
    match id {
        1033 | 1036 => "res/thumbnail",
        1060 => "res/xmp",
        1039 => "res/icc",
        1028 => "res/iptc",
        1058 | 1059 => "res/exif",
        1065 => "res/layer-comps",
        1088 => "res/path-sel",
        2000..=2997 => "res/paths",
        4000..=4999 => "res/plugin",
        _ => "res/other",
    }
}

fn analyze(d: &[u8], st: &mut Stats) -> R<Doc> {
    let mut rd = Rd::new(d);
    if rd.bytes(4)? != b"8BPS" {
        return err("not 8BPS");
    }
    let ver = rd.u16()?;
    if ver != 1 && ver != 2 {
        return err(format!("version {ver}"));
    }
    rd.bytes(6)?;
    let channels = rd.u16()?;
    let height = rd.u32()? as u64;
    let width = rd.u32()? as u64;
    let depth = rd.u16()?;
    let mode = rd.u16()?;
    let doc = Doc { psb: ver == 2, channels, width, height, depth };
    st.add("header", 26, 0);
    st.add(format!("meta/depth{depth}"), 0, 1);
    st.add(format!("meta/mode{mode}"), 0, 1);

    let n = rd.u32()? as usize;
    rd.bytes(n)?;
    st.add("colormode", 4 + n as u64, 0);

    let n = rd.u32()? as usize;
    st.add("res/hdr", 4, 0);
    let mut res = rd.sub(n)?;
    while res.left() >= 12 {
        let s = res.p;
        res.bytes(4)?;
        let id = res.u16()?;
        let nl = res.u8()? as usize;
        res.bytes(nl + (nl + 1) % 2)?;
        let dl = res.u32()? as usize;
        let h = res.p - s;
        let pad = dl % 2;
        // Some writers get the length slightly wrong; trust the next signature within 2 bytes.
        let next = res.p + dl + pad;
        let end = [0isize, -1, -2, 1, 2]
            .iter()
            .map(|&o| next.wrapping_add_signed(o))
            .find(|&q| q >= res.p && (q == res.d.len() || res.d.get(q..q + 4) == Some(b"8BIM")))
            .unwrap_or(next);
        if end != next {
            st.add("res/misaligned", 0, 1);
        }
        res.bytes(end - res.p)?;
        st.add("res/hdr", (h + pad) as u64, 0);
        st.add(resource_cat(id), (end - s - h - pad) as u64, 0);
    }
    if res.left() > 0 {
        st.add("res/unparsed", res.left() as u64, 0);
    }

    let n = rd.len(doc.psb)?;
    let mut lm = rd.sub(n)?;
    st.add("layer/hdr", if doc.psb { 8 } else { 4 }, 0);
    if n > 0 {
        let ln = lm.len(doc.psb)?;
        st.add("layer/hdr", if doc.psb { 8 } else { 4 }, 0);
        let mut li = lm.sub(ln)?;
        layer_info(&mut li, &doc, st)?;
        if lm.left() >= 4 {
            let gl = lm.u32()? as usize;
            lm.bytes(gl.min(lm.left()))?;
            st.add("gmask", 4 + gl as u64, 0);
        }
        tagged_blocks(&mut lm, &doc, "global", st)?;
    }

    // Composite image.
    let comp = rd.u16()?;
    let raw = doc.row_bytes(width) * height * channels as u64;
    let rest = rd.left() as u64;
    if comp == 1 {
        let counts = height * channels as u64 * if doc.psb { 4 } else { 2 };
        st.add("composite/rle-counts", 2 + counts.min(rest), 0);
        st.add("composite/rle", rest.saturating_sub(counts), raw);
    } else {
        st.add(format!("composite/{}", comp_name(comp)), 2 + rest, raw);
    }
    Ok(doc)
}

fn main() -> ExitCode {
    let mut verbose = false;
    let mut files = Vec::new();
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "-v" => verbose = true,
            _ => files.push(a),
        }
    }
    let mut all = Stats::default();
    let mut failed = 0;
    for f in &files {
        let d = match std::fs::read(f) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("{f}: {e}");
                failed += 1;
                continue;
            }
        };
        let mut st = Stats::default();
        let res = analyze(&d, &mut st);
        let covered = st.total();
        if covered < d.len() as u64 {
            st.add("unparsed", d.len() as u64 - covered, 0);
        }
        match res {
            Ok(doc) => {
                if verbose {
                    let pick = |p: &str| -> u64 {
                        st.cells.iter().filter(|(k, _)| k.starts_with(p)).map(|(_, c)| c.bytes).sum()
                    };
                    let size = d.len() as f64;
                    println!(
                        "{f}\t{}x{}x{}@{}\t{}\tlayers {:.3}\tcomposite {:.3}\tres {:.3}\tlnk {:.3}\tunparsed {:.3}",
                        doc.width,
                        doc.height,
                        doc.channels,
                        doc.depth,
                        d.len(),
                        pick("layer/chan") as f64 / size,
                        pick("composite") as f64 / size,
                        pick("res/") as f64 / size,
                        st.cells.iter().filter(|(k, _)| k.contains("/lnk/")).map(|(_, c)| c.bytes).sum::<u64>() as f64
                            / size,
                        pick("unparsed") as f64 / size + pick("layer/unparsed") as f64 / size,
                    );
                }
                all.add("files", 0, 1);
            }
            Err(Fail(e)) => {
                eprintln!("{f}: {e}");
                failed += 1;
                all.add("files-failed", 0, 1);
            }
        }
        all.merge(&st);
    }
    let total = all.total() as f64;
    println!("{:<32} {:>14} {:>7} {:>14} {:>7} {:>8}", "category", "bytes", "%", "raw", "ratio", "count");
    for (k, c) in &all.cells {
        let ratio = if c.raw > 0 && c.bytes > 0 { format!("{:.3}", c.bytes as f64 / c.raw as f64) } else { String::new() };
        println!(
            "{k:<32} {:>14} {:>7.3} {:>14} {:>7} {:>8}",
            c.bytes,
            100.0 * c.bytes as f64 / total,
            c.raw,
            ratio,
            c.count
        );
    }
    println!("{:<32} {:>14}", "total", all.total());
    if failed > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}
