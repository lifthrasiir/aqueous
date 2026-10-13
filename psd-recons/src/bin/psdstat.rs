//! Byte breakdown of PSD/PSB files by section, to see where the bytes go.
//!
//! `psdstat [-v] FILE...` prints, over all files, the bytes of each category and (for pixel
//! data) the decoded size. `-v` adds one line per file. Bytes outside every parsed structure
//! are counted as `other` (padding, or junk), so every file sums to its size.

use std::collections::BTreeMap;
use std::process::ExitCode;

use psd_recons::psd::{Channel, Compression, LayerInfo, Psd, Tag};

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
    fn sum(&self, f: impl Fn(&str) -> bool) -> u64 {
        self.cells.iter().filter(|(k, _)| f(k)).map(|(_, c)| c.bytes).sum()
    }
}

fn key_str(k: &[u8]) -> String {
    k.iter().map(|&b| if b.is_ascii_graphic() || b == b' ' { b as char } else { '?' }).collect()
}

fn be_u32(d: &[u8], p: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(p..p + 4)?.try_into().unwrap()))
}

fn be_u64(d: &[u8], p: usize) -> Option<u64> {
    Some(u64::from_be_bytes(d.get(p..p + 8)?.try_into().unwrap()))
}

/// Embedded (smart object) files of a `lnk2`/`lnkD`/`lnk3` block, by declared file type.
fn linked_files(d: &[u8], prefix: &str, st: &mut Stats) {
    let mut p = 0;
    while d.len() - p >= 8 {
        let n = be_u64(d, p).unwrap() as usize;
        let end = (p + 8).saturating_add(n).min(d.len());
        let e = &d[p + 8..end];
        // kind, version, Pascal unique id, Unicode name, file type, creator, data length
        let ty = (|| {
            let q = 8 + 1 + *e.get(8)? as usize;
            let q = q + 4 + 2 * be_u32(e, q)? as usize;
            Some((key_str(e.get(q..q + 4)?), be_u64(e, q + 8)?))
        })();
        let (ft, dl) = ty.unwrap_or(("?".into(), 0));
        let ft = match ft.trim() {
            "" => "none".into(),
            t => t.to_string(),
        };
        let total = (end - p) as u64;
        let dl = dl.min(total);
        st.add(format!("{prefix}/lnk/{ft}"), dl, 0);
        st.add(format!("{prefix}/lnk/meta"), total - dl, 0);
        p = end.next_multiple_of(4).min(d.len());
    }
}

fn tags(psd: &Psd, d: &[u8], tags: &[Tag], prefix: &str, st: &mut Stats) {
    for t in tags {
        st.add(format!("{prefix}/tag-hdr"), (t.span.len() - t.data.len()) as u64, 0);
        match &t.key {
            // Counted by layer_info().
            b"Layr" | b"Lr16" | b"Lr32" if psd.layer_info.span.start == t.data.start => {}
            b"lnk2" | b"lnkD" | b"lnk3" => linked_files(&d[t.data.clone()], prefix, st),
            k => st.add(format!("{prefix}/tag/{}", key_str(k)), t.data.len() as u64, 0),
        }
    }
}

fn channel(psd: &Psd, d: &[u8], c: &Channel, st: &mut Stats) {
    let kind = match c.id {
        -1 => "alpha",
        -2 | -3 => "mask",
        _ => "color",
    };
    let Some(comp) = c.compression(d) else {
        st.add("layer/chan/empty", c.span.len() as u64, 0);
        return;
    };
    let raw = psd.row_bytes(c.width) * c.height;
    let cat = format!("layer/chan/{kind}/{}", comp.name());
    if comp == Compression::Rle {
        let counts = (c.height as usize * if psd.psb { 4 } else { 2 }).min(c.span.len() - 2);
        st.add(format!("layer/chan/{kind}/rle-counts"), 2 + counts as u64, 0);
        st.add(cat, (c.span.len() - 2 - counts) as u64, raw);
    } else {
        st.add(cat, c.span.len() as u64, raw);
    }
}

fn layer_info(psd: &Psd, d: &[u8], li: &LayerInfo, st: &mut Stats) {
    if li.span.is_empty() {
        return;
    }
    st.add("layer/records", 2, 0);
    st.add("layer/count", 0, li.layers.len() as u64);
    for l in &li.layers {
        let tag_bytes: usize = l.tags.iter().map(|t| t.span.len()).sum();
        st.add("layer/records", (l.record.len() - tag_bytes) as u64, 0);
        tags(psd, d, &l.tags, "layer", st);
        for c in &l.channels {
            channel(psd, d, c, st);
        }
    }
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

fn analyze(psd: &Psd, d: &[u8], st: &mut Stats) {
    st.add("header", 26, 0);
    st.add(format!("meta/depth{}", psd.depth), 0, 1);
    st.add(format!("meta/mode{}", psd.mode), 0, 1);
    st.add("colormode", 4 + psd.color_mode.len() as u64, 0);

    st.add("res/hdr", 4, 0);
    for r in &psd.resources {
        st.add("res/hdr", (r.span.len() - r.data.len()) as u64, 0);
        st.add(resource_cat(r.id), r.data.len() as u64, 0);
    }

    let lf = if psd.psb { 8 } else { 4 };
    st.add("layer/hdr", lf, 0);
    if psd.layer_section.len() > lf as usize {
        st.add("layer/hdr", lf, 0);
    }
    layer_info(psd, d, &psd.layer_info, st);
    if let Some(g) = &psd.global_mask {
        st.add("gmask", g.len() as u64, 0);
    }
    tags(psd, d, &psd.global_tags, "global", st);

    let comp = psd.composite_compression(d);
    let raw = psd.row_bytes(psd.width) * psd.height * psd.channels as u64;
    let n = psd.composite.len() as u64;
    if comp == Compression::Rle {
        let counts = (psd.height * psd.channels as u64 * if psd.psb { 4 } else { 2 }).min(n - 2);
        st.add("composite/rle-counts", 2 + counts, 0);
        st.add("composite/rle", n - 2 - counts, raw);
    } else {
        st.add(format!("composite/{}", comp.name()), n, raw);
    }
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
        match psd_recons::parse(&d) {
            Ok(psd) => {
                analyze(&psd, &d, &mut st);
                let covered = st.total();
                if covered < d.len() as u64 {
                    st.add("other", d.len() as u64 - covered, 0);
                }
                assert_eq!(st.total(), d.len() as u64, "{f}: overcounted");
                if verbose {
                    let size = d.len() as f64;
                    println!(
                        "{f}\t{}x{}x{}@{}\t{}\tlayers {:.3}\tcomposite {:.3}\tres {:.3}\tlnk {:.3}\tother {:.3}",
                        psd.width,
                        psd.height,
                        psd.channels,
                        psd.depth,
                        d.len(),
                        st.sum(|k| k.starts_with("layer/chan")) as f64 / size,
                        st.sum(|k| k.starts_with("composite")) as f64 / size,
                        st.sum(|k| k.starts_with("res/")) as f64 / size,
                        st.sum(|k| k.contains("/lnk/")) as f64 / size,
                        st.sum(|k| k == "other") as f64 / size,
                    );
                }
                all.add("files", 0, 1);
            }
            Err(e) => {
                eprintln!("{f}: {e}");
                failed += 1;
                st.add("unparsed", d.len() as u64, 0);
                all.add("files-failed", 0, 1);
            }
        }
        all.merge(&st);
    }
    let total = all.total() as f64;
    println!("{:<32} {:>14} {:>7} {:>14} {:>7} {:>8}", "category", "bytes", "%", "raw", "ratio", "count");
    for (k, c) in &all.cells {
        let ratio =
            if c.raw > 0 && c.bytes > 0 { format!("{:.3}", c.bytes as f64 / c.raw as f64) } else { String::new() };
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
