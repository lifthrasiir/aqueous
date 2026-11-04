//! `aqr -I` (`--info`): all that an Aqueous archive records, for debugging and forensics
//! ([`aqr::inspect`]), on standard output. Nothing is decoded but the formula and metadata
//! blocks.
//!
//! - `-I ARCHIVE`: the archive itself: its encryption (where it is encrypted), its kind and
//!   footer, every block (where it is, its type and bytes, its segment), and each segment: its
//!   metadata block, its module, resource and dictionary blocks, how many entries of each kind it
//!   has, the archive-wide members of its optional metadata, the root directory's extra, and
//!   what does not hold of its references. The last segment is the archive as it is now
//!   (`[current]`). An archive of a single file (of no metadata block) has its file told too:
//!   the formula of each output of each leaf block, the file all of them one after another.
//! - `-I ARCHIVE PATH...`: each path's entry as each segment has it (`segment #k`; an
//!   archive of several segments has its entries before an update too), the one the archive has
//!   as it is now `[current]` (the spec's merging of segments, "Merging segments"), a tombstone
//!   `[removed]`: what it is, where its content is, every member of its entry extra (all its
//!   timestamps among them), its forks, and the formula of its content and of each fork in full,
//!   a node a line, indented by its depth. `.` or `/` is the root directory.
//!
//! Members of the entry extras that this crate knows are told as they read (times to their
//! nanoseconds and time zones, permissions, attributes, names); members it does not know, as
//! CBOR in its diagnostic notation, their strings cut at 64 bytes but with `-v`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::Path;

use aqr_formula::aqr::inspect::{self, Content, Entry, Inspection, Node};
use aqr_formula::aqr::{self, Extra, Member, Time, Zone, entry_key_name, name_bytes, name_from};

use crate::Args;

pub fn info(a: &Args) -> Result<(), String> {
    let input = &a.inputs[0];
    let text = info_of(a, input)?;
    let mut out = std::io::stdout().lock();
    out.write_all(text.as_bytes()).and_then(|_| out.flush()).map_err(|e| e.to_string())
}

fn info_of(a: &Args, input: &Path) -> Result<String, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", input.display());
    let name = input.file_name().map(crate::meta::os_bytes).unwrap_or_default();
    if aqr_unpack::by_name(&name).is_some() {
        return Err(err(&"only Aqueous archives are told of"));
    }
    let file = fs::File::open(input).map_err(|e| err(&e))?;
    // SAFETY: the archive is not to change while it is read; where it does, its checksums tell
    let c = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| err(&e))?;
    let mut s = String::new();
    let paths = &a.inputs[1..];
    if paths.is_empty() {
        let _ = writeln!(s, "{}", input.display());
        let _ = writeln!(s, "  file: {} B", c.len());
    }
    let encrypted = aqr::envelope::is_encrypted(&c) || aqr::envelope::is_damaged_encrypted(&c);
    let decrypted = match encrypted {
        true => {
            if paths.is_empty() {
                let (e, opened) = aqr::envelope::open_damaged(&c).map_err(|e| err(&e))?;
                let (salt, nonce) = e.salt_and_nonce();
                let mirrored = c.starts_with(&aqr::envelope::ENCRYPTED_MIRRORED_HEADER_MAGIC);
                let _ = writeln!(
                    s,
                    "  encrypted: scrypt of work factor {}, salt {}; {} chunk{}, nonce {}{}",
                    e.work_factor(),
                    hex(salt),
                    e.chunks(),
                    plural(e.chunks()),
                    hex(nonce),
                    if mirrored { "; headers mirrored" } else { "" },
                );
                if let Some(k) = opened.header_from {
                    let _ = writeln!(s, "  its header damaged: taken from mirrored header {k}");
                }
                if opened.cut {
                    let _ = writeln!(s, "  cut short in its last chunk");
                }
            }
            // (not told from chunks that do not authenticate, whose bytes are not to be trusted)
            match crate::decrypt(&c, input, a.opt.threads)? {
                (d, erased) if erased.is_empty() => Some(d),
                _ => return Err(err(&"encrypted chunks not authenticating (-K repairs what it can)")),
            }
        }
        false => None,
    };
    let payload: &[u8] = decrypted.as_deref().unwrap_or(&c);
    let cut = if a.verbose { None } else { Some(64) };
    let i = inspect::inspect(payload, encrypted, &aqr_decompress::identify, cut).map_err(|e| err(&e))?;
    if paths.is_empty() {
        if encrypted {
            let _ = writeln!(s, "  payload: {} B", payload.len());
        }
        archive(&mut s, &i).map_err(|e| err(&e))?;
    } else {
        if i.segments[0].metadata.is_none() {
            return Err(err(&"an archive of a single file, not of paths"));
        }
        for (k, p) in paths.iter().enumerate() {
            if k > 0 {
                s.push('\n');
            }
            path(&mut s, &i, p).map_err(|e| err(&e))?;
        }
    }
    Ok(s)
}

/// The archive itself, of `-I ARCHIVE`.
fn archive(s: &mut String, i: &Inspection) -> aqr::Decoded<()> {
    let _ = writeln!(s, "  kind: {} archive", if i.overlay { "overlay" } else { "standalone" });
    let last = i.segments.last().and_then(|g| g.metadata.as_ref());
    let _ =
        write!(s, "  footer: {} B, size hint {}, metadata backward offset {}", i.footer, i.size_hint, i.metadata_back);
    match last {
        Some(m) => _ = writeln!(s, " (to the metadata block at {})", m.at),
        None => _ = writeln!(s, " (no metadata block)"),
    }
    let _ = writeln!(s, "  segments: {}", i.segments.len());
    let _ = writeln!(s, "\nblocks ({}):", i.blocks.len());
    let width = i.blocks.last().map_or(1, |b| b.at.to_string().len());
    for (k, b) in i.blocks.iter().enumerate() {
        let seg = b.segment.map_or_else(|| "-".into(), |g| format!("segment #{}", g + 1));
        let _ = writeln!(
            s,
            "  #{:<5} at {:>width$}  type {:#04x}  {:>9} B, data {:>9} B  {seg:<11}  {}",
            k + 1,
            b.at,
            b.ty,
            b.size,
            b.data,
            b.kind
        );
    }
    let n = i.segments.len();
    for (k, g) in i.segments.iter().enumerate() {
        let current = if k + 1 == n { " [current]" } else { "" };
        let _ = writeln!(s, "\nsegment #{}{current}:", k + 1);
        let (first, end) = (&i.blocks[g.blocks.start], &i.blocks[g.blocks.end - 1]);
        let _ = writeln!(
            s,
            "  blocks #{}-#{}, at {}-{}",
            g.blocks.start + 1,
            g.blocks.end,
            first.at,
            end.at + end.size as usize
        );
        if let Some(m) = &g.metadata {
            let how = if m.zstd { "Zstandard data" } else { "stored" };
            let _ = writeln!(
                s,
                "  metadata block at {}: {} B decoded ({how}), of optional metadata {} B",
                m.at, m.data, m.optional
            );
            let to = match (m.prev_at, k) {
                (Some(_), 0) => "to the header".into(),
                (Some(at), _) => format!("to the metadata block at {at}"),
                (None, _) => "to nowhere".into(),
            };
            let _ = writeln!(s, "  previous metadata offset: {} ({to})", m.prev);
            let tombstones = g.entries.iter().filter(|e| e.node == Node::Tombstone).count();
            let [dirs, files, specials] = m.counts;
            let _ = write!(
                s,
                "  entries: {dirs} director{}, {files} file{}, {specials} special{}",
                if dirs == 1 { "y" } else { "ies" },
                plural(files),
                plural(specials)
            );
            if tombstones > 0 {
                let _ = write!(s, " ({tombstones} tombstone{})", plural(tombstones));
            }
            s.push('\n');
        }
        let aux = |s: &mut String, what: &str, list: &[inspect::Aux]| {
            if list.is_empty() {
                return;
            }
            let _ = writeln!(s, "  {what}:");
            for (k, x) in list.iter().enumerate() {
                let _ = write!(s, "    #{} at {}: {} B", k + 1, x.at, x.size);
                if let Some(d) = x.decoded {
                    let _ = write!(s, ", {d} B decoded");
                }
                if let Some(n) = &x.name {
                    let _ = write!(s, ", {n}");
                }
                s.push('\n');
            }
        };
        aux(s, "modules", &g.modules);
        aux(s, "resources", &g.resources);
        aux(s, "dictionaries", &g.dictionaries);
        let formulas = g.leaves.iter().filter(|l| l.formula).count();
        let _ = writeln!(
            s,
            "  leaf blocks: {} ({formulas} formula, {} data); non-leaf blocks: {}",
            g.leaves.len(),
            g.leaves.len() - formulas,
            g.non_leaf.len()
        );
        if !g.archive_extra.is_empty() {
            let _ = writeln!(s, "  archive extra:");
            for m in &g.archive_extra {
                let _ = writeln!(s, "    {}", archive_member(m));
            }
        }
        if let Some(root) = g.entries.first() {
            let _ = writeln!(s, "  root directory:");
            entry_extra(s, root, 4);
        }
        if !g.problems.is_empty() {
            let _ = writeln!(s, "  problems:");
            for p in &g.problems {
                let _ = writeln!(s, "    {p}");
            }
        }
    }
    // an archive of a single file: the file
    if i.segments[0].metadata.is_none() {
        let contents = i.unnamed();
        let formulas = i.formulas(0, &contents, 4)?;
        let _ = writeln!(s, "\nfile (unnamed), of {} output{}:", contents.len(), plural(contents.len()));
        for (c, f) in contents.iter().zip(formulas) {
            let l = c.leaf.expect("a leaf");
            let leaf = &i.segments[0].leaves[l];
            let what = if leaf.formula { "formula block" } else { "leaf data block" };
            let _ =
                writeln!(s, "  leaf #{} ({what} at {}), output {} of {}:", l + 1, leaf.at, c.output + 1, leaf.outputs);
            s.push_str(&f);
        }
    }
    Ok(())
}

/// Path `p`'s entry in each segment, of `-I ARCHIVE PATH`.
fn path(s: &mut String, i: &Inspection, p: &Path) -> Result<(), String> {
    let bytes = crate::meta::os_bytes(p.as_os_str());
    let names: Vec<Vec<u8>> =
        bytes.split(|&b| b == b'/').filter(|n| !n.is_empty() && *n != b".").map(name_from).collect();
    if names.iter().any(|n| n == b"..") {
        return Err(format!("{}: not a path in an archive", p.display()));
    }
    let found: Vec<(usize, usize)> = (i.segments.iter().enumerate())
        .flat_map(|(k, g)| g.entries.iter().enumerate().filter(|(_, e)| e.names == names).map(move |(e, _)| (k, e)))
        .collect();
    if found.is_empty() {
        return Err(format!("{}: not in the archive", p.display()));
    }
    let now = current(i, &names);
    let shown = match names.is_empty() {
        true => "/".into(),
        false => names.iter().map(|n| String::from_utf8_lossy(name_bytes(n).0)).collect::<Vec<_>>().join("/"),
    };
    let _ = writeln!(s, "{shown}");
    for &(k, e) in &found {
        let entry = &i.segments[k].entries[e];
        let mark = match now {
            Some((g, f, _)) if (g, f) == (k, e) => " [current]",
            _ if entry.node == Node::Tombstone => " [removed]",
            _ => "",
        };
        let _ = writeln!(s, "  segment #{}{mark}:", k + 1);
        if let Some((g, f, Some(x))) = now
            && (g, f) == (k, e)
            && x != k
        {
            let _ = writeln!(s, "    (its extra that of segment #{})", x + 1);
        }
        entry_of(s, i, k, entry).map_err(|e| e.to_string())?;
    }
    if now.is_none() {
        let _ = writeln!(s, "  not in the archive as it is now");
    }
    Ok(())
}

/// Where the entry at `names` is in the archive as it is now, segments merged (spec, "Merging
/// segments"): its segment and entry, and for a directory the segment whose extra it has
/// (`None`: none has one).
fn current(i: &Inspection, names: &[Vec<u8>]) -> Option<(usize, usize, Option<usize>)> {
    // by path, the segment and entry, whether a directory, and the segment of its extra
    type Now = BTreeMap<Vec<Vec<u8>>, (usize, usize, bool, Option<usize>)>;
    let mut now: Now = BTreeMap::new();
    let remove = |now: &mut Now, path: &Vec<Vec<u8>>| {
        if now.remove(path).is_some_and(|n| n.2) {
            // (a directory, with what is under it)
            let under: Vec<_> =
                now.range(path.clone()..).take_while(|(k, _)| k.starts_with(path)).map(|(k, _)| k.clone()).collect();
            under.iter().for_each(|k| _ = now.remove(k));
        }
    };
    for (k, g) in i.segments.iter().enumerate() {
        for (e, entry) in g.entries.iter().enumerate() {
            let extra = entry.members.is_some().then_some(k);
            match entry.node {
                Node::Tombstone => remove(&mut now, &entry.names),
                Node::Root | Node::Dir => match now.get_mut(&entry.names) {
                    // (merged: the latest, and the extra of the latest of one)
                    Some(n) if n.2 => (n.0, n.1, n.3) = (k, e, extra.or(n.3)),
                    _ => {
                        remove(&mut now, &entry.names);
                        now.insert(entry.names.clone(), (k, e, true, extra));
                    }
                },
                _ => {
                    remove(&mut now, &entry.names);
                    now.insert(entry.names.clone(), (k, e, false, None));
                }
            }
        }
    }
    now.get(names).map(|&(k, e, dir, x)| (k, e, if dir { x } else { None }))
}

/// Entry `e` of segment `k`: what it is, its content, its extra, its forks and the formulas.
fn entry_of(s: &mut String, i: &Inspection, k: usize, e: &Entry) -> aqr::Decoded<()> {
    let g = &i.segments[k];
    let what = match &e.node {
        Node::Root => "the root directory".into(),
        Node::Dir => "a directory".into(),
        Node::File(c) => format!("a file of {} B", c.size),
        Node::Link(t) => {
            let t = String::from_utf8_lossy(name_bytes(t).0).into_owned();
            let kind = match e.extra.win_link_type {
                Some(2) => "a junction",
                Some(1) => "a link to a directory",
                _ => "a symbolic link",
            };
            format!("{kind} to {t:?}")
        }
        Node::Fifo => "a FIFO".into(),
        &Node::Device { block, major, minor } => {
            format!("a {} device {major},{minor}", if block { "block" } else { "character" })
        }
        Node::Tombstone => "a tombstone (removing what was there)".into(),
        Node::Special(kind) => format!("a special entry of kind {kind}, not known"),
    };
    let _ = write!(s, "    {what}");
    if e.number > 0 {
        let _ = write!(s, " (entry #{}, in #{})", e.number, e.parent);
    }
    s.push('\n');
    let content = |s: &mut String, c: &Content| {
        let _ = match c.leaf {
            Some(l) => {
                let leaf = &g.leaves[l];
                let what = if leaf.formula { "formula block" } else { "leaf data block" };
                write!(s, "output {} of {} of the {what} at {}", c.output + 1, leaf.outputs, leaf.at)
            }
            None => write!(s, "no leaf block"),
        };
        let _ = write!(s, " (relative offset {})", c.offset);
        if let Some(d) = c.dictionary {
            let _ = write!(s, ", dictionary #{}", d + 1);
        }
    };
    if let Node::File(c) = &e.node {
        s.push_str("    content: ");
        content(s, c);
        s.push('\n');
    }
    if e.node != Node::Tombstone {
        entry_extra(s, e, 4);
    }
    let mut contents = Vec::new();
    if let Node::File(c) = &e.node {
        contents.push(*c);
    }
    contents.extend(e.forks.iter().map(|(_, c)| *c));
    let formulas = i.formulas(k, &contents, 6)?;
    let mut formulas = formulas.into_iter();
    if let Node::File(_) = &e.node {
        let _ = writeln!(s, "    formula:");
        s.push_str(&formulas.next().unwrap_or_default());
    }
    for (name, c) in &e.forks {
        let _ = write!(s, "    fork {:?}: {} B, ", String::from_utf8_lossy(name), c.size);
        content(s, c);
        s.push_str("\n      formula:\n");
        for line in formulas.next().unwrap_or_default().lines() {
            let _ = writeln!(s, "  {line}");
        }
    }
    Ok(())
}

/// The members of `e`'s entry extra, a line each, indented by `indent`.
fn entry_extra(s: &mut String, e: &Entry, indent: usize) {
    let Some(members) = &e.members else {
        let _ = writeln!(s, "{:indent$}extra: none", "");
        return;
    };
    if members.is_empty() {
        let _ = writeln!(s, "{:indent$}extra: none (an empty map)", "");
    }
    for m in members {
        for line in member(m, &e.extra) {
            let _ = writeln!(s, "{:indent$}{line}", "");
        }
    }
}

/// Member `m` of an entry extra, read into `x`: its lines.
fn member(m: &Member, x: &Extra) -> Vec<String> {
    let unknown = || vec![format!("{} (not known): {}", m.key_text, m.value)];
    let Some(k) = m.key else { return unknown() };
    let Some(name) = entry_key_name(k) else { return unknown() };
    let not_read = || vec![format!("{name}: {} (not read)", m.value)];
    let text = |t: &Option<std::sync::Arc<[u8]>>| t.as_ref().map(|t| format!("{name}: {}", quoted(t)));
    let line = match name {
        "crtime" | "ctime" | "mtime" | "atime" | "arctime" => {
            let t = match name {
                "crtime" => x.crtime,
                "ctime" => x.ctime,
                "mtime" => x.mtime,
                "atime" => x.atime,
                _ => x.arctime,
            };
            match t {
                Some(t) => format!("{name}: {}", time(t)),
                None => format!("{name}: an extended time, not read: {}", m.value),
            }
        }
        "comment" => match &m.text {
            Some(t) => format!("{name}: {}", quoted(t)),
            None => return not_read(),
        },
        "link-group" => match x.link_group {
            Some(g) if g < 0 => format!("{name}: {g} (archive-wide)"),
            Some(g) => format!("{name}: {g} (of the segment)"),
            None => return not_read(),
        },
        "xattrs" => {
            let mut lines = vec![format!("{name}: {}", x.xattrs.len())];
            for (n, v) in &x.xattrs {
                lines.push(format!("  {}: {} B {}", quoted(n), v.len(), value(v)));
            }
            return lines;
        }
        // (told with the forks)
        "forks" => return Vec::new(),
        "win-link-type" => match x.win_link_type {
            Some(t) => format!("{name}: {t} ({})", ["a file", "a directory", "a junction"][t as usize]),
            None => return not_read(),
        },
        "win-attrs" => match x.win_attrs {
            Some(w) => format!("{name}: {w:#x} ({})", crate::list::win(x)),
            None => return not_read(),
        },
        "unix-mode" => match x.mode {
            Some(o) => format!("{name}: {o:#o} ({})", crate::list::mode(x)),
            None => return not_read(),
        },
        "unix-uid" | "unix-gid" => match if name == "unix-uid" { x.uid } else { x.gid } {
            Some(id) => format!("{name}: {id}"),
            None => return not_read(),
        },
        "unix-uname" => match text(&x.uname) {
            Some(l) => l,
            None => return not_read(),
        },
        "unix-gname" => match text(&x.gname) {
            Some(l) => l,
            None => return not_read(),
        },
        "mac-flags" => match x.mac_flags {
            Some(f) => format!("{name}: {f:#x} ({})", crate::list::mac(x)),
            None => return not_read(),
        },
        "zip-extras" => match &m.fields {
            Some(fields) => {
                let mut lines = vec![format!("{name}: {}", fields.len())];
                for (k, v) in fields {
                    let (header, id) = if *k < 0 { ("central", -1 - k) } else { ("local", *k) };
                    lines.push(format!("  {header} {id:#06x}: {} B {}", v.len(), hex(v)));
                }
                return lines;
            }
            None => return not_read(),
        },
        _ => return unknown(),
    };
    vec![line]
}

/// A member of the top-level map of the optional metadata.
fn archive_member(m: &Member) -> String {
    match (m.key.and_then(aqr::archive_key_name), &m.text) {
        (Some(name), Some(t)) if name == "archive-comment" => format!("{name}: {}", quoted(t)),
        (Some(name), _) => format!("{name}: {} (not read)", m.value),
        (None, _) => format!("{} (not known): {}", m.key_text, m.value),
    }
}

/// A time as recorded, in full: its date, time of day to the digits of its quantization
/// interval (and that interval where it is not a power of ten of seconds), and its time zone
/// (`Z` of none told, its offset, or `local` of a local time in an unknown time zone).
fn time(t: Time) -> String {
    let (y, mo, d, h, mi, sec) = t.civil();
    let mut s = format!("{y:04}-{mo:02}-{d:02}");
    if t.unit < 86_400_000_000_000 {
        let _ = write!(s, " {h:02}:{mi:02}:{sec:02}");
        // (the digits of the fraction the interval has)
        let mut digits = 9;
        let mut u = t.unit;
        while digits > 0 && u.is_multiple_of(10) {
            u /= 10;
            digits -= 1;
        }
        if digits > 0 {
            let frac = t.nanos / 10u32.pow(9 - digits as u32);
            let _ = write!(s, ".{frac:0digits$}");
        }
    }
    match t.zone {
        Zone::Utc => s.push('Z'),
        Zone::Offset(o) => {
            let m = (o as i32).abs() * 15;
            let _ = write!(s, "{}{:02}:{:02}", if o < 0 { '-' } else { '+' }, m / 60, m % 60);
        }
        Zone::Local => s += " local",
    }
    let pow10 = |mut u: u64| {
        while u.is_multiple_of(10) && u > 1 {
            u /= 10;
        }
        u == 1
    };
    if !pow10(t.unit) && t.unit != 86_400_000_000_000 {
        let _ = write!(s, " (to {})", interval(t.unit));
    }
    s
}

/// A quantization interval in nanoseconds, in the unit it is of.
fn interval(ns: u64) -> String {
    match ns {
        n if n.is_multiple_of(1_000_000_000) => format!("{} s", n / 1_000_000_000),
        n if n.is_multiple_of(1_000_000) => format!("{} ms", n / 1_000_000),
        n if n.is_multiple_of(1_000) => format!("{} us", n / 1_000),
        n => format!("{n} ns"),
    }
}

/// Text (an `aqr-text`'s bytes) quoted, bytes not UTF-8 escaped.
fn quoted(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(t) => format!("{t:?}"),
        Err(_) => format!("{:?}", b.escape_ascii().to_string()),
    }
}

/// A value of bytes: as text where it is printable UTF-8, else in hexadecimal.
fn value(v: &[u8]) -> String {
    match std::str::from_utf8(v) {
        Ok(t) if !t.chars().any(|c| c.is_control() && c != '\n' && c != '\t') => format!("{t:?}"),
        _ => hex(v),
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_in_full() {
        let t = Time { nanos: 123_456_789, ..Time::new(1_798_675_200, 0) };
        assert_eq!(time(t), "2026-12-31 00:00:00.123456789Z");
        assert_eq!(time(Time { unit: 1_000_000, nanos: 123_000_000, ..t }), "2026-12-31 00:00:00.123Z");
        assert_eq!(
            time(Time { unit: 1_000_000_000, nanos: 0, zone: Zone::Offset(36), ..t }),
            "2026-12-31 09:00:00+09:00"
        );
        assert_eq!(time(Time { unit: 2_000_000_000, nanos: 0, ..t }), "2026-12-31 00:00:00Z (to 2 s)");
        assert_eq!(time(Time { unit: 86_400_000_000_000, nanos: 0, zone: Zone::Local, ..t }), "2026-12-31 local");
        assert_eq!(time(Time { unit: 100, ..t }), "2026-12-31 00:00:00.1234567Z");
    }
}
