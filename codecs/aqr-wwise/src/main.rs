//! `aqr-wwise ids FILE...`: the Vorbis media of `.wem` files and of banks (`.bnk`), their
//! stripped setups grouped, and the codebooks of aqr-vorbis's setup database that their ids may stand for:
//! the database's skeletons with as many codebooks tried on each setup, by whether its packets
//! parse to their ends.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use aqr_vorbis::packet;
use aqr_vorbis::setup::{Ident, Setup};
use aqr_vorbis::setups::Setups;
use aqr_wwise::stripped::{self, Stripped};
use aqr_wwise::wem;

fn u32le(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// The media of bank `d` (little endian), as ranges.
fn bank_media(d: &[u8]) -> Vec<std::ops::Range<usize>> {
    let (mut didx, mut data) = (None, None);
    let mut at = 0usize;
    while let (Some(tag), Some(size)) = (d.get(at..at + 4), u32le(d, at + 4)) {
        let r = at + 8..(at + 8).saturating_add(size as usize).min(d.len());
        match tag {
            b"DIDX" => didx = Some(r.clone()),
            b"DATA" => data = Some(r.clone()),
            _ => {}
        }
        at = r.end;
    }
    let mut out = Vec::new();
    if let (Some(didx), Some(data)) = (didx, data) {
        for e in d[didx].as_chunks::<12>().0 {
            let (o, n) = (u32le(e, 4).unwrap() as usize, u32le(e, 8).unwrap() as usize);
            if data.start + o + n <= data.end {
                out.push(data.start + o..data.start + o + n);
            }
        }
    }
    out
}

/// The RIFF WAVEs of format `FFFF` in `d` (a pack of another format), as ranges.
fn embedded(d: &[u8]) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(k) = d[at..].windows(8).position(|w| w == b"WAVEfmt ") {
        let w = at + k;
        at = w + 8;
        let Some(start) = w.checked_sub(8) else { continue };
        if &d[start..start + 4] != b"RIFF" || d.get(w + 12..w + 14) != Some(&[0xff, 0xff]) {
            continue;
        }
        let end = (start + 8).saturating_add(u32le(d, start + 4).unwrap() as usize).min(d.len());
        out.push(start..end);
        at = end;
    }
    out
}

struct Medium {
    name: String,
    d: Vec<u8>,
    w: wem::Wem,
}

/// How a setup parses a group's packets: (parsed exactly, tried).
fn score(s: &Stripped, setup: &Setup, media: &[&Medium], limit: usize) -> (usize, usize) {
    let (exact, tried, _) = score_by(s, setup, media, limit);
    (exact, tried)
}

/// As `score`, with how far the packets that parse end from their ends, in bits, in all.
fn score_by(s: &Stripped, setup: &Setup, media: &[&Medium], limit: usize) -> (usize, usize, u64) {
    let (mut exact, mut tried, mut off) = (0, 0, 0u64);
    'm: for m in media {
        let ps = &m.w.packets;
        let long = |i: usize| -> bool {
            ps.get(i)
                .and_then(|p| stripped::mode(s, &m.d[p.clone()]))
                .and_then(|k| s.blockflags.get(k).copied())
                .unwrap_or(false)
        };
        for (i, p) in ps.iter().enumerate() {
            if tried >= limit {
                break 'm;
            }
            tried += 1;
            let d = &m.d[p.clone()];
            let prev = i > 0 && long(i - 1);
            let miss = d.len() as u64 * 8;
            let Some((p, added)) = stripped::rebuild(s, d, prev, long(i + 1)) else {
                off += miss;
                continue;
            };
            let Ok(v) = packet::parse(setup, &p) else {
                off += miss;
                continue;
            };
            if v.short {
                off += miss;
            } else {
                off += (v.tail.len() as u64).saturating_sub(added as u64 + 7);
            }
            let consumed = p.len() * 8 - v.tail.len() - added;
            let left = (d.len() * 8).checked_sub(consumed);
            if !v.short && v.tail.iter().all(|&b| b == 0) && left.is_some_and(|l| l < 8) {
                exact += 1;
            }
        }
    }
    (exact, tried, off)
}

/// Codebook parts for `s` found one place at a time, from skeleton codebooks `start`: each place
/// given the part that parses the most packets exactly (then ends them nearest their ends), the
/// others kept, in rounds until all parse or nothing changes.
fn descend(db: &Setups, s: &Stripped, ident: Ident, ms: &[&Medium], start: &[u32]) -> (Vec<u32>, (usize, usize)) {
    const SAMPLE: usize = 200;
    let eval = |parts: &[u32]| -> Option<(usize, usize, u64)> {
        let books: Vec<_> = parts.iter().map(|&b| db.codebook(b as usize)).collect();
        let setup = Setup::parse(&s.assemble(&books), ident).ok()?;
        Some(score_by(s, &setup, ms, SAMPLE))
    };
    let key = |r: (usize, usize, u64)| (r.0, std::cmp::Reverse(r.2));
    let mut parts = start.to_vec();
    let mut best = eval(&parts).unwrap_or((0, SAMPLE, u64::MAX));
    for round in 0..6 {
        let mut changed = false;
        for place in 0..parts.len() {
            for cand in 0..db.codebook_count() as u32 {
                if cand == parts[place] {
                    continue;
                }
                let old = std::mem::replace(&mut parts[place], cand);
                match eval(&parts) {
                    Some(r) if key(r) > key(best) => {
                        best = r;
                        changed = true;
                    }
                    _ => parts[place] = old,
                }
            }
        }
        println!("  descent round {round}: {}/{} exact, {} bits off", best.0, best.1, best.2);
        if !changed || best.0 == best.1 {
            break;
        }
    }
    (parts, (best.0, best.1))
}

/// What becomes of the packets that do not parse exactly: counts by kind (where in the medium,
/// how it fails), and some media.
fn diagnose(s: &Stripped, setup: &Setup, media: &[&Medium]) {
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut shown = 0;
    for m in media {
        let ps = &m.w.packets;
        let long = |i: usize| -> bool {
            ps.get(i)
                .and_then(|p| stripped::mode(s, &m.d[p.clone()]))
                .and_then(|k| s.blockflags.get(k).copied())
                .unwrap_or(false)
        };
        let mut bad = Vec::new();
        for (i, p) in ps.iter().enumerate() {
            let d = &m.d[p.clone()];
            let (rp, added) = match stripped::rebuild(s, d, i > 0 && long(i - 1), long(i + 1)) {
                Some(x) => x,
                None => {
                    bad.push((i, "no mode".to_string()));
                    continue;
                }
            };
            let kind = match packet::parse(setup, &rp) {
                Err(e) => format!("error {}", e.0),
                Ok(v) if v.short => "short".into(),
                Ok(v) => {
                    let left = (d.len() * 8) as i64 - (rp.len() * 8 - v.tail.len() - added) as i64;
                    let ones = v.tail.iter().filter(|&&b| b == 1).count();
                    if (0..8).contains(&left) && ones == 0 {
                        continue;
                    }
                    format!("left {} bits ({ones} ones)", if left > 64 { ">64".into() } else { left.to_string() })
                }
            };
            bad.push((i, kind));
        }
        for (i, k) in &bad {
            let at = if *i == 0 {
                "first"
            } else if *i + 1 == ps.len() {
                "last"
            } else {
                "inner"
            };
            *kinds.entry(format!("{at}: {k}")).or_default() += 1;
        }
        if !bad.is_empty() && shown < 6 {
            shown += 1;
            println!(
                "    {}: {} of {} packets ({} bytes, {} samples), e.g. {:?}",
                m.name,
                bad.len(),
                ps.len(),
                m.d.len(),
                m.w.samples,
                &bad[..bad.len().min(4)]
            );
        }
    }
    for (k, n) in kinds {
        println!("    {n} {k}");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("ids") {
        eprintln!("usage: aqr-wwise ids [--db FILE] FILE...|@LIST...");
        std::process::exit(2);
    }
    let mut media = Vec::new();
    let mut failed: BTreeMap<String, usize> = BTreeMap::new();
    // (`@LIST`: the files named in LIST, a line each)
    // (`--db FILE`: a database other than aqr-vorbis's, as its FORMAT.md §8.1 has it)
    let mut rest = &args[1..];
    let mut db_file = None;
    if rest.first().map(String::as_str) == Some("--db") {
        db_file = Some(std::fs::read(&rest[1]).expect("database"));
        rest = &rest[2..];
    }
    let names: Vec<String> = rest
        .iter()
        .flat_map(|a| match a.strip_prefix('@') {
            Some(list) => std::fs::read_to_string(list).expect("list").lines().map(str::to_owned).collect(),
            None => vec![a.clone()],
        })
        .collect();
    for name in &names {
        let d = std::fs::read(name).unwrap_or_else(|e| panic!("{name}: {e}"));
        let ranges = if d.starts_with(b"BKHD") {
            bank_media(&d)
        } else if d.starts_with(b"RIFF") {
            std::iter::once(0..d.len()).collect()
        } else {
            embedded(&d)
        };
        for (i, r) in ranges.into_iter().enumerate() {
            let m = &d[r.clone()];
            match wem::parse(m) {
                Ok(w) => media.push(Medium { name: format!("{name}#{i}@{:x}", r.start), d: m.to_vec(), w }),
                Err(e) => *failed.entry(e.to_string()).or_default() += 1,
            }
        }
    }
    eprintln!("{} Vorbis media; not taken: {failed:?}", media.len());

    // Group by setup (and the stream's channels and block sizes).
    let mut groups: HashMap<(Stripped, usize, [u8; 2]), Vec<&Medium>> = HashMap::new();
    let mut bad = 0;
    for m in &media {
        match stripped::parse(&m.d[m.w.setup.clone()], m.w.channels) {
            Some(s) => groups.entry((s, m.w.channels, m.w.blocksize_pows)).or_default().push(m),
            None => bad += 1,
        }
    }
    eprintln!("{} setups ({bad} not parsed)", groups.len());

    let own = db_file.map(|d| aqr_vorbis::setups::Setups::parse(&d).expect("database"));
    let db = match &own {
        Some(db) => db,
        None => aqr_vorbis::setups::database().expect("database"),
    };
    // For each id, the codebook parts of the skeletons that parse a setup with it.
    let mut by_id: BTreeMap<u16, BTreeMap<u32, usize>> = BTreeMap::new();
    let mut groups: Vec<_> = groups.into_iter().collect();
    groups.sort_by_key(|g| std::cmp::Reverse(g.1.len()));
    for ((s, channels, pows), ms) in &groups {
        let ident = Ident { channels: *channels, blocksizes: [1 << pows[0], 1 << pows[1]] };
        let packets: usize = ms.iter().map(|m| m.w.packets.len()).sum();
        println!(
            "setup: {} media ({packets} packets), {channels} ch, {}..{} Hz, blocks {:?}, {} books, ends {:?}, read {}/{} bits, e.g. {}",
            ms.len(),
            ms.iter().map(|m| m.w.rate).min().unwrap(),
            ms.iter().map(|m| m.w.rate).max().unwrap(),
            ident.blocksizes,
            s.ids.len(),
            s.ends,
            s.used.0,
            s.used.1,
            ms[0].name
        );
        println!("  ids {:?}", s.ids);
        let mut hits = Vec::new();
        for (k, skel) in db.skeletons().iter().enumerate() {
            if skel.codebooks().len() != s.ids.len() {
                continue;
            }
            let books: Vec<_> = skel.codebooks().iter().map(|&b| db.codebook(b as usize)).collect();
            let bytes = s.assemble(&books);
            let setup = match Setup::parse(&bytes, ident) {
                Ok(x) => x,
                Err(e) => {
                    println!("  skeleton {k}: setup not parsed: {}", e.0);
                    continue;
                }
            };
            let (exact, tried) = score(s, &setup, ms, 400);
            println!("  skeleton {k}: {exact}/{tried} packets exact");
            if exact == tried {
                hits.push(k);
            }
        }
        if hits.is_empty() {
            let best = db.skeletons().iter().filter(|k| k.codebooks().len() == s.ids.len()).max_by_key(|k| {
                let books: Vec<_> = k.codebooks().iter().map(|&b| db.codebook(b as usize)).collect();
                Setup::parse(&s.assemble(&books), ident).map_or(0, |x| score(s, &x, ms, 400).0)
            });
            if let Some(k) = best {
                let (parts, _) = descend(db, s, ident, ms, k.codebooks());
                let books: Vec<_> = parts.iter().map(|&b| db.codebook(b as usize)).collect();
                if let Ok(setup) = Setup::parse(&s.assemble(&books), ident) {
                    let (exact, tried) = score(s, &setup, ms, usize::MAX);
                    println!("  descent, all packets: {exact}/{tried} exact; parts {parts:?}");
                    if exact == tried {
                        for (&id, &b) in s.ids.iter().zip(&parts) {
                            *by_id.entry(id).or_default().entry(b).or_default() += 1;
                        }
                    }
                }
            }
        }
        for &k in &hits {
            let books: Vec<_> = db.skeletons()[k].codebooks().iter().map(|&b| db.codebook(b as usize)).collect();
            let setup = Setup::parse(&s.assemble(&books), ident).expect("parsed before");
            let (exact, tried) = score(s, &setup, ms, usize::MAX);
            println!("  skeleton {k}, all packets: {exact}/{tried} exact");
            if exact < tried {
                diagnose(s, &setup, ms);
            }
            for (&id, &b) in s.ids.iter().zip(db.skeletons()[k].codebooks()) {
                *by_id.entry(id).or_default().entry(b).or_default() += 1;
            }
        }
    }
    println!("ids: candidates (codebook part: setups)");
    for (id, c) in &by_id {
        let set: BTreeSet<_> = c.keys().collect();
        println!("  {id}: {c:?}{}", if set.len() > 1 { "  AMBIGUOUS" } else { "" });
    }
}
