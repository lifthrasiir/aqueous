//! Below level 4, where DEFLATE streams are not rebuilt ([`on`]): the files of an archive whose
//! data is that of a DEFLATE stream archived along with them (the body of a ZIP archive's entry,
//! a gzip member's or a zlib stream's) made of the stream by aqr-flate's module `flate-inflate`,
//! the stream a slice of the file's formula. The container keeps the stream as it is, a slice
//! of its own (aqr-zip's bodies kept, [`gzip`] and [`zlib`] here), so that the archive stores it
//! once ([`aqr_formula::Formula::share_slices`]): the file costs only the application.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use aqr_formula::{Bytes, Formula, Node};

use crate::{Format, Options, detect};

/// Data shorter than this is not looked for: a slice of its stream would not be shared
/// ([`crate::archive::SLICE_MIN`]), and what it holds is found by compressing it solid.
const MIN: usize = 4 << 10;

/// The most bytes of data of a zlib stream inflated to tell what it holds (its checksum tells
/// nothing of that).
const ZLIB_MAX: usize = 256 << 20;

/// Whether DEFLATE streams are kept as they are at the level of `opt`, and files holding their
/// data made of them: below level 4, where anything is recompressed.
pub(crate) fn on(opt: &Options) -> bool {
    opt.recompresses() && opt.level.scale() < 4
}

/// A DEFLATE stream of [`Bodies`]: the file holding it (by index), and where it is in the file.
type Body = (usize, Range<usize>);

/// The DEFLATE streams of the files of an archive, by the CRC-32 and size (modulo 2^32, as gzip
/// tells it) of their data, and where the files that hold them are.
#[derive(Default)]
pub(crate) struct Bodies {
    by: HashMap<(u32, u32), Vec<Body>>,
    lens: HashSet<u32>,
    /// The files (their bytes, and where they are: start and end), by index.
    files: Vec<(Bytes, usize, usize)>,
    /// Where the files are that hold any of the streams (start and end).
    holding: HashSet<(usize, usize)>,
}

impl Bodies {
    /// Of `files` (those of an archive, by index; shared, [`Bytes::share`], as they are kept)
    /// but those in `raw`, which are archived as they are; on up to `threads` threads.
    pub(crate) fn of(files: &[Bytes], raw: &HashSet<usize>, threads: usize) -> Bodies {
        let _s = aqr_common::trace::span("inflatable");
        let found = aqr_common::pool::par_map(files.len(), threads, |k| {
            let d = &files[k][..];
            if raw.contains(&k) || d.len() < MIN {
                return Vec::new();
            }
            match detect(d) {
                Some(Format::Zip) => aqr_zip::formula::deflated(d)
                    .into_iter()
                    .filter(|b| b.1 >= MIN as u64)
                    .map(|(crc, n, r)| (crc, n as u32, r))
                    .collect(),
                Some(Format::Gzip) => {
                    let Some(body) = gzip_body(d) else { return Vec::new() };
                    let t = &d[d.len() - 8..];
                    let word = |p: usize| u32::from_le_bytes(t[p..p + 4].try_into().expect("4 bytes"));
                    vec![(word(0), word(4), body)]
                }
                Some(Format::Zlib) => {
                    let Some(body) = zlib_body(d) else { return Vec::new() };
                    match crate::flate::zlib_data(d, ZLIB_MAX) {
                        Some(data) => vec![(aqr_common::crc32::hash(&data), data.len() as u32, body)],
                        None => Vec::new(),
                    }
                }
                _ => Vec::new(),
            }
        });
        let mut b = Bodies::default();
        for (k, bodies) in found.into_iter().enumerate() {
            for (crc, n, r) in bodies {
                b.lens.insert(n);
                b.holding.insert((files[k].as_ptr() as usize, files[k].as_ptr() as usize + files[k].len()));
                b.by.entry((crc, n)).or_default().push((k, r));
            }
        }
        b.files = files.iter().map(|d| (d.clone(), d.as_ptr() as usize, d.as_ptr() as usize + d.len())).collect();
        b
    }

    /// Whether `d` is all of a file that holds any of the streams: its formula keeps them as
    /// they are, which is taken whether or not it is smaller ([`crate::smaller`]), so that the
    /// files made of them find them.
    pub(crate) fn holds_streams(&self, d: &[u8]) -> bool {
        self.holding.contains(&(d.as_ptr() as usize, d.as_ptr() as usize + d.len()))
    }

    /// The formula of `d` as the data of one of the streams, if it is, from a file other than
    /// the one `d` is in.
    pub(crate) fn formula(&self, d: &[u8]) -> Option<Formula> {
        if d.len() < MIN || !self.lens.contains(&(d.len() as u32)) {
            return None;
        }
        let candidates = self.by.get(&(aqr_common::crc32::hash(d), d.len() as u32))?;
        let at = d.as_ptr() as usize;
        let (k, r) = candidates.iter().find(|(k, r)| {
            let (bytes, start, end) = &self.files[*k];
            // (not within the file that holds it)
            !(*start <= at && at < *end)
                && aqr_flate::inflate::data(&bytes[r.clone()], d.len()).is_ok_and(|data| data == d)
        })?;
        let mut f = Formula::default();
        let raw = f.push(CLASS, self.files[*k].0.slice(r.clone()));
        f.root = aqr_flate::formula::inflated(raw, d.len() as u64);
        Some(f)
    }
}

/// Whether `f` is a formula of [`gzip`] or [`zlib`]: its DEFLATE stream cut off as it is.
pub(crate) fn is(f: &Formula) -> bool {
    f.applies().is_empty() && f.streams.iter().any(|s| s.class == CLASS)
}

/// The class of a DEFLATE stream in a stream of its own: of a formula of [`Bodies::formula`],
/// and of [`gzip`] and [`zlib`] (stored once, the one shared by the other).
pub(crate) const CLASS: &str = "flate/deflated";

/// Where the DEFLATE stream of the gzip file `d` is, taken to be one member (what is not
/// inflates to nothing: [`Bodies::formula`] checks).
fn gzip_body(d: &[u8]) -> Option<Range<usize>> {
    let start = crate::flate::gzip_start(d)?;
    let end = d.len().checked_sub(8)?;
    (start + 2 <= end).then_some(start..end)
}

/// Where the DEFLATE stream of the zlib stream `d` is, taken to end at its checksum, at the end.
fn zlib_body(d: &[u8]) -> Option<Range<usize>> {
    let end = d.len().checked_sub(4)?;
    (2 + 2 <= end && d[1] & 0x20 == 0).then_some(2..end)
}

/// The gzip file `d` as it is, its DEFLATE stream a slice of its own ([`on`]).
pub(crate) fn gzip(d: &[u8]) -> Option<Formula> {
    sliced("gzip", d, gzip_body(d)?)
}

/// The zlib stream `d` as it is, its DEFLATE stream a slice of its own ([`on`]).
pub(crate) fn zlib(d: &[u8]) -> Option<Formula> {
    sliced("zlib", d, zlib_body(d)?)
}

fn sliced(class: &'static str, d: &[u8], body: Range<usize>) -> Option<Formula> {
    // (the stream in a stream of its own: slices of one stream next to each other are one,
    // which would not be found as the stream)
    let mut f = Formula::default();
    let bytes = aqr_formula::store::find(d).unwrap_or_else(|| d.to_vec().into());
    let around = [&d[..body.start], &d[body.end..]].concat();
    let (a, n) = (body.start as u64, around.len() as u64);
    let around = f.push(class, around);
    let Node::Slice { stream, .. } = around else { return None };
    let deflated = f.push(CLASS, bytes.slice(body));
    f.root = Node::concat([Node::slice(stream, 0, a), deflated, Node::slice(stream, a, n - a)]);
    Some(f)
}
