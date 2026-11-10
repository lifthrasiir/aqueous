//! Journal points on the command line (`docs/append-design.md`, "Journaling"): the label of `-j`
//! with its placeholders, the time a run is named by, and `--at`, which picks a point by its
//! number, its label or a date.

use std::path::Path;

use aqr_foreign::time::{Civil, local_civil, local_secs};
use aqr_formula::aqr::{self, JournalPoint};

/// The time a run is named by, in milliseconds since the epoch (UTC): the clock's, or
/// `AQR_TEST_NOW` (milliseconds) where it is set, for tests.
pub fn now_ms() -> Result<u64, String> {
    if let Ok(v) = std::env::var("AQR_TEST_NOW") {
        return v.trim().parse().map_err(|_| "AQR_TEST_NOW takes milliseconds since the epoch".to_string());
    }
    let d =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_| "the clock is before 1970")?;
    Ok(d.as_millis() as u64)
}

/// `ms` as `YYYY-MM-DD HH:MM:SS` in local time (UTC where the system does not tell).
pub fn show(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let (y, mo, d, h, mi, s) = local_civil(secs).unwrap_or_else(|| civil(secs));
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// The label `label` of `-j` with its placeholders filled in at the local time `at` (`{today}`:
/// `YYYY-MM-DD`; `{now}`: `YYYY-MM-DD HH:MM:SS`); `{{` and `}}` are braces. Another name is an
/// error.
pub fn expand(label: &str, at: Civil) -> Result<String, String> {
    let (y, mo, d, h, mi, s) = at;
    let mut out = String::new();
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => return Err("-j: a { never closed ({{ for a brace)".into()),
                    }
                }
                match name.as_str() {
                    "today" => out.push_str(&format!("{y:04}-{mo:02}-{d:02}")),
                    "now" => out.push_str(&format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")),
                    _ => {
                        return Err(format!("-j: no placeholder {{{name}}} ({{today}} and {{now}}; {{{{ for a brace)"));
                    }
                }
            }
            '}' => return Err("-j: a lone } (}} for a brace)".into()),
            c => out.push(c),
        }
    }
    Ok(out)
}

/// The label and time of a run named by `label` at `now` (milliseconds since the epoch).
pub fn point(label: &str, now: u64) -> Result<(u64, String), String> {
    let secs = (now / 1000) as i64;
    let at = local_civil(secs).unwrap_or_else(|| civil(secs));
    Ok((now, expand(label, at)?))
}

/// Which point `--at` picks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum At {
    /// `#N`: the Nth (from 1), or `#-N` the Nth from the last.
    Number(i64),
    /// `@LABEL`: the last of that label.
    Label(String),
    /// A date: the last at or before this time, the end of the period it names, in milliseconds.
    Until(u64),
}

/// `--at SPEC`. A date is `YYYY`, `YYYY-MM`, `YYYY-MM-DD` or `YYYY-MM-DD[T ]HH[:MM[:SS]]`, then
/// `Z` or `+HH[:MM]` for another time zone than the local one; what is left out is to the end of
/// the period (`2026-11-01` is up to 23:59:59.999 that day). `local` takes a date and time of day
/// in local time to seconds since the epoch.
pub fn parse_at(spec: &str, local: &dyn Fn(Civil) -> Option<i64>) -> Result<At, String> {
    let bad = || format!("--at {spec}: takes #N, #-N, @LABEL or a date (YYYY[-MM[-DD[THH[:MM[:SS]]]]])");
    if let Some(n) = spec.strip_prefix('#') {
        return match n.parse::<i64>() {
            Ok(n) if n != 0 => Ok(At::Number(n)),
            _ => Err(bad()),
        };
    }
    if let Some(l) = spec.strip_prefix('@') {
        return Ok(At::Label(l.to_string()));
    }
    // the zone off the end
    let (body, zone) = match spec.strip_suffix('Z') {
        Some(b) => (b, Some(0i64)),
        None => match spec.rfind(['+', '-']).filter(|&i| i >= 10 && spec[..i].contains(['T', ' '])) {
            Some(i) => {
                let (h, m) = spec[i + 1..].split_once(':').unwrap_or((&spec[i + 1..], "0"));
                let off = h.parse::<i64>().map_err(|_| bad())? * 3600 + m.parse::<i64>().map_err(|_| bad())? * 60;
                (&spec[..i], Some(if spec[i..].starts_with('-') { -off } else { off }))
            }
            None => (spec, None),
        },
    };
    let (date, time) = match body.split_once(['T', ' ']) {
        Some((d, t)) => (d, Some(t)),
        None => (body, None),
    };
    let num = |s: &str| s.parse::<u32>().ok().filter(|_| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()));
    let mut parts = date.split('-');
    let y = parts.next().filter(|y| y.len() == 4).and_then(num).ok_or_else(bad)? as i32;
    let mo = parts.next().map(|s| num(s).filter(|m| (1..=12).contains(m)).ok_or_else(bad)).transpose()?;
    let d = parts.next().map(|s| num(s).filter(|d| (1..=31).contains(d)).ok_or_else(bad)).transpose()?;
    if parts.next().is_some() || (time.is_some() && d.is_none()) {
        return Err(bad());
    }
    let clock: Vec<u32> = match time {
        Some(t) => t.split(':').map(|s| num(s).ok_or_else(bad)).collect::<Result<_, _>>()?,
        None => Vec::new(),
    };
    if clock.len() > 3 || clock.first().is_some_and(|&h| h > 23) || clock.iter().skip(1).any(|&v| v > 59) {
        return Err(bad());
    }
    let (h, mi, s) = (clock.first().copied(), clock.get(1).copied(), clock.get(2).copied());
    if let Some(d) = d
        && d > days_in(y, mo.unwrap_or(1))
    {
        return Err(bad());
    }
    let secs = |c: Civil| match zone {
        Some(off) => Some(days_from_civil(c.0, c.1, c.2) * 86400 + (c.3 * 3600 + c.4 * 60 + c.5) as i64 - off),
        None => local(c),
    };
    // the start of the period named, and the start of the next one
    let start = (y, mo.unwrap_or(1), d.unwrap_or(1), h.unwrap_or(0), mi.unwrap_or(0), s.unwrap_or(0));
    let next = match (mo, d, h) {
        (None, ..) => secs((y + 1, 1, 1, 0, 0, 0)),
        (Some(m), None, _) => secs(if m == 12 { (y + 1, 1, 1, 0, 0, 0) } else { (y, m + 1, 1, 0, 0, 0) }),
        (Some(m), Some(d), None) => secs(match (d < days_in(y, m), m) {
            (true, _) => (y, m, d + 1, 0, 0, 0),
            (false, 12) => (y + 1, 1, 1, 0, 0, 0),
            (false, _) => (y, m + 1, 1, 0, 0, 0),
        }),
        // (to the end of the finest unit given)
        (Some(_), Some(_), Some(_)) => {
            let unit = if s.is_some() {
                1
            } else if mi.is_some() {
                60
            } else {
                3600
            };
            secs(start).map(|t| t + unit)
        }
    };
    let next = next.ok_or_else(|| format!("--at {spec}: not a time here"))?;
    Ok(At::Until(u64::try_from(next * 1000 - 1).map_err(|_| format!("--at {spec}: before 1970"))?))
}

/// The point of `points` that `at` picks.
pub fn pick<'a>(points: &'a [JournalPoint], at: &At) -> Result<&'a JournalPoint, String> {
    if points.is_empty() {
        return Err("no journal points (-j makes them)".into());
    }
    match at {
        At::Number(n) => {
            let k = if *n > 0 { *n as usize - 1 } else { points.len().wrapping_sub(n.unsigned_abs() as usize) };
            points.get(k).ok_or_else(|| format!("no point #{n}: it has {}", points.len()))
        }
        At::Label(l) => points.iter().rev().find(|p| p.label == *l).ok_or_else(|| format!("no point labeled {l:?}")),
        At::Until(ms) => {
            // (the latest time, and of equal times the later segment)
            let best = points.iter().enumerate().filter(|(_, p)| p.time <= *ms).max_by_key(|&(k, p)| (p.time, k));
            best.map(|(_, p)| p).ok_or_else(|| {
                let first = points.iter().map(|p| p.time).min().unwrap_or(0);
                format!("no point at or before {}: the earliest is {}", show(*ms), show(first))
            })
        }
    }
}

/// `--at SPEC` as the command line gives it (dates in local time here).
pub fn at_of(spec: &str) -> Result<At, String> {
    parse_at(spec, &local_secs)
}

/// Where archive `bytes` (of `input`, not encrypted) is to end for `at`: the metadata block of
/// the segment of the point it picks ([`aqr::open_upto`]), with what is shown of the archive
/// told unless `quiet`.
pub fn upto(input: &Path, bytes: &[u8], at: &At, quiet: bool) -> Result<usize, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", input.display());
    if aqr::envelope::is_encrypted(bytes) {
        return Err(err(&"--at: an encrypted archive has no journal (it is not appended to)"));
    }
    let archive = aqr::open(bytes, &aqr::NoModules).map_err(|e| err(&e))?;
    let points = archive.journal();
    let picked = pick(points, at).map_err(|e| err(&format!("--at: {e}")))?;
    if !quiet {
        let n = points.iter().position(|p| p.metadata == picked.metadata && p.time == picked.time).unwrap_or(0);
        let later = points.len() - n - 1;
        eprintln!(
            "{}: as of #{} {} {:?}{}",
            input.display(),
            n + 1,
            show(picked.time),
            picked.label,
            match later {
                0 => String::new(),
                1 => "; 1 later point not shown".to_string(),
                n => format!("; {n} later points not shown"),
            }
        );
        if out_of_order(points) {
            eprintln!("{}: the times of its journal points do not increase with their order", input.display());
        }
    }
    Ok(picked.metadata)
}

/// `--history`: the points of each archive of `a` listed.
pub fn history_all(a: &crate::Args) -> Result<(), String> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    for (k, input) in a.inputs.iter().enumerate() {
        let err = |e: &dyn std::fmt::Display| format!("{}: {e}", input.display());
        let file = std::fs::File::open(input).map_err(|e| err(&crate::exit::seen(e)))?;
        // SAFETY: the archive is not to change while it is read; where it does, its checksums tell
        let c = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| err(&crate::exit::seen(e)))?;
        let text = history(input, crate::located(&c, input, a.quiet))?;
        if a.inputs.len() > 1 {
            if k > 0 {
                writeln!(out).map_err(|e| e.to_string())?;
            }
            writeln!(out, "{}:", input.display()).map_err(|e| e.to_string())?;
        }
        out.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    }
    out.flush().map_err(|e| e.to_string())
}

/// An entry of a state as `--diff` tells states apart: its path, and what it is (a file by its
/// size) with its metadata but for what is numbered by the archive (forks, link groups).
type Seen = (Vec<Vec<u8>>, aqr::EntryNode, aqr::Extra);

/// The entries of `archive` (a directory by its path alone), by path.
fn seen(archive: &aqr::Archive) -> std::collections::BTreeMap<Vec<Vec<u8>>, Seen> {
    let Some(t) = archive.tree() else { return Default::default() };
    let mut out = std::collections::BTreeMap::new();
    for (e, path) in t.entries.iter().zip(crate::append::paths(t)) {
        let (node, extra) = match e.node {
            aqr::EntryNode::Dir => (aqr::EntryNode::Dir, aqr::Extra::default()),
            // (a file by its size: the number of its content is the archive's)
            aqr::EntryNode::File(c) => {
                let size = archive.content_size(c).unwrap_or(u64::MAX) as usize;
                (aqr::EntryNode::File(size), aqr::Extra { forks: Vec::new(), link_group: None, ..e.extra.clone() })
            }
            ref other => (other.clone(), aqr::Extra { forks: Vec::new(), link_group: None, ..e.extra.clone() }),
        };
        out.insert(path.clone(), (path, node, extra));
    }
    out
}

/// The entries of `archive` as a quick check tells them apart (`-a`'s: a file by its size and
/// modification time, other entries by what they are), by path.
fn quick(archive: &aqr::Archive) -> std::collections::BTreeMap<Vec<Vec<u8>>, Seen> {
    let Some(t) = archive.tree() else { return Default::default() };
    let sized = |e: &aqr::Entry| match e.node {
        aqr::EntryNode::File(c) => aqr::EntryNode::File(archive.content_size(c).unwrap_or(u64::MAX) as usize),
        ref other => other.clone(),
    };
    keyed(t, &sized)
}

/// The entries of `t` as a quick check tells them apart, a file's content told by `node`.
fn keyed(
    t: &aqr::Tree,
    node: &dyn Fn(&aqr::Entry) -> aqr::EntryNode,
) -> std::collections::BTreeMap<Vec<Vec<u8>>, Seen> {
    let mut out = std::collections::BTreeMap::new();
    for (e, path) in t.entries.iter().zip(crate::append::paths(t)) {
        let node = node(e);
        let mtime = matches!(node, aqr::EntryNode::File(_)).then_some(e.extra.mtime).flatten();
        out.insert(path.clone(), (path, node, aqr::Extra { mtime, ..aqr::Extra::default() }));
    }
    out
}

/// Writes how `after` differs from `before` (a line each: `+` added, `~` changed, `-` removed),
/// and tells how many of each.
fn report(out: &mut dyn std::io::Write, before: &Seens, after: &Seens) -> Result<(usize, usize, usize), String> {
    let shown = |path: &[Vec<u8>], dir: bool| {
        let names: Vec<String> =
            path.iter().map(|n| String::from_utf8_lossy(aqr::name_bytes(n).0).into_owned()).collect();
        names.join("/") + if dir { "/" } else { "" }
    };
    let is_dir = |e: &Seen| e.1 == aqr::EntryNode::Dir;
    let io = |e: std::io::Error| e.to_string();
    let (mut added, mut changed, mut removed) = (0, 0, 0);
    for (path, now) in after {
        let mark = match before.get(path) {
            None => '+',
            Some(was) if was.1 != now.1 || was.2 != now.2 => '~',
            Some(_) => continue,
        };
        match mark {
            '+' => added += 1,
            _ => changed += 1,
        }
        writeln!(out, "{mark} {}", shown(path, is_dir(now))).map_err(io)?;
    }
    for (path, was) in before.iter().filter(|(p, _)| !after.contains_key(*p)) {
        removed += 1;
        writeln!(out, "- {}", shown(path, is_dir(was))).map_err(io)?;
    }
    Ok((added, changed, removed))
}

type Seens = std::collections::BTreeMap<Vec<Vec<u8>>, Seen>;

/// `--diff`: what differs between two states of the archive of `a` (its first input), a line
/// each: `+` added, `~` changed (by size and metadata), `-` removed; a directory with a `/` after
/// it, and not told for its metadata. The states are the points `--from` and `--at` pick; `--at`
/// alone against the point before it (none: nothing), neither the last point against the
/// archive as it is now. With paths after the archive, the archive (as `--at` picks it) against
/// them ([`compare`]).
pub fn diff_all(a: &crate::Args) -> Result<(), String> {
    use std::io::Write;
    let input = a.inputs.first().ok_or("--diff takes an archive")?;
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", input.display());
    let file = std::fs::File::open(input).map_err(|e| err(&crate::exit::seen(e)))?;
    // SAFETY: the archive is not to change while it is read; where it does, its checksums tell
    let c = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| err(&crate::exit::seen(e)))?;
    let bytes = crate::located(&c, input, a.quiet);
    if aqr::envelope::is_encrypted(bytes) {
        return Err(err(&"an encrypted archive has no journal (it is not appended to)"));
    }
    let whole = aqr::open(bytes, &aqr::NoModules).map_err(|e| err(&e))?;
    let points = whole.journal();
    let find = |at: &At| pick(points, at).map_err(|e| err(&e));
    let index = |p: &JournalPoint| points.iter().position(|q| q.metadata == p.metadata && q.time == p.time);
    let named = |p: &JournalPoint| format!("#{} {:?}", index(p).map_or(0, |i| i + 1), p.label);
    let state = |p: Option<&JournalPoint>| -> Result<Seens, String> {
        match p {
            Some(p) => aqr::open_upto(bytes, &aqr::NoModules, p.metadata).map(|x| seen(&x)).map_err(|e| err(&e)),
            None => Ok(Default::default()),
        }
    };
    let mut out = std::io::stdout().lock();
    if a.inputs.len() > 1 {
        return compare(a, &whole, bytes, &mut out);
    }
    // the later state, and the earlier (none: nothing)
    let (later, to) = match &a.at {
        Some(at) => {
            let p = find(at)?;
            (Some(p), named(p))
        }
        None => (None, "the archive now".to_string()),
    };
    let earlier = match (&a.from, later) {
        (Some(at), _) => Some(find(at)?),
        (None, Some(p)) => index(p).and_then(|i| i.checked_sub(1)).map(|i| &points[i]),
        (None, None) => Some(points.last().ok_or_else(|| err(&"no journal points (-j makes them)"))?),
    };
    let from = earlier.map_or("nothing".to_string(), named);
    let (before, after) = (state(earlier)?, if later.is_some() { state(later)? } else { seen(&whole) });
    let (added, changed, removed) = report(&mut out, &before, &after)?;
    if !a.quiet {
        eprintln!("{}: {added} added, {changed} changed, {removed} removed ({from} -> {to})", input.display());
    }
    out.flush().map_err(|e| e.to_string())
}

/// `--diff ARCHIVE PATH...`: the archive (as `--at` picks it, else as it is) against the paths
/// as `-a` would take them, told by a quick check (size and modification time): `+` what they
/// have that it does not, `-` what it has that they do not, `~` what differs.
fn compare(a: &crate::Args, whole: &aqr::Archive, bytes: &[u8], out: &mut dyn std::io::Write) -> Result<(), String> {
    let input = &a.inputs[0];
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", input.display());
    if a.from.is_some() {
        return Err("--from with paths (the archive against them; --at picks its state)".into());
    }
    if a.renames[0].is_some() {
        return Err(format!("{}: --rename on the archive", input.display()));
    }
    let (state, of) = match &a.at {
        Some(at) => {
            let p = pick(whole.journal(), at).map_err(|e| err(&e))?;
            let n = whole.journal().iter().position(|q| q.metadata == p.metadata && q.time == p.time).unwrap_or(0);
            let archive = aqr::open_upto(bytes, &aqr::NoModules, p.metadata).map_err(|e| err(&e))?;
            (quick(&archive), format!("#{} {:?}", n + 1, p.label))
        }
        None => (quick(whole), "the archive now".to_string()),
    };
    let mapper = crate::Mapper::new().map_err(|e| format!("files not to be mapped: {e}"))?;
    let mut g = crate::Gathered::new(mapper, a.preserve);
    let listing = crate::view::Listing::new(crate::bars());
    let gathered = crate::ticking(a, &listing, || -> Result<(), String> {
        crate::gather(&a.inputs[1..], &a.given[1..], &a.renames[1..], &mut g, &listing, a)?;
        listing.finish();
        Ok(())
    });
    listing.finish();
    gathered?;
    if !a.quiet {
        g.skipped.iter().for_each(|s| eprintln!("skipped {s}"));
        g.warnings.iter().for_each(|w| eprintln!("{w}"));
    }
    let here = keyed(&g.tree, &|e| match e.node {
        aqr::EntryNode::File(c) => aqr::EntryNode::File(g.contents[c].2 as usize),
        ref other => other.clone(),
    });
    let (added, changed, removed) = report(out, &state, &here)?;
    if !a.quiet {
        let to: Vec<String> = a.inputs[1..].iter().map(|p| p.display().to_string()).collect();
        eprintln!(
            "{}: {added} only in {to}, {changed} changed, {removed} only in the archive ({of} -> {to})",
            input.display(),
            to = to.join(" ")
        );
    }
    out.flush().map_err(|e| e.to_string())
}

/// The points of archive `bytes` (of `input`) listed, one on a line: its number, time and label.
pub fn history(input: &Path, bytes: &[u8]) -> Result<String, String> {
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", input.display());
    if aqr::envelope::is_encrypted(bytes) {
        return Err(err(&"an encrypted archive has no journal (it is not appended to)"));
    }
    let archive = aqr::open(bytes, &aqr::NoModules).map_err(|e| err(&e))?;
    let mut out = String::new();
    for (k, p) in archive.journal().iter().enumerate() {
        out += &format!("#{}  {}  {}\n", k + 1, show(p.time), p.label);
    }
    if out.is_empty() {
        return Err(err(&"no journal points (-j makes them)"));
    }
    Ok(out)
}

/// Whether the times of `points` do not increase with their order.
pub fn out_of_order(points: &[JournalPoint]) -> bool {
    points.windows(2).any(|w| w[1].time < w[0].time)
}

fn days_in(y: i32, m: u32) -> u32 {
    match m {
        2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days from 1970-01-01 to the date (proleptic Gregorian).
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = y as i64 - (m <= 2) as i64;
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m as i64 + if m > 2 { -3 } else { 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The date and time of day in UTC of `secs` since the epoch.
fn civil(secs: i64) -> Civil {
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = (yoe + era * 400) as i32 + (m <= 2) as i32;
    (y, m, d, (rem / 3600) as u32, (rem % 3600 / 60) as u32, (rem % 60) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UTC as the local time.
    fn utc(c: Civil) -> Option<i64> {
        Some(days_from_civil(c.0, c.1, c.2) * 86400 + (c.3 * 3600 + c.4 * 60 + c.5) as i64)
    }

    fn at(spec: &str) -> Result<At, String> {
        parse_at(spec, &utc)
    }

    fn ms(c: Civil) -> u64 {
        utc(c).unwrap() as u64 * 1000
    }

    #[test]
    fn civil_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(civil(1_700_000_000), (2023, 11, 14, 22, 13, 20));
        for secs in [0, 86_399, 951_782_400, 1_709_251_199, 4_102_444_800] {
            let c = civil(secs);
            assert_eq!(utc(c), Some(secs), "{c:?}");
        }
    }

    #[test]
    fn labels() {
        let now = (2026, 11, 3, 4, 5, 6);
        assert_eq!(expand("daily {today}", now).unwrap(), "daily 2026-11-03");
        assert_eq!(expand("{now}", now).unwrap(), "2026-11-03 04:05:06");
        assert_eq!(expand("{{today}} }}", now).unwrap(), "{today} }");
        assert_eq!(expand("", now).unwrap(), "");
        assert!(expand("{tomorrow}", now).is_err());
        assert!(expand("a } b", now).is_err());
        assert!(expand("{today", now).is_err());
    }

    #[test]
    fn at_specs() {
        assert_eq!(at("#3"), Ok(At::Number(3)));
        assert_eq!(at("#-1"), Ok(At::Number(-1)));
        assert_eq!(at("@weekly"), Ok(At::Label("weekly".into())));
        assert!(at("#0").is_err() && at("#x").is_err() && at("").is_err() && at("yesterday").is_err());
        // up to the end of the period named
        assert_eq!(at("2026"), Ok(At::Until(ms((2027, 1, 1, 0, 0, 0)) - 1)));
        assert_eq!(at("2026-12"), Ok(At::Until(ms((2027, 1, 1, 0, 0, 0)) - 1)));
        assert_eq!(at("2026-02"), Ok(At::Until(ms((2026, 3, 1, 0, 0, 0)) - 1)));
        assert_eq!(at("2026-11-01"), Ok(At::Until(ms((2026, 11, 2, 0, 0, 0)) - 1)));
        assert_eq!(at("2026-11-30"), Ok(At::Until(ms((2026, 12, 1, 0, 0, 0)) - 1)));
        assert_eq!(at("2028-02-29"), Ok(At::Until(ms((2028, 3, 1, 0, 0, 0)) - 1)));
        assert_eq!(at("2026-12-31"), Ok(At::Until(ms((2027, 1, 1, 0, 0, 0)) - 1)));
        assert_eq!(at("2026-11-01T09"), Ok(At::Until(ms((2026, 11, 1, 10, 0, 0)) - 1)));
        assert_eq!(at("2026-11-01 09:30"), Ok(At::Until(ms((2026, 11, 1, 9, 31, 0)) - 1)));
        assert_eq!(at("2026-11-01T09:30:15"), Ok(At::Until(ms((2026, 11, 1, 9, 30, 16)) - 1)));
        // another zone
        assert_eq!(at("2026-11-01T09:30Z"), Ok(At::Until(ms((2026, 11, 1, 9, 31, 0)) - 1)));
        assert_eq!(at("2026-11-01T09:30+09:00"), Ok(At::Until(ms((2026, 11, 1, 0, 31, 0)) - 1)));
        assert_eq!(at("2026-11-01T09:30-05"), Ok(At::Until(ms((2026, 11, 1, 14, 31, 0)) - 1)));
        for bad in ["2026-13", "2026-02-30", "2026-11-01T24", "2026-11-01T09:60", "26", "2026-11-01T", "2026-11-01-01"]
        {
            assert!(at(bad).is_err(), "{bad}");
        }
    }

    fn points(spec: &[(u64, &str)]) -> Vec<JournalPoint> {
        spec.iter()
            .enumerate()
            .map(|(k, &(time, l))| JournalPoint { metadata: 100 + k * 10, time, label: l.to_string() })
            .collect()
    }

    #[test]
    fn picks() {
        let day = |d: u32| ms((2026, 11, d, 12, 0, 0));
        let ps = points(&[(day(1), "daily"), (day(2), "daily"), (day(2), "weekly"), (day(5), "daily")]);
        let pick = |s: &str| pick(&ps, &at(s).unwrap()).map(|p| p.metadata);
        assert_eq!(pick("#1"), Ok(100));
        assert_eq!(pick("#4"), Ok(130));
        assert_eq!(pick("#-1"), Ok(130));
        assert_eq!(pick("#-4"), Ok(100));
        assert!(pick("#5").is_err() && pick("#-5").is_err());
        assert_eq!(pick("@daily"), Ok(130));
        assert_eq!(pick("@weekly"), Ok(120));
        assert!(pick("@monthly").is_err());
        assert_eq!(pick("2026-11-01"), Ok(100));
        // two points of a day: the later
        assert_eq!(pick("2026-11-02"), Ok(120));
        assert_eq!(pick("2026-11-04"), Ok(120));
        assert_eq!(pick("2026-11-05T11"), Ok(120));
        assert_eq!(pick("2026-11-05"), Ok(130));
        let early = pick("2026-10").unwrap_err();
        assert!(early.contains("the earliest is 2026-11-01"), "{early}");
        assert!(super::pick(&[], &At::Number(1)).is_err());
        assert!(!out_of_order(&ps));
        assert!(out_of_order(&points(&[(5, "a"), (4, "b")])));
        // out of order: the latest time wins, not the last segment
        let ps = points(&[(day(9), "a"), (day(3), "b")]);
        assert_eq!(super::pick(&ps, &At::Until(day(10))).map(|p| p.metadata), Ok(100));
    }
}
