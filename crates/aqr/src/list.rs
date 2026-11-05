//! `aqr -t`: the entries of Aqueous archives listed on standard output, without decoding
//! their files (but for their formula blocks, read to tell how each file is made); with paths
//! of the archive given ([`crate::extract::Selection`]), the entries at them and under them alone.
//!
//! Entries are listed by their paths (names in turn, as bytes), a directory's ending with `/`;
//! an archive of a single file, which has no metadata, as one entry `<Unnamed>`. In place of a
//! size, other than a file has its kind: `<DIR>`, `<LINK>` (its target on the next line, as
//! `  => SYMLINK: TARGET`, or `JUNCTION`), `<FIFO>`, `<CHR M,N>` and `<BLK M,N>` (devices of their
//! numbers), `<SPECIAL K>` (a kind not known).
//!
//! - `-q`: the paths alone, nothing else.
//! - By default: the size, the modification time, the flags (`x` executable, `r` read-only,
//!   `h` hidden, `@` extended attributes, `+` forks, `=` a hard link) and the instructions of
//!   the formula making the file (`[N]`) of each, and the total.
//! - `-v`: the bytes of the data blocks a file is made of and of the other blocks it takes
//!   (its formula block, modules, resources and dictionaries; blocks shared by several files
//!   counted for each of them), its size, the ratio of its share of those blocks to its size (a
//!   block taken by several files shared among them by their sizes, so that files taking the
//!   same blocks have the same ratio: [`aqr::Listed::share`]), its segment (from 1), its
//!   owner, permissions (`rwxr-xr-x`), Windows' attributes (`RHSATI`), macOS' flags (as
//!   `chflags` names them), the flags `@+=`, its modification time, and the formula summarized
//!   ([`aqr::Listed::formula`]); and the total of the archive's data blocks and the rest.
//!
//! A time is told as it is recorded ([`aqr::Zone`]): `Z` of no time zone told, its offset where
//! one is, none of a local time in an unknown time zone; the digits of a quantization interval
//! over a second `?` (`??:??:??` of a date).

use std::fs;
use std::io::Write;
use std::path::Path;

use aqr_formula::aqr::{self, EntryNode, Extra, Time, Zone, name_bytes};

use crate::{Args, decrypt, size};

/// What `-t` lists of each entry.
struct Row {
    path: Vec<u8>,
    /// The size or the kind, as shown.
    size: String,
    listed: Option<aqr::Listed>,
    extra: Extra,
    is_dir: bool,
    /// A link's target, and whether it is a junction.
    link: Option<(Vec<u8>, bool)>,
}

pub fn list_all(a: &Args) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    let several = a.inputs.len() > 1;
    for (i, input) in a.inputs.iter().enumerate() {
        let name = input.file_name().map(crate::meta::os_bytes).unwrap_or_default();
        if aqr_unpack::by_name(&name).is_some() {
            return Err(format!("{}: only Aqueous archives are listed", input.display()));
        }
        let text = list(a, input)?;
        if several && !a.quiet {
            if i > 0 {
                writeln!(out).map_err(|e| e.to_string())?;
            }
            writeln!(out, "{}:", input.display()).map_err(|e| e.to_string())?;
        }
        out.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    }
    out.flush().map_err(|e| e.to_string())
}

/// The listing of archive `input`.
fn list(a: &Args, input: &Path) -> Result<String, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", input.display());
    let file = fs::File::open(input).map_err(|e| err(&e))?;
    // SAFETY: the archive is not to change while it is read; where it does, its checksums tell
    let c = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| err(&e))?;
    let decrypted = match aqr::envelope::is_encrypted(&c) {
        // (not listed from chunks that do not authenticate, whose bytes are not to be trusted:
        // decompressing or testing repairs it)
        true => match decrypt(&c, input, a.opt.threads)? {
            (d, erased) if erased.is_empty() => Some(d),
            _ => return Err(err(&"encrypted chunks not authenticating (-K repairs what it can)")),
        },
        false => None,
    };
    // (no module is run)
    let archive = match &decrypted {
        Some(d) => aqr::open_payload(d, &aqr::NoModules),
        None => aqr::open(crate::located(&c, input, a.quiet), &aqr::NoModules),
    }
    .map_err(|e| err(&e))?;
    if !a.quiet {
        crate::tell_incomplete(archive.status(), input);
    }
    let name = |wasm: &[u8]| aqr_decompress::identify(wasm).map(|n| n.split('@').next().unwrap_or(&n).to_owned());
    let mut listed: Vec<Option<aqr::Listed>> =
        archive.listing(&name).map_err(|e| err(&e))?.into_iter().map(Some).collect();
    let mut rows = Vec::new();
    match archive.tree() {
        None if a.select.is_some() => return Err(err(&"an archive of a single file, not of paths")),
        None => {
            let l = listed[0].take().unwrap();
            let size = size_shown(a, l.size);
            rows.push(Row {
                path: b"<Unnamed>".to_vec(),
                size,
                listed: Some(l),
                extra: Extra::default(),
                is_dir: false,
                link: None,
            });
        }
        Some(t) => {
            let picked = match &a.select {
                Some(sel) => {
                    sel.check(t, &|_| Vec::new()).map_err(|e| err(&e))?;
                    Some(sel.picked(t))
                }
                None => None,
            };
            let mut paths: Vec<Vec<u8>> = Vec::with_capacity(t.entries.len());
            for (i, e) in t.entries.iter().enumerate() {
                let name = name_bytes(&e.name).0;
                let path = match e.parent {
                    Some(p) => [&paths[p][..], b"/", name].concat(),
                    None => name.to_vec(),
                };
                if picked.as_ref().is_some_and(|p| !p[i]) {
                    paths.push(path);
                    continue;
                }
                let (size, listed, link) = match &e.node {
                    EntryNode::Dir => ("<DIR>".into(), None, None),
                    &EntryNode::File(c) => {
                        let l = listed.get(c).and_then(|l| l.clone());
                        (l.as_ref().map_or_else(String::new, |l| size_shown(a, l.size)), l, None)
                    }
                    EntryNode::Link(to) => {
                        ("<LINK>".into(), None, Some((name_bytes(to).0.to_vec(), e.extra.win_link_type == Some(2))))
                    }
                    EntryNode::Fifo => ("<FIFO>".into(), None, None),
                    &EntryNode::Device { block, major, minor } => {
                        (format!("<{} {major},{minor}>", if block { "BLK" } else { "CHR" }), None, None)
                    }
                    EntryNode::Special(k) => (format!("<SPECIAL {k}>"), None, None),
                };
                rows.push(Row {
                    path: path.clone(),
                    size,
                    listed,
                    extra: e.extra.clone(),
                    is_dir: e.node == EntryNode::Dir,
                    link,
                });
                paths.push(path);
            }
            listed.clear();
        }
    }
    // by their paths, a name at a time
    rows.sort_by(|x, y| x.path.split(|&b| b == b'/').cmp(y.path.split(|&b| b == b'/')));
    let shown = |r: &Row| {
        let p = String::from_utf8_lossy(&r.path).into_owned();
        if r.is_dir { p + "/" } else { p }
    };
    let mut s = String::new();
    if a.quiet {
        for r in &rows {
            s += &shown(r);
            s.push('\n');
        }
        return Ok(s);
    }
    let files = rows.iter().filter(|r| r.listed.is_some()).count();
    let dirs = rows.iter().filter(|r| r.is_dir).count();
    let links = rows.iter().filter(|r| r.link.is_some()).count();
    let others = rows.len() - files - dirs - links;
    let total: u64 = rows.iter().filter_map(|r| r.listed.as_ref()).map(|l| l.size).sum();
    let mut counts = format!("{files} file{}", if files == 1 { "" } else { "s" });
    if dirs > 0 {
        counts += &format!(", {dirs} director{}", if dirs == 1 { "y" } else { "ies" });
    }
    if links > 0 {
        counts += &format!(", {links} link{}", if links == 1 { "" } else { "s" });
    }
    if others > 0 {
        counts += &format!(", {others} other{}", if others == 1 { "" } else { "s" });
    }
    let mut table: Vec<Vec<String>> = Vec::new();
    // (columns aligned to the right, the others to the left; the last not padded)
    let right: &[bool];
    if a.verbose {
        right = &[true, true, true, true, true, false, false, false, false, false, false, false, false];
        table.push(
            [
                "Data", "Overhead", "Size", "Ratio", "Seg", "Owner", "Mode", "Win", "Mac", "Flags", "Modified",
                "Formula", "Name",
            ]
            .map(String::from)
            .to_vec(),
        );
        for r in &rows {
            let (data, overhead, ratio, seg, formula) = match &r.listed {
                Some(l) => (
                    l.data.to_string(),
                    l.overhead.to_string(),
                    ratio(l.share, l.size),
                    "1".into(),
                    format!("{} [{}]", l.formula, l.nodes),
                ),
                None => ("-".into(), "-".into(), "-".into(), "1".into(), "-".into()),
            };
            table.push(vec![
                data,
                overhead,
                r.size.clone(),
                ratio,
                seg,
                owner(&r.extra),
                mode(&r.extra),
                win(&r.extra),
                mac(&r.extra),
                flags(&r.extra, None),
                time(r.extra.mtime),
                formula,
                shown(r),
            ]);
        }
        // (of paths selected, the files' shares of the archive; else the archive's bytes)
        let (data, overhead, whole) = match a.select {
            Some(_) => ("-".into(), "-".into(), rows.iter().filter_map(|r| r.listed.as_ref()).map(|l| l.share).sum()),
            None => {
                let (data, len) = (archive.data_bytes(), c.len() as u64);
                (data.to_string(), (len - data.min(len)).to_string(), len)
            }
        };
        table.push(vec![
            data,
            overhead,
            total.to_string(),
            ratio(whole, total),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            counts,
        ]);
    } else {
        right = &[true, false, false, true, false];
        table.push(["Size", "Modified", "Flags", "", "Name"].map(String::from).to_vec());
        for r in &rows {
            let nodes = r.listed.as_ref().map_or_else(String::new, |l| format!("[{}]", l.nodes));
            table.push(vec![
                r.size.clone(),
                time(r.extra.mtime),
                flags(&r.extra, Some(r.listed.is_some())),
                nodes,
                shown(r),
            ]);
        }
        table.push(vec![size_shown(a, total), String::new(), String::new(), String::new(), counts]);
    }
    let cols = table[0].len();
    let width: Vec<usize> =
        (0..cols).map(|k| table.iter().map(|row| row[k].chars().count()).max().unwrap_or(0)).collect();
    let line = |s: &mut String, row: &[String]| {
        let mut l = String::new();
        for (k, cell) in row.iter().enumerate() {
            if k > 0 {
                l += "  ";
            }
            let pad = " ".repeat(width[k] - cell.chars().count());
            match (right[k], k + 1 == cols) {
                (true, _) => {
                    l += &pad;
                    l += cell;
                }
                (false, false) => {
                    l += cell;
                    l += &pad;
                }
                (false, true) => l += cell,
            }
        }
        *s += l.trim_end();
        s.push('\n');
    };
    let indent = width[..cols - 1].iter().map(|w| w + 2).sum::<usize>();
    line(&mut s, &table[0]);
    for (r, row) in rows.iter().zip(&table[1..]) {
        line(&mut s, row);
        if let Some((to, junction)) = &r.link {
            let kind = if *junction { "JUNCTION" } else { "SYMLINK" };
            s += &format!("{}  => {kind}: {}\n", " ".repeat(indent), String::from_utf8_lossy(to));
        }
    }
    s += &"-".repeat(indent + width[cols - 1]);
    s.push('\n');
    line(&mut s, table.last().unwrap());
    Ok(s)
}

/// A size as `-v` shows it (in bytes), or as `-t` alone does.
fn size_shown(a: &Args, n: u64) -> String {
    if a.verbose { n.to_string() } else { size(n as f64) }
}

fn ratio(a: u64, of: u64) -> String {
    if of == 0 { "-".into() } else { format!("{:.1}%", 100.0 * a as f64 / of as f64) }
}

/// The modification time as recorded ([`Time::civil`]), with `?` for the digits finer than its
/// quantization interval where it is over a second.
fn time(t: Option<Time>) -> String {
    let Some(t) = t else { return "-".into() };
    let (y, mo, d, h, mi, sec) = t.civil();
    let clock = match t.unit {
        u if u >= 86_400_000_000_000 => "??:??:??".into(),
        u if u > 1_000_000_000 => format!("{h:02}:{mi:02}:{}?", sec / 10),
        _ => format!("{h:02}:{mi:02}:{sec:02}"),
    };
    let zone = match t.zone {
        Zone::Utc => "Z".into(),
        Zone::Offset(o) => {
            let m = (o as i32).abs() * 15;
            format!("{}{:02}:{:02}", if o < 0 { '-' } else { '+' }, m / 60, m % 60)
        }
        Zone::Local => String::new(),
    };
    format!("{y:04}-{mo:02}-{d:02} {clock}{zone}")
}

/// `@` (extended attributes), `+` (forks) and `=` (a hard link), `-` where not; with `x`
/// (executable), `r` (read-only) and `h` (hidden) before them by default (of a file or not).
fn flags(x: &Extra, file: Option<bool>) -> String {
    let f = |on: bool, c: char| if on { c } else { '-' };
    let mut s = String::new();
    if let Some(file) = file {
        let exec = file && x.mode.is_some_and(|m| m & 0o111 != 0);
        let ro = x.win_attrs.is_some_and(|w| w & 0x1 != 0) || x.mode.is_some_and(|m| m & 0o222 == 0);
        let hidden = x.win_attrs.is_some_and(|w| w & 0x2 != 0) || x.mac_flags.is_some_and(|m| m & 0x8000 != 0);
        s.extend([f(exec, 'x'), f(ro, 'r'), f(hidden, 'h')]);
    }
    s.extend([f(!x.xattrs.is_empty(), '@'), f(!x.forks.is_empty(), '+'), f(x.link_group.is_some(), '=')]);
    s
}

/// `USER:GROUP`, by names where they are kept, else by ids; `-` where neither is.
fn owner(x: &Extra) -> String {
    let one = |name: &Option<std::sync::Arc<[u8]>>, id: Option<u64>| match (name, id) {
        (Some(n), _) => Some(String::from_utf8_lossy(n).into_owned()),
        (None, Some(id)) => Some(id.to_string()),
        (None, None) => None,
    };
    match (one(&x.uname, x.uid), one(&x.gname, x.gid)) {
        (None, None) => "-".into(),
        (u, g) => format!("{}:{}", u.as_deref().unwrap_or("?"), g.as_deref().unwrap_or("?")),
    }
}

/// The permissions as `ls -l` shows them (`rwxr-xr-x`, `s`, `S`, `t` and `T` too).
pub(crate) fn mode(x: &Extra) -> String {
    let Some(m) = x.mode else { return "-".into() };
    let mut s = String::new();
    for (k, special, c) in [(6, 0o4000, 's'), (3, 0o2000, 's'), (0, 0o1000, 't')] {
        let bits = m >> k & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(match (bits & 1 != 0, m & special != 0) {
            (true, true) => c,
            (false, true) => c.to_ascii_uppercase(),
            (true, false) => 'x',
            (false, false) => '-',
        });
    }
    s
}

/// Windows' attributes, `RHSATI`, `-` for those not set; `-` alone where none are kept.
pub(crate) fn win(x: &Extra) -> String {
    let Some(w) = x.win_attrs else { return "-".into() };
    [(0x1, 'R'), (0x2, 'H'), (0x4, 'S'), (0x20, 'A'), (0x100, 'T'), (0x2000, 'I')]
        .iter()
        .map(|&(b, c)| if w & b != 0 { c } else { '-' })
        .collect()
}

/// macOS' flags, as `chflags` names them, between commas; `-` where none are.
pub(crate) fn mac(x: &Extra) -> String {
    const NAMES: [(u32, &str); 9] = [
        (0x1, "nodump"),
        (0x2, "uchg"),
        (0x4, "uappnd"),
        (0x8, "opaque"),
        (0x8000, "hidden"),
        (0x1_0000, "arch"),
        (0x2_0000, "schg"),
        (0x4_0000, "sappnd"),
        (0x10_0000, "sunlnk"),
    ];
    let f = x.mac_flags.unwrap_or(0);
    let names: Vec<&str> = NAMES.iter().filter(|&&(b, _)| f & b != 0).map(|&(_, n)| n).collect();
    if names.is_empty() { "-".into() } else { names.join(",") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        let t = Time::new(1_798_675_200, 0);
        assert_eq!(time(Some(t)), "2026-12-31 00:00:00Z");
        assert_eq!(time(Some(Time { zone: Zone::Offset(36), ..t })), "2026-12-31 09:00:00+09:00");
        assert_eq!(time(Some(Time { zone: Zone::Offset(-14), ..t })), "2026-12-30 20:30:00-03:30");
        assert_eq!(time(Some(Time { zone: Zone::Local, unit: 86_400_000_000_000, ..t })), "2026-12-31 ??:??:??");
        let two = Time { secs: t.secs + 11, unit: 2_000_000_000, ..t };
        assert_eq!(time(Some(two)), "2026-12-31 00:00:1?Z");
        assert_eq!(time(None), "-");
    }

    #[test]
    fn permissions_and_flags() {
        let x = Extra { mode: Some(0o4755), ..Extra::default() };
        assert_eq!(mode(&x), "rwsr-xr-x");
        assert_eq!(mode(&Extra { mode: Some(0o1644), ..x.clone() }), "rw-r--r-T");
        assert_eq!(flags(&x, Some(true)), "x-----");
        assert_eq!(flags(&x, Some(false)), "------");
        assert_eq!(flags(&Extra { link_group: Some(3), win_attrs: Some(0x3), ..Extra::default() }, None), "--=");
        assert_eq!(win(&Extra { win_attrs: Some(0x21), ..Extra::default() }), "R--A--");
        assert_eq!(mac(&Extra { mac_flags: Some(0x8002), ..Extra::default() }), "uchg,hidden");
        assert_eq!(owner(&Extra { uid: Some(501), gname: Some(b"staff"[..].into()), ..Extra::default() }), "501:staff");
    }
}
