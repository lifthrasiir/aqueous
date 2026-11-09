//! UDIF images (`.dmg`) of compressed chunks (aqr-disk's `dmg`): the disk a content, recompressed
//! in turn (a disk image of its own: its files taken apart), cut into the parts of its chunks
//! once (a Split of all of it, so that it is one content): zlib's chunks rebuilt from their parts
//! by applications of aqr-flate's (of 32 MiB of data each; their headers and checksums kept), raw
//! ones those parts as they are, zeros dropped (each part the source of a Patch of nothing). The
//! rest of the file (the property list, the trailer, what is between the chunks) is kept as it
//! is. A UDIF image of raw chunks only is a container of aqr-disk's, not this; one of chunks of
//! another compression (bzip2, LZFSE, LZMA, ADC) is not taken yet.

use aqr_disk::dmg::{self, How};
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

/// A zlib chunk deconstructed: where its DEFLATE stream is in the chunk, the bytes of its data,
/// and its record.
type Member = (std::ops::Range<usize>, usize, Vec<u8>);

/// The class of the bytes kept of an image.
pub(crate) const KEPT: &str = "dmg";

/// Whether `d` is a UDIF image of compressed chunks this takes (zlib's; raw ones and zeros too).
pub(crate) fn starts(d: &[u8]) -> bool {
    dmg::parse(d).is_some_and(|u| {
        u.chunks.iter().any(|c| c.how == How::Zlib)
            && u.chunks.iter().all(|c| matches!(c.how, How::Zlib | How::Raw | How::Zeros))
    })
}

pub(crate) fn dmg(d: &[u8], opt: &Options, cx: &Cx) -> Option<Formula> {
    let u = dmg::parse(d)?;
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
        if !matches!(c.how, How::Zlib | How::Zeros)
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
    // hdiutil writes every chunk with zlib's level 1, whose misses the default profile codes in
    // about a tenth of the stream, and zlib's emulation (of the adaptive profiles) in none: where
    // it predicts every token of a chunk, the chunks are rebuilt by it, its parameters hinted (none
    // searched for, which is what makes those profiles slow), else as aqr-flate's profile is
    let header = |m: &[u8]| aqr_flate::Hints::zlib_header([m[0], m[1]]);
    let mut params = None;
    let sample = (by_disk.iter().take(SAMPLED).map(|&k| &u.chunks[k]))
        .filter(|c| c.how == How::Zlib && c.file.len() > 2)
        .max_by_key(|c| c.disk.end - c.disk.start);
    if let Some(c) = sample.filter(|_| zlib) {
        let profile = profile.max(aqr_flate::ADAPTIVE);
        let sopt = aqr_flate::Options {
            threads: 1,
            max_data: (c.disk.end - c.disk.start) as usize,
            ..aqr_flate::Options::for_profile(profile).ok()?
        };
        let m = &d[c.file.clone()];
        let report = aqr_flate::Parsed::head(&m[2..], &sopt).and_then(|p| p.deconstruct(&sopt, &header(m)));
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
    let mut members: Vec<Option<Member>> = vec![None; u.chunks.len()];
    let mut disk = Writer::new(Some(len));
    for batch in by_disk.chunks(BATCH) {
        let made = aqr_common::pool::par_map(batch.len(), opt.threads.max(1), |i| {
            let c = &u.chunks[batch[i]];
            if c.how != How::Zlib {
                return Some(None);
            }
            let m = &d[c.file.clone()];
            let opts = aqr_flate::Options { max_data: (c.disk.end - c.disk.start) as usize, ..fopt.clone() };
            let parsed = aqr_flate::Parsed::head(m.get(2..)?, &opts).ok()?;
            let raw = 2..2 + parsed.len();
            let sum = m.get(raw.end..raw.end + 4)?;
            if aqr_common::adler32::hash(parsed.data()) != u32::from_be_bytes(sum.try_into().unwrap()) {
                return None;
            }
            let (data, record, _) = parsed.deconstruct_bytes(&opts, &aqr_flate::Hints { params, ..header(m) }).ok()?;
            (data.len() as u64 == c.disk.end - c.disk.start && record.len() < raw.len())
                .then_some(Some((raw, data, record)))
        });
        for (&k, m) in batch.iter().zip(made) {
            let c = &u.chunks[k];
            match m? {
                Some((raw, data, record)) => {
                    disk.write(&data);
                    members[k] = Some((raw, data.len(), record));
                }
                None if c.how == How::Raw => disk.write(&d[c.file.clone()]),
                None => disk.write(&vec![0; (c.disk.end - c.disk.start) as usize]),
            }
        }
    }
    let mut f = Formula::default();
    f.push(KEPT, Vec::new());
    let drop = f.push("dmg/drop", vec![1, 0, 0]);
    // (a disk image, by its kind: recompressed as one from its start, not scanned)
    let whole = f.push_content("raw", Kind::Format(crate::detect::Format::Disk.name()), disk.into_bytes());
    let sizes: Vec<u64> = by_disk.iter().map(|&k| u.chunks[k].disk.end - u.chunks[k].disk.start).collect();
    let mut parts: Vec<Option<Node>> = vec![None; u.chunks.len()];
    for (&k, n) in by_disk.iter().zip(f.split_node(whole, sizes)) {
        parts[k] = Some(n);
    }
    // the zlib chunks' streams, in the order of the file, by applications of [`GROUP_DATA`] of data
    // at most each, split into them (one of all would hold all of the disk where it is evaluated
    // beforehand); their records and indexes one stream each, sliced
    let mut file_order: Vec<usize> = (0..u.chunks.len()).collect();
    file_order.sort_by_key(|&k| u.chunks[k].file.start);
    let zlibs: Vec<usize> = file_order.iter().copied().filter(|&k| members[k].is_some()).collect();
    let mut groups = Vec::new();
    let (mut rest, mut records, mut indexes) = (&zlibs[..], Vec::new(), Vec::new());
    while !rest.is_empty() {
        let (mut n, mut data) = (0, 0);
        while let Some(&k) = rest.get(n).filter(|&&k| n == 0 || data + members[k].as_ref().unwrap().1 <= GROUP_DATA) {
            data += members[k].as_ref().unwrap().1;
            n += 1;
        }
        let (group, after) = rest.split_at(n);
        rest = after;
        let (mut datas, mut lens, mut raws) = (Vec::new(), Vec::new(), Vec::new());
        let (r, i) = (records.len() as u64, indexes.len() as u64);
        for &k in group {
            let (raw, data, record) = members[k].as_ref().expect("a zlib chunk");
            datas.push(parts[k].take().expect("its part"));
            lens.push((*data, record.len(), raw.len()));
            raws.push(raw.len() as u64);
            records.extend_from_slice(record);
        }
        indexes.extend(aqr_flate::module::index(&lens));
        groups.push((datas, raws, r..records.len() as u64, i..indexes.len() as u64));
    }
    let (records_at, indexes_at) = (f.streams.len(), f.streams.len() + 1);
    f.push("flate/record", records);
    f.push("flate/index", indexes);
    let mut streams = Vec::with_capacity(zlibs.len());
    for (datas, raws, r, i) in groups {
        let records = Node::slice(records_at, r.start, r.end - r.start);
        let index = Node::slice(indexes_at, i.start, i.end - i.start);
        let a =
            aqr_flate::formula::apply(profile, Node::concat(datas), records, index, raws.iter().sum(), Derived::None)
                .ok()?;
        streams.extend(match raws.len() {
            1 => vec![Node::Apply(Box::new(a))],
            _ => f.split(a, raws),
        });
    }
    let mut streams = streams.into_iter();
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
        match (&c.how, &members[k]) {
            (How::Zeros, _) => {
                let part = parts[k].take().expect("its part");
                root.push(Node::patch(part, drop.clone(), 0));
            }
            (How::Raw, _) => {
                keep(&mut kept, &mut root, &d[at..c.file.start]);
                root.push(parts[k].take().expect("its part"));
                at = c.file.end;
            }
            (How::Zlib, Some((raw, _, _))) => {
                keep(&mut kept, &mut root, &d[at..c.file.start + raw.start]);
                root.push(streams.next().expect("its stream"));
                at = c.file.start + raw.end;
            }
            _ => return None,
        }
    }
    keep(&mut kept, &mut root, &d[at..]);
    f.streams[0].data = kept.into();
    f.root = Node::concat(root);
    (f.root.len() == d.len() as u64).then_some(f)
}
