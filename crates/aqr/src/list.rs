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
//! - `-q`: the paths alone, nothing else: of the archive's metadata blocks alone (an encrypted
//!   one's chunks of them decrypted), of paths given those back from the last as far as they
//!   tell them ([`aqr::open_indexed_at`]). Of paths given without `-v`, those and the blocks of
//!   their files alone are read too ([`aqr::Archive::sized`]); `-v` and the whole archive listed
//!   read all of its blocks, which a file's share of the blocks it takes is of.
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
//! `--json`: each archive's listing as a line of JSON instead ([`json`]): its entries with all
//! their metadata (what `-v` tells too, but `-q`), and the counts and totals.
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
    /// What it is (none: the file of an archive of a single file).
    node: Option<EntryNode>,
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
        // (named by its path as given)
        let text = match a.json {
            true => {
                let mut v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                v["archive"] = crate::stdio::shown(input).to_string().into();
                v.to_string() + "\n"
            }
            false => text,
        };
        if several && !a.quiet && !a.json {
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
    let file = fs::File::open(input).map_err(|e| err(&crate::exit::seen(e)))?;
    // SAFETY: the archive is not to change while it is read; where it does, its checksums tell
    let c = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| err(&crate::exit::seen(e)))?;
    // the paths alone, or those of paths given but of their blocks: of its metadata blocks alone
    // (of paths given, those back from the last as far as they tell them) and the blocks of
    // those files, an encrypted archive decrypted as it is read
    let indexed = a.quiet || a.select.is_some() && !a.verbose;
    let decrypting = match indexed && aqr::envelope::is_encrypted(&c) {
        true => Some(crate::unlock(&c, input)?),
        false => None,
    };
    if indexed {
        let payload = match &decrypting {
            Some(d) => aqr::Payload::Sealed(d),
            None => aqr::Payload::Plain(crate::located(&c, input, a.quiet)),
        };
        let paths: Option<Vec<Vec<Vec<u8>>>> = a.select.as_ref().map(|s| s.paths.iter().map(|p| p.0.clone()).collect());
        let upto = match &a.at {
            Some(at) => Some(crate::journal::upto(input, crate::located(&c, input, a.quiet), at, a.quiet)?),
            None => None,
        };
        let archive = crate::by_index(payload, &aqr::NoModules, a.opt.threads, paths.as_deref(), upto)
            .map_err(|e| err(&crate::exit::seen(e)))?;
        if a.quiet {
            return listed(a, &archive, Vec::new(), &err, None);
        }
        crate::tell_incomplete(archive.status(), input);
        // (their sizes and instructions alone, all that is shown without -v)
        let (Some(sel), Some(t)) = (&a.select, archive.tree()) else {
            return Err(err(&"an archive of a single file, not of paths"));
        };
        sel.check(t, &|_| Vec::new()).map_err(|e| err(&crate::exit::seen(e)))?;
        let only = sel.contents(t, archive.contents_len());
        let sized = archive.sized(&only).map_err(|e| err(&crate::exit::seen(e)))?;
        let listing = sized
            .into_iter()
            .map(|s| {
                s.map(|(size, nodes)| aqr::Listed {
                    size,
                    nodes,
                    data: 0,
                    overhead: 0,
                    share: 0,
                    formula: String::new(),
                    segment: 0,
                })
            })
            .collect();
        return listed(a, &archive, listing, &err, None);
    }
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
        None => {
            let bytes = crate::located(&c, input, a.quiet);
            match &a.at {
                Some(at) => aqr::open_upto(bytes, &aqr::NoModules, crate::journal::upto(input, bytes, at, a.quiet)?),
                None => aqr::open(bytes, &aqr::NoModules),
            }
        }
    }
    .map_err(|e| err(&crate::exit::seen(e)))?;
    if !a.quiet {
        crate::tell_incomplete(archive.status(), input);
    }
    let name = |wasm: &[u8]| aqr_decompress::identify(wasm).map(|n| n.split('@').next().unwrap_or(&n).to_owned());
    let listing = archive.listing(&name).map_err(|e| err(&crate::exit::seen(e)))?.into_iter().map(Some).collect();
    listed(a, &archive, listing, &err, Some(c.len() as u64))
}

/// The listing of `archive` (of `len` bytes: none, of the paths alone), the files `listed` (by
/// content; none, of the paths alone).
fn listed(
    a: &Args,
    archive: &aqr::Archive,
    mut listed: Vec<Option<aqr::Listed>>,
    err: &dyn Fn(&dyn std::fmt::Display) -> String,
    len: Option<u64>,
) -> Result<String, String> {
    let mut rows = Vec::new();
    match archive.tree() {
        None if a.select.is_some() => return Err(err(&"an archive of a single file, not of paths")),
        None => {
            let l = listed.get_mut(0).and_then(Option::take);
            let size = l.as_ref().map_or_else(String::new, |l| size_shown(a, l.size));
            rows.push(Row {
                path: b"<Unnamed>".to_vec(),
                size,
                listed: l,
                extra: Extra::default(),
                is_dir: false,
                link: None,
                node: None,
            });
        }
        Some(t) => {
            let picked = match &a.select {
                Some(sel) => {
                    sel.check(t, &|_| Vec::new()).map_err(|e| err(&crate::exit::seen(e)))?;
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
                    node: Some(e.node.clone()),
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
    if a.json {
        return json(a, archive, &rows, len, err);
    }
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
                    (l.segment + 1).to_string(),
                    format!("{} [{}]", l.formula, l.nodes),
                ),
                None => ("-".into(), "-".into(), "-".into(), "-".into(), "-".into()),
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
                let (data, len) = (archive.data_bytes().map_err(|e| err(&crate::exit::seen(e)))?, len.unwrap_or(0));
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

/// The listing of `archive` (of `len` bytes, where known) as a line of JSON, of its `rows`:
/// `{"archive" (set by [`list_all`]), "entries": [...], "files", "directories", "links", "others", "size"}`, and the
/// archive's `"data"` and `"overhead"` bytes where they are known. Each entry has its `"path"`
/// (`"path_hex"` too where it is not UTF-8; null of an archive of a single file), its `"type"` (`file`, `dir`, `link`, `fifo`, `chr`,
/// `blk`, `special`) and what is known of it: of a file, `"size"` and `"nodes"` (with all of
/// it read, `"data"`, `"overhead"`, `"share"`, `"segment"` and `"formula"` too); of a link,
/// `"target"` and `"junction"`; of a device, `"major"` and `"minor"`; and its metadata (times
/// in RFC 3339, without an offset where it is a local time of an unknown time zone).
fn json(
    a: &Args,
    archive: &aqr::Archive,
    rows: &[Row],
    len: Option<u64>,
    err: &dyn Fn(&dyn std::fmt::Display) -> String,
) -> Result<String, String> {
    use serde_json::{Map, Value, json};
    let text = |b: &[u8]| Value::from(String::from_utf8_lossy(b).into_owned());
    let mut entries = Vec::with_capacity(rows.len());
    let (mut files, mut dirs, mut links, mut others, mut total) = (0, 0, 0, 0, 0u64);
    for r in rows {
        let mut e = Map::new();
        // (none of the file of an archive of a single file)
        e.insert("path".into(), if r.node.is_some() { text(&r.path) } else { Value::Null });
        if r.node.is_some() && std::str::from_utf8(&r.path).is_err() {
            e.insert("path_hex".into(), r.path.iter().map(|b| format!("{b:02x}")).collect::<String>().into());
        }
        let kind = match &r.node {
            None | Some(EntryNode::File(_)) => "file",
            Some(EntryNode::Dir) => "dir",
            Some(EntryNode::Link(_)) => "link",
            Some(EntryNode::Fifo) => "fifo",
            Some(EntryNode::Device { block: true, .. }) => "blk",
            Some(EntryNode::Device { block: false, .. }) => "chr",
            Some(EntryNode::Special(_)) => "special",
        };
        match kind {
            "file" => files += 1,
            "dir" => dirs += 1,
            "link" => links += 1,
            _ => others += 1,
        }
        e.insert("type".into(), kind.into());
        if let Some(l) = &r.listed {
            total += l.size;
            e.insert("size".into(), l.size.into());
            e.insert("nodes".into(), l.nodes.into());
            if a.verbose || a.select.is_none() {
                e.insert("data".into(), l.data.into());
                e.insert("overhead".into(), l.overhead.into());
                e.insert("share".into(), l.share.into());
                e.insert("segment".into(), (l.segment + 1).into());
                e.insert("formula".into(), l.formula.clone().into());
            }
        }
        if let Some((to, junction)) = &r.link {
            e.insert("target".into(), text(to));
            e.insert("junction".into(), (*junction).into());
        }
        match r.node {
            Some(EntryNode::Device { major, minor, .. }) => {
                e.insert("major".into(), major.into());
                e.insert("minor".into(), minor.into());
            }
            Some(EntryNode::Special(k)) => _ = e.insert("kind".into(), k.into()),
            _ => {}
        }
        let x = &r.extra;
        for (name, t) in
            [("mtime", x.mtime), ("ctime", x.ctime), ("crtime", x.crtime), ("atime", x.atime), ("arctime", x.arctime)]
        {
            if let Some(t) = t {
                e.insert(name.into(), rfc3339(t).into());
            }
        }
        let mut num = |name: &str, v: Option<u64>| _ = v.map(|v| e.insert(name.into(), v.into()));
        num("mode", x.mode.map(u64::from));
        num("uid", x.uid);
        num("gid", x.gid);
        num("win_attrs", x.win_attrs.map(u64::from));
        num("mac_flags", x.mac_flags.map(u64::from));
        if let Some(u) = &x.uname {
            e.insert("user".into(), text(u));
        }
        if let Some(g) = &x.gname {
            e.insert("group".into(), text(g));
        }
        if let Some(g) = x.link_group {
            e.insert("link_group".into(), i64::try_from(g).map_or_else(|_| g.to_string().into(), Value::from));
        }
        if !x.xattrs.is_empty() {
            e.insert("xattrs".into(), x.xattrs.iter().map(|(n, _)| text(n)).collect());
        }
        if !x.forks.is_empty() {
            e.insert("forks".into(), x.forks.iter().map(|(n, _)| text(name_bytes(n).0)).collect());
        }
        entries.push(Value::Object(e));
    }
    let mut o = json!({
        "entries": entries,
        "files": files,
        "directories": dirs,
        "links": links,
        "others": others,
    });
    if !a.quiet {
        o["size"] = total.into();
    }
    if a.select.is_none()
        && let Some(len) = len
    {
        let data = archive.data_bytes().map_err(|e| err(&crate::exit::seen(e)))?;
        o["data"] = data.into();
        o["overhead"] = (len - data.min(len)).into();
    }
    Ok(o.to_string() + "\n")
}

/// A time as RFC 3339 has it: of its offset (`Z` of none told), none of a local time in an
/// unknown time zone; fractions of a second where it has them.
fn rfc3339(t: Time) -> String {
    let (y, mo, d, h, mi, sec) = t.civil();
    let frac = match t.nanos {
        0 => String::new(),
        n => format!(".{n:09}").trim_end_matches('0').to_owned(),
    };
    let zone = match t.zone {
        Zone::Utc => "Z".into(),
        Zone::Offset(o) => {
            let m = (o as i32).abs() * 15;
            format!("{}{:02}:{:02}", if o < 0 { '-' } else { '+' }, m / 60, m % 60)
        }
        Zone::Local => String::new(),
    };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{sec:02}{frac}{zone}")
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
