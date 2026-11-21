//! Electron's asar archives: `slack`, the bytes of the data that no file of the header takes
//! zeroed in place. Electron reads a file by its offset and size in the header (`files`, of
//! directories nested; a file `unpacked` is beside the archive, a link has no bytes): the bytes
//! between and after them are never read (tools that take files out of the header leave them:
//! one archive of aqr-asar's corpus has 52 MB of them). The header (its pickles, the JSON and the
//! padding after it) is kept as it is.

use std::ops::Range;

use aqr_asar::json::{self, Kind, Value};

use crate::Options;

/// The bytes of the data that the files of the header at `text` take, from `start` (of the
/// archive), to `out`; why the header is not taken (a size or an offset not a whole number, past
/// `len`).
fn take(text: &[u8], files: &Value, start: usize, len: usize, out: &mut Vec<Range<usize>>) -> Result<(), String> {
    let Kind::Object(members) = &files.kind else { return Err("files not an object".into()) };
    for (_, e) in members {
        if !matches!(e.kind, Kind::Object(_)) {
            return Err("an entry not an object".into());
        }
        if let Some(sub) = e.get(text, b"files") {
            take(text, sub, start, len, out)?;
            continue;
        }
        if e.get(text, b"link").is_some() {
            continue;
        }
        if e.get(text, b"unpacked").is_some_and(|u| &text[u.span.clone()] == b"true") {
            continue;
        }
        let number = |v: Option<&Value>, quoted: bool| -> Option<usize> {
            let v = v?;
            let r = if quoted { v.inner()? } else { matches!(v.kind, Kind::Number).then(|| v.span.clone())? };
            let s = std::str::from_utf8(&text[r]).ok()?;
            s.bytes().all(|b| b.is_ascii_digit()).then(|| s.parse().ok())?
        };
        let size = number(e.get(text, b"size"), false).ok_or("a size not a whole number")?;
        let offset = number(e.get(text, b"offset"), true).ok_or("an offset not a whole number")?;
        let at = start.checked_add(offset).filter(|&a| a.checked_add(size).is_some_and(|e| e <= len));
        let at = at.ok_or("a file past the end")?;
        out.push(at..at + size);
    }
    Ok(())
}

/// `d` with the bytes of its data that no file takes zeroed, None where there are none (or all
/// are zeros); why it is kept where its header is not read whole.
pub(super) fn slack(d: &[u8], _: &Options) -> Result<Option<Vec<u8>>, String> {
    let (span, start) = aqr_asar::header(d).ok_or("not an asar header")?;
    let text = &d[span];
    let root = json::parse(text).ok_or("a header not of JSON")?;
    let files = root.get(text, b"files").ok_or("a header of no files")?;
    let mut live = Vec::new();
    take(text, files, start, d.len(), &mut live)?;
    live.sort_unstable_by_key(|r| r.start);
    let mut r: Option<Vec<u8>> = None;
    let mut at = start;
    for l in live.iter().cloned().chain(std::iter::once(d.len()..d.len())) {
        if l.start > at && d[at..l.start].iter().any(|&b| b != 0) {
            r.get_or_insert_with(|| d.to_vec())[at..l.start].fill(0);
        }
        at = at.max(l.end);
    }
    Ok(r)
}
