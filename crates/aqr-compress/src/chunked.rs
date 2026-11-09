//! Disk images whose disk is in chunks, some compressed (aqr-disk's `chunked`): UDIF images
//! (`.dmg`) of compressed chunks, qcow2 images of compressed clusters, streamOptimized VMDK extents. The disk is a content,
//! recompressed in turn (a disk image of its own: its files taken apart), cut into the parts of
//! its chunks once (a Split of all of it, so that it is one content): zlib's and raw DEFLATE
//! chunks rebuilt from their parts by applications of aqr-flate's (of 32 MiB of data each; zlib's
//! headers and checksums, qcow2's padding kept), bzip2's each by aqr-bz2's, LZFSE's each by
//! aqr-bvx's, raw ones those parts as they are, zeros dropped (each part the source of a Patch of
//! nothing), and so the parts of chunks not rebuilt, kept as they are. The rest of the file (a
//! DMG's property list and trailer, qcow2's header and tables, VMDK's markers, what is between
//! the chunks) is
//! kept as it is. A UDIF or qcow2 image of raw chunks only is a container of aqr-disk's, not this;
//! one of chunks of another compression (LZMA, ADC, qcow2's zstd) is not taken yet.

use aqr_disk::chunked::{Chunked, How};
use aqr_disk::dmg;
use aqr_flate::formula::Derived;
use aqr_formula::store::Writer;
use aqr_formula::{Formula, Kind, Node};

use crate::{Cx, Options};

/// Chunks deconstructed at once (of 1 MiB of data each, as hdiutil writes them), their data written
/// to the disk as each batch is done.
const BATCH: usize = 64;

/// Chunks (from the disk's start) among which the largest of zlib's is tried for zlib's emulation.
const SAMPLED: usize = 64;

/// Bytes of data at most of the zlib chunks rebuilt by one application, as aqr-disk's
/// `compressed::STREAM_DATA` (but one chunk of more).
const GROUP_DATA: usize = 32 << 20;

/// Whether `d` is a UDIF image of compressed chunks this takes (zlib's; raw ones and zeros too).
pub(crate) fn starts(d: &[u8]) -> bool {
    dmg::parse(d).is_some_and(|u| {
        u.chunks.iter().any(|c| matches!(c.how, How::Zlib | How::Bzip2 | How::Lzfse))
            && u.chunks.iter().all(|c| matches!(c.how, How::Zlib | How::Bzip2 | How::Lzfse | How::Raw | How::Zeros))
    })
}

/// The formula of UDIF image `d` ([`chunked`]).
pub(crate) fn dmg(d: &[u8], opt: &Options, cx: &Cx) -> Option<Formula> {
    let u = dmg::parse(d)?;
    chunked(d, &Chunked { len: u.len, chunks: u.chunks }, "dmg", opt, cx)
}

/// The formula of compressed container `d` ([`chunked`]: a qcow2 image of compressed clusters, a
/// streamOptimized VMDK extent), None where it is not one.
pub(crate) fn container(d: &[u8], opt: &Options, cx: &Cx) -> Option<Formula> {
    let u = aqr_disk::chunked::container(d)?;
    chunked(d, &u, if d.starts_with(b"KDMV") { "vmdk" } else { "qcow2" }, opt, cx)
}

/// The formula of image `d` of chunks `u`, its bytes kept of class `kept`.
fn chunked(d: &[u8], u: &Chunked, kept: &'static str, opt: &Options, cx: &Cx) -> Option<Formula> {
    // the chunks by where they are in the disk, covering it one after another
    let mut by_disk: Vec<usize> = (0..u.chunks.len()).collect();
    by_disk.sort_by_key(|&k| u.chunks[k].disk.start);
    let mut at = 0;
    for &k in &by_disk {
        if u.chunks[k].disk.start != at {
            return None;
        }
        at = u.chunks[k].disk.end;
    }
    let len = usize::try_from(u.len).ok()?;
    if at != u.len {
        return None;
    }
    // the zlib chunks deconstructed on the threads, a batch at a time in the order of the disk, the
    // data of each written to the disk as it is made (its records kept: what rebuilds the
    // chunks); none probed, as they are parts of one disk (the first of them, of the partition
    // table's sectors, is no probe of the rest)
    for c in &u.chunks {
        if !matches!(c.how, How::Zlib | How::Deflate | How::Bzip2 | How::Lzfse | How::Zeros)
            && !(c.how == How::Raw && c.file.len() as u64 == c.disk.end - c.disk.start)
        {
            return None;
        }
    }
    // (the disk as data inflated, as aqr-flate's streams are: within what the input allows)
    if !cx.inflate(u.len) {
        return None;
    }
    let (mut profile, zlib) = opt.flate();
    // hdiutil writes every chunk with zlib's level 1 (qemu's clusters zlib's level 6, of a window of
    // 4 KiB), whose misses the default profile codes in about a tenth of the stream, and zlib's
    // emulation (of the adaptive profiles) in none: where
    // it predicts every token of a chunk, the chunks are rebuilt by it, its parameters hinted (none
    // searched for, which is what makes those profiles slow), else as aqr-flate's profile is
    // (a zlib stream's header and where its DEFLATE starts)
    let header = |how: How, m: &[u8]| match how {
        How::Zlib => (aqr_flate::Hints::zlib_header([m[0], m[1]]), 2),
        _ => (aqr_flate::Hints::default(), 0),
    };
    let mut params = None;
    let sample = (by_disk.iter().take(SAMPLED).map(|&k| &u.chunks[k]))
        .filter(|c| matches!(c.how, How::Zlib | How::Deflate) && c.file.len() > 2)
        .max_by_key(|c| c.disk.end - c.disk.start);
    if let Some(c) = sample.filter(|_| zlib) {
        let profile = profile.max(aqr_flate::ADAPTIVE);
        let sopt = aqr_flate::Options {
            threads: 1,
            max_data: (c.disk.end - c.disk.start) as usize,
            ..aqr_flate::Options::for_profile(profile).ok()?
        };
        let m = &d[c.file.clone()];
        let (hints, at) = header(c.how, m);
        let report = aqr_flate::Parsed::head(&m[at..], &sopt).and_then(|p| p.deconstruct(&sopt, &hints));
        if let Ok((_, _, r)) = report
            && r.misses == 0
            && r.zlib.is_some()
        {
            params = r.zlib;
        }
    }
    if params.is_some() {
        profile = profile.max(aqr_flate::ADAPTIVE);
    }
    let fopt = aqr_flate::Options { threads: 1, zlib, store: true, ..aqr_flate::Options::for_profile(profile).ok()? };
    let mut made: Vec<Made> = vec![Made::Kept; u.chunks.len()];
    let mut disk = Writer::new(Some(len));
    for batch in by_disk.chunks(BATCH) {
        let taken = aqr_common::pool::par_map(batch.len(), opt.threads.max(1), |i| {
            let c = &u.chunks[batch[i]];
            let (m, n) = (&d[c.file.clone()], (c.disk.end - c.disk.start) as usize);
            match c.how {
                How::Zlib => {
                    let opts = aqr_flate::Options { max_data: n, ..fopt.clone() };
                    let parsed = aqr_flate::Parsed::head(m.get(2..)?, &opts).ok()?;
                    let raw = 2..2 + parsed.len();
                    let sum = m.get(raw.end..raw.end + 4)?;
                    if parsed.data().len() != n
                        || aqr_common::adler32::hash(parsed.data()) != u32::from_be_bytes(sum.try_into().unwrap())
                    {
                        return None;
                    }
                    let (data, record, _) =
                        parsed.deconstruct_bytes(&opts, &aqr_flate::Hints { params, ..header(How::Zlib, m).0 }).ok()?;
                    let rebuilt = record.len() < raw.len();
                    Some((Some(data), if rebuilt { Made::Zlib { raw, record } } else { Made::Kept }))
                }
                How::Deflate => {
                    // (qemu's last cluster, past the disk's end, is compressed whole: of more data
                    // than its part, kept as it is)
                    let opts = aqr_flate::Options { max_data: n.max(1 << 16).saturating_mul(2), ..fopt.clone() };
                    let parsed = aqr_flate::Parsed::head(m, &opts).ok()?;
                    let raw = 0..parsed.len();
                    if parsed.data().len() > n {
                        return Some((Some(parsed.data()[..n].to_vec().into()), Made::Kept));
                    }
                    if parsed.data().len() != n {
                        return None;
                    }
                    let (data, record, _) =
                        parsed.deconstruct_bytes(&opts, &aqr_flate::Hints { params, ..Default::default() }).ok()?;
                    let rebuilt = record.len() < raw.len();
                    Some((Some(data), if rebuilt { Made::Zlib { raw, record } } else { Made::Kept }))
                }
                How::Bzip2 => {
                    let mut yes = |_: usize, _: &[u8], _: bool| true;
                    if let Some((sp, data)) = aqr_bz2::formula::take_whole(m, aqr_bz2::Framing::Bzip2, 1, n, &mut yes) {
                        return (data.len() == n).then(|| (Some(data.into()), Made::Bzip2(sp)));
                    }
                    let s = aqr_bz2::stream::decode(m, true, n).ok()?;
                    (s.data.len() == n && s.len == m.len()).then(|| (Some(s.data.into()), Made::Kept))
                }
                How::Lzfse => {
                    // (split, and joined again to check it: some 40 MB/s of data a thread each)
                    let st = aqr_bvx::recons::split(m, n).ok().filter(|sp| sp.used == m.len() && sp.data.len() == n);
                    if let Some(sp) = st {
                        let mut out = Vec::with_capacity(m.len());
                        if aqr_bvx::recons::join_onto(&sp.data, &sp.record, &sp.counts, &mut out).is_ok() && out == m {
                            let made = Made::Lzfse { params: sp.counts.params(), record: sp.record };
                            return Some((Some(sp.data.into()), made));
                        }
                    }
                    let st = aqr_bvx::bvx::decode(m, n).ok()?;
                    (st.data.len() == n && st.used == m.len()).then(|| (Some(st.data.into()), Made::Kept))
                }
                How::Raw | How::Zeros => Some((None, Made::Kept)),
                _ => None,
            }
        });
        for (&k, t) in batch.iter().zip(taken) {
            let c = &u.chunks[k];
            let (data, m) = t?;
            match data {
                Some(data) => disk.write(&data),
                None if c.how == How::Raw => disk.write(&d[c.file.clone()]),
                None => disk.write(&vec![0; (c.disk.end - c.disk.start) as usize]),
            }
            made[k] = m;
        }
    }
    let mut f = Formula::default();
    f.push(kept, Vec::new());
    let drop = f.push("dmg/drop", vec![1, 0, 0]);
    // (a disk image, by its kind: recompressed as one from its start, not scanned)
    let whole = f.push_content("raw", Kind::Format(crate::detect::Format::Disk.name()), disk.into_bytes());
    let size = |k: usize| u.chunks[k].disk.end - u.chunks[k].disk.start;
    let sizes: Vec<u64> = by_disk.iter().map(|&k| size(k)).collect();
    let mut parts: Vec<Option<Node>> = vec![None; u.chunks.len()];
    for (&k, n) in by_disk.iter().zip(f.split_node(whole, sizes)) {
        parts[k] = Some(n);
    }
    // zlib's chunks rebuilt, in the order of the file, by applications of aqr-flate's of
    // [`GROUP_DATA`] of data at most each, split into them (one of all would hold all of the disk
    // where it is evaluated beforehand), their records and indexes one stream each, sliced
    let mut file_order: Vec<usize> = (0..u.chunks.len()).collect();
    file_order.sort_by_key(|&k| u.chunks[k].file.start);
    let groups = |of: &dyn Fn(&Made) -> bool| {
        let mut out: Vec<Vec<usize>> = Vec::new();
        let mut data = 0;
        for &k in file_order.iter().filter(|&&k| of(&made[k])) {
            match out.last_mut() {
                Some(g) if data + size(k) as usize <= GROUP_DATA => g.push(k),
                _ => {
                    out.push(vec![k]);
                    data = 0;
                }
            }
            data += size(k) as usize;
        }
        out
    };
    let split = |f: &mut Formula, a: aqr_formula::Apply, lens: Vec<u64>| match lens.len() {
        1 => vec![Node::Apply(Box::new(a))],
        _ => f.split(a, lens),
    };
    let mut streams: Vec<Option<Node>> = vec![None; u.chunks.len()];
    let (mut records, mut indexes, mut zlib_groups) = (Vec::new(), Vec::new(), Vec::new());
    for g in groups(&|m| matches!(m, Made::Zlib { .. })) {
        let (r, i) = (records.len() as u64, indexes.len() as u64);
        let mut lens = Vec::new();
        for &k in &g {
            let Made::Zlib { raw, record } = &made[k] else { unreachable!("a zlib chunk") };
            lens.push((size(k) as usize, record.len(), raw.len()));
            records.extend_from_slice(record);
        }
        indexes.extend(aqr_flate::module::index(&lens));
        zlib_groups.push((g, lens, r..records.len() as u64, i..indexes.len() as u64));
    }
    let (records_at, indexes_at) = (f.streams.len(), f.streams.len() + 1);
    f.push("flate/record", records);
    f.push("flate/index", indexes);
    for (g, lens, r, i) in zlib_groups {
        let datas: Vec<Node> = g.iter().map(|&k| parts[k].take().expect("its part")).collect();
        let records = Node::slice(records_at, r.start, r.end - r.start);
        let index = Node::slice(indexes_at, i.start, i.end - i.start);
        let raws: Vec<u64> = lens.iter().map(|l| l.2 as u64).collect();
        let a =
            aqr_flate::formula::apply(profile, Node::concat(datas), records, index, raws.iter().sum(), Derived::None)
                .ok()?;
        for (&k, n) in g.iter().zip(split(&mut f, a, raws)) {
            streams[k] = Some(n);
        }
    }
    // (bzip2's each an application of its own, at the top of the file's concatenation: evaluated
    // ahead on the threads, as rebuilding takes some 12 MB/s of data a thread)
    for &k in &file_order {
        if let Made::Bzip2(sp) = &made[k] {
            let part = parts[k].take().expect("its part");
            let a = aqr_bz2::formula::apply_framed(
                aqr_bz2::Framing::Bzip2,
                std::slice::from_ref(sp),
                part,
                u.chunks[k].file.len() as u64,
            );
            streams[k] = Some(Node::Apply(Box::new(a)));
        }
    }
    // (LZFSE's likewise, their records one stream, sliced)
    let mut records = Vec::new();
    let mut lzfse = Vec::new();
    for &k in &file_order {
        if let Made::Lzfse { params, record } = &made[k] {
            lzfse.push((k, params.clone(), records.len() as u64, record.len() as u64));
            records.extend_from_slice(record);
        }
    }
    if !lzfse.is_empty() {
        let at = f.streams.len();
        f.push_as("bvx/record", Kind::Compressed, records);
        for (k, params, start, n) in lzfse {
            let part = parts[k].take().expect("its part");
            let inputs = vec![part, Node::slice(at, start, n)];
            streams[k] = Some(Node::apply(aqr_bvx::formula::MODULE, params, inputs, u.chunks[k].file.len() as u64));
        }
    }
    // the file: what is kept, then each chunk in turn
    let mut kept = Vec::new();
    let mut root = Vec::new();
    let keep = |kept: &mut Vec<u8>, root: &mut Vec<Node>, b: &[u8]| {
        if !b.is_empty() {
            root.push(Node::slice(0, kept.len() as u64, b.len() as u64));
            kept.extend_from_slice(b);
        }
    };
    let mut at = 0;
    for &k in &file_order {
        let c = &u.chunks[k];
        // (chunks overlapping in the file, of tables not hdiutil's, are not taken)
        if c.how != How::Zeros && c.file.start < at {
            return None;
        }
        // where the chunk's bytes are in the file, as they are rebuilt
        let rebuilt = match (&c.how, &made[k]) {
            (How::Zeros, _) | (_, Made::Kept) if c.how != How::Raw => None,
            (How::Zlib | How::Deflate, Made::Zlib { raw, .. }) => {
                Some(c.file.start + raw.start..c.file.start + raw.end)
            }
            _ => Some(c.file.clone()),
        };
        match rebuilt {
            Some(r) => {
                keep(&mut kept, &mut root, &d[at..r.start]);
                root.push(match c.how {
                    How::Raw => parts[k].take().expect("its part"),
                    _ => streams[k].take().expect("its stream"),
                });
                at = r.end;
            }
            // (zeros, and a chunk kept as it is: its part of the disk dropped)
            None => root.push(Node::patch(parts[k].take().expect("its part"), drop.clone(), 0)),
        }
    }
    keep(&mut kept, &mut root, &d[at..]);
    f.streams[0].data = kept.into();
    f.root = Node::concat(root);
    if f.root.len() != d.len() as u64 {
        return None;
    }
    // (the disk's Split recorded, its bytes at hand: not so once the disk is a formula)
    if let Some(pre) = &cx.pre {
        pre.learn_splits(&f, opt.threads);
    }
    Some(f)
}

/// What a chunk is made of in the formula: zlib's, bzip2's or LZFSE's rebuilt (where its DEFLATE
/// stream is in the chunk and its record; the bzip2 stream's parameters; aqr-bvx's params and
/// record), or its bytes kept as they are (raw ones, zeros, and those not rebuilt).
#[derive(Clone)]
enum Made {
    Zlib { raw: std::ops::Range<usize>, record: Vec<u8> },
    Bzip2(aqr_bz2::module::StreamParams),
    Lzfse { params: Vec<u8>, record: Vec<u8> },
    Kept,
}
