//! How much of a PNG file's DEFLATE tokens another, similar image predicts (research tool):
//! as in sets of variants of one picture with layers shown or hidden ("差分"), where most
//! pixels are the same.
//!
//! usage: aqr-png-refprobe [--code] DIR...
//!
//! In each directory, the PNG files of the same header (8 bits per sample, not interlaced)
//! are a group. Each file but one is paired with a parent: a maximum spanning tree of the
//! group, from the file most like the others, by the fraction of 256-byte segments of the
//! inflated data that are the same at the same place. Of each file against its parent:
//! - tokens *in sync*: where the parent has the same token (literal or match) at the same
//!   place of the data;
//! - of those out of sync, the ones over pixels that are the same in the parent (all their
//!   bytes, unfiltered), which a model of the parent's pixels may still predict;
//! - the runs in sync, and the flags telling which tokens are in sync (where the parent has a
//!   token at the same place), at their entropy given the flag before.
//!
//! The record that aqr-png codes each file to, alone, gives rough estimates: the record times
//! the fraction of tokens out of sync (and of those over other pixels), plus the flags. Tokens
//! out of sync are where the images differ, so they may well cost more than the others.
//!
//! With `--code`, each file is also coded against its parent (`formula::encode_against`) and
//! decoded back: the records' size, checked, and the time both take (one thread each).

use std::path::Path;

use aqr_flate::deflate::Token;
use aqr_png::{filter, png};

/// A PNG file: its group, the inflated (filtered) and unfiltered data, the tokens, and the
/// bytes of its record coded alone.
struct Image {
    path: std::path::PathBuf,
    key: (u32, u32, u8),
    stride: usize,
    data: Vec<u8>,
    raw: Vec<u8>,
    tokens: Vec<Token>,
    record: usize,
    file: usize,
}

fn load(path: &Path) -> Option<Image> {
    let file = std::fs::read(path).ok()?;
    let (chunks, _) = png::chunks(&file)?;
    let ih = png::Ihdr::parse(chunks.iter().find(|c| &c.kind == b"IHDR")?.data)?;
    if ih.depth != 8 || ih.interlace != 0 || ih.channels() == 0 {
        return None;
    }
    let idat: Vec<u8> = chunks.iter().filter(|c| &c.kind == b"IDAT").flat_map(|c| c.data.iter().copied()).collect();
    let f = aqr_flate::inflate::parse(idat.get(2..)?).ok()?;
    let (bpp, stride) = (ih.channels(), ih.channels() * ih.width as usize);
    let mut raw = Vec::new();
    if f.data.len() != (stride + 1) * ih.height as usize || !filter::unfilter(&f.data, stride, bpp, &mut raw) {
        return None;
    }
    let record = aqr_png::formula::encode(&file, 1)
        .and_then(|f| f.streams.into_iter().find(|s| s.class == "png/records"))
        .map_or(file.len(), |s| s.data.len());
    Some(Image {
        path: path.to_owned(),
        key: (ih.width, ih.height, ih.color),
        stride,
        data: f.data,
        raw,
        tokens: f.tokens,
        record,
        file: file.len(),
    })
}

/// The fraction of 256-byte segments of `a` and `b` that are the same.
fn alike(a: &[u8], b: &[u8]) -> f64 {
    let (sa, sb) = (a.chunks(256), b.chunks(256));
    let n = sa.len().max(1);
    sa.zip(sb).filter(|(x, y)| x == y).count() as f64 / n as f64
}

/// Parents of a maximum spanning tree of `sim`, from the member most like the others.
fn tree(sim: &[Vec<f64>]) -> Vec<Option<usize>> {
    let n = sim.len();
    let root = (0..n).max_by(|&i, &j| sim[i].iter().sum::<f64>().total_cmp(&sim[j].iter().sum())).unwrap();
    let mut parent = vec![None; n];
    let mut best: Vec<(f64, usize)> = (0..n).map(|k| (sim[root][k], root)).collect();
    let mut done = vec![false; n];
    done[root] = true;
    for _ in 1..n {
        let k = (0..n).filter(|&k| !done[k]).max_by(|&i, &j| best[i].0.total_cmp(&best[j].0)).unwrap();
        done[k] = true;
        parent[k] = Some(best[k].1);
        for o in 0..n {
            if !done[o] && sim[k][o] > best[o].0 {
                best[o] = (sim[k][o], k);
            }
        }
    }
    parent
}

/// Counts of a file against its parent.
#[derive(Default, Clone, Copy)]
struct Counts {
    tokens: usize,
    synced: usize,
    synced_bytes: usize,
    bytes: usize,
    runs: usize,
    /// Out of sync, over pixels that are the same in the parent.
    same_pixels: usize,
    same_pixel_bytes: usize,
    /// Of those, the literals.
    same_pixel_lits: usize,
    /// Where the parent has a token at the same place, whether the one before was in sync
    /// and whether this one is.
    flags: [[usize; 2]; 2],
}

impl Counts {
    /// Bytes of the flags telling which tokens are in sync, at their entropy given the one
    /// before (where the parent has no token at the same place, none is in sync).
    fn flag_bytes(&self) -> f64 {
        let h = |n: [usize; 2]| {
            let t = (n[0] + n[1]) as f64;
            n.iter().filter(|&&k| k > 0).map(|&k| -(k as f64) * (k as f64 / t).log2()).sum::<f64>()
        };
        (h(self.flags[0]) + h(self.flags[1])) / 8.0
    }
}

impl std::ops::AddAssign for Counts {
    fn add_assign(&mut self, o: Counts) {
        self.tokens += o.tokens;
        self.synced += o.synced;
        self.synced_bytes += o.synced_bytes;
        self.bytes += o.bytes;
        self.runs += o.runs;
        self.same_pixels += o.same_pixels;
        self.same_pixel_bytes += o.same_pixel_bytes;
        self.same_pixel_lits += o.same_pixel_lits;
        for (f, g) in self.flags.as_flattened_mut().iter_mut().zip(o.flags.as_flattened()) {
            *f += g;
        }
    }
}

fn sync(b: &Image, a: &Image) -> Counts {
    let mut at = vec![u32::MAX; a.data.len() + 1];
    let mut p = 0;
    for (i, t) in a.tokens.iter().enumerate() {
        at[p] = i as u32;
        p += len(t);
    }
    // whether the byte of the inflated data at `q` unfilters to the parent's (a filter type
    // byte to the parent's filter type)
    let same = |q: usize| {
        let (y, x) = (q / (b.stride + 1), q % (b.stride + 1));
        if x == 0 { b.data[q] == a.data[q] } else { b.raw[y * b.stride + x - 1] == a.raw[y * b.stride + x - 1] }
    };
    let (mut s, mut p, mut was) = (Counts::default(), 0, false);
    for t in &b.tokens {
        let n = len(t);
        let i = at.get(p).copied().unwrap_or(u32::MAX);
        let ok = i != u32::MAX && a.tokens[i as usize] == *t;
        s.tokens += 1;
        s.bytes += n;
        if i != u32::MAX {
            s.flags[was as usize][ok as usize] += 1;
        }
        if ok {
            s.synced += 1;
            s.synced_bytes += n;
            s.runs += !was as usize;
        } else if (p..p + n).all(same) {
            s.same_pixels += 1;
            s.same_pixel_lits += matches!(t, Token::Lit(_)) as usize;
            s.same_pixel_bytes += n;
        }
        was = ok;
        p += n;
    }
    s
}

/// The record of `b` coded against `a`, its size (checked by decoding it back), and the
/// seconds coding and decoding take.
fn against(b: &Image, a: &Image) -> (usize, f64, f64) {
    let (fb, fa) = (std::fs::read(&b.path).unwrap(), std::fs::read(&a.path).unwrap());
    let t = std::time::Instant::now();
    let f = aqr_png::formula::encode_against(&fb, &fa, 1).expect("coded");
    let enc = t.elapsed().as_secs_f64();
    let inputs: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
    let t = std::time::Instant::now();
    let decode = if inputs.len() == 4 { aqr_png::formula::decode_against } else { aqr_png::formula::decode };
    let out = decode(&[], &inputs, fb.len() as u64, 1).expect("decodes");
    let dec = t.elapsed().as_secs_f64();
    assert!(out == fb, "{} against {} does not round trip", b.path.display(), a.path.display());
    let rec = f.streams.iter().find(|s| s.class == "png/records").unwrap().data.len();
    (rec, enc, dec)
}

fn len(t: &Token) -> usize {
    match *t {
        Token::Lit(_) => 1,
        Token::Match { len, .. } => len as usize,
    }
}

/// `f` on each of `items`, on as many threads as there are cores.
fn par<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out: Vec<std::sync::Mutex<Option<R>>> = items.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    *out[i].lock().unwrap() = Some(f(item));
                }
            });
        }
    });
    out.into_iter().map(|m| m.into_inner().unwrap().unwrap()).collect()
}

fn main() {
    let (mut all, mut files, mut records, mut est_sync, mut est_pixels) = (Counts::default(), 0, 0, 0.0, 0.0);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = args.iter().any(|a| a == "--code");
    let (mut coded, mut enc_s, mut dec_s) = (0usize, 0.0, 0.0);
    for dir in args.into_iter().filter(|a| a != "--code") {
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")))
            .collect();
        paths.sort();
        let images: Vec<Image> = par(&paths, |p| load(p)).into_iter().flatten().collect();
        let mut keys: Vec<_> = images.iter().map(|i| i.key).collect();
        keys.sort();
        keys.dedup();
        let mut dir_sync = Counts::default();
        let (mut dir_files, mut dir_records, mut dir_est_sync, mut dir_est_pixels) = (0, 0, 0.0, 0.0);
        for key in keys {
            let g: Vec<&Image> = images.iter().filter(|i| i.key == key).collect();
            let pairs: Vec<(usize, usize)> = (0..g.len()).flat_map(|i| (i + 1..g.len()).map(move |j| (i, j))).collect();
            let sims = par(&pairs, |&(i, j)| alike(&g[i].data, &g[j].data));
            let mut sim = vec![vec![1.0; g.len()]; g.len()];
            for (&(i, j), &v) in pairs.iter().zip(&sims) {
                (sim[i][j], sim[j][i]) = (v, v);
            }
            let parent = tree(&sim);
            let kids: Vec<(usize, usize)> = parent.iter().enumerate().filter_map(|(k, p)| Some((k, (*p)?))).collect();
            let syncs = par(&kids, |&(k, p)| sync(g[k], g[p]));
            let mut s = Counts::default();
            let (mut est1, mut est2) = (0.0, 0.0);
            for (&(k, _), &x) in kids.iter().zip(&syncs) {
                s += x;
                let r = g[k].record as f64;
                est1 += r * (1.0 - x.synced as f64 / x.tokens as f64) + x.flag_bytes();
                est2 += r * (1.0 - (x.synced + x.same_pixels) as f64 / x.tokens as f64) + x.flag_bytes();
            }
            // the root, alone
            let root = parent.iter().position(|p| p.is_none()).unwrap();
            let (rec, fil) = (g.iter().map(|i| i.record).sum::<usize>(), g.iter().map(|i| i.file).sum::<usize>());
            let (e1, e2) = (est1 + g[root].record as f64, est2 + g[root].record as f64);
            let mut got = String::new();
            if code {
                let res = par(&kids, |&(k, p)| against(g[k], g[p]));
                let c = g[root].record + res.iter().map(|r| r.0).sum::<usize>();
                coded += c;
                enc_s += res.iter().map(|r| r.1).sum::<f64>();
                dec_s += res.iter().map(|r| r.2).sum::<f64>();
                got = format!(", coded {:.1} MB", c as f64 / 1e6);
            }
            if g.len() > 1 {
                println!(
                    "{dir} {}x{} n={}: in sync {:.1}% of tokens ({:.1}% of bytes), out of sync over the same pixels \
                     {:.1}% ({:.1}%), runs {} (flags {:.2} MB); png {:.1} MB, records {:.1} MB, est. {:.1} / {:.1} MB{got}",
                    key.0,
                    key.1,
                    g.len(),
                    100.0 * s.synced as f64 / s.tokens as f64,
                    100.0 * s.synced_bytes as f64 / s.bytes as f64,
                    100.0 * s.same_pixels as f64 / s.tokens as f64,
                    100.0 * s.same_pixel_bytes as f64 / s.bytes as f64,
                    s.runs,
                    s.flag_bytes() / 1e6,
                    fil as f64 / 1e6,
                    rec as f64 / 1e6,
                    e1 / 1e6,
                    e2 / 1e6
                );
            }
            dir_sync += s;
            (dir_files, dir_records, dir_est_sync, dir_est_pixels) =
                (dir_files + fil, dir_records + rec, dir_est_sync + e1, dir_est_pixels + e2);
        }
        all += dir_sync;
        (files, records, est_sync, est_pixels) =
            (files + dir_files, records + dir_records, est_sync + dir_est_sync, est_pixels + dir_est_pixels);
    }
    println!(
        "all: in sync {:.1}% of tokens ({:.1}% of bytes), out of sync over the same pixels {:.1}% ({:.1}%, \
         literals {:.1}%); \
         png {:.1} MB, records {:.1} MB, est. in sync {:.1} MB, with the same pixels {:.1} MB",
        100.0 * all.synced as f64 / all.tokens as f64,
        100.0 * all.synced_bytes as f64 / all.bytes as f64,
        100.0 * all.same_pixels as f64 / all.tokens as f64,
        100.0 * all.same_pixel_bytes as f64 / all.bytes as f64,
        100.0 * all.same_pixel_lits as f64 / all.tokens as f64,
        files as f64 / 1e6,
        records as f64 / 1e6,
        est_sync / 1e6,
        est_pixels / 1e6
    );
    if code {
        println!(
            "coded against the parents: {:.1} MB (all checked), coding {:.1} s, decoding {:.1} s (the parents' files not counted)",
            coded as f64 / 1e6,
            enc_s,
            dec_s
        );
    }
}
