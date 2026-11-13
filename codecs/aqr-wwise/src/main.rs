//! `aqr-wwise ids FILE...`: the Vorbis media of `.wem` files and of banks (`.bnk`), their
//! stripped setups grouped, and the codebooks of aqr-fsb's database that their ids may stand for:
//! the database's skeletons with as many codebooks tried on each setup, by whether its packets
//! parse to their ends.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use aqr_vorbis::packet;
use aqr_vorbis::setup::{Ident, Setup};
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

struct Medium<'a> {
    name: String,
    d: &'a [u8],
    w: wem::Wem,
}

/// How a setup parses a group's packets: (parsed exactly, tried).
fn score(s: &Stripped, setup: &Setup, media: &[&Medium], limit: usize) -> (usize, usize) {
    let (mut exact, mut tried) = (0, 0);
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
            let Some((p, added)) = stripped::rebuild(s, d, prev, long(i + 1)) else { continue };
            let Ok(v) = packet::parse(setup, &p) else { continue };
            let consumed = p.len() * 8 - v.tail.len() - added;
            let left = (d.len() * 8).checked_sub(consumed);
            if !v.short && v.tail.iter().all(|&b| b == 0) && left.is_some_and(|l| l < 8) {
                exact += 1;
            }
        }
    }
    (exact, tried)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("ids") {
        eprintln!("usage: aqr-wwise ids FILE...");
        std::process::exit(2);
    }
    let files: Vec<(String, Vec<u8>)> =
        args[1..].iter().map(|p| (p.clone(), std::fs::read(p).unwrap_or_else(|e| panic!("{p}: {e}")))).collect();
    let mut media = Vec::new();
    let mut failed: BTreeMap<&str, usize> = BTreeMap::new();
    for (name, d) in &files {
        let ranges = if d.starts_with(b"BKHD") { bank_media(d) } else { std::iter::once(0..d.len()).collect() };
        for (i, r) in ranges.into_iter().enumerate() {
            let m = &d[r];
            match wem::parse(m) {
                Ok(w) => media.push(Medium { name: format!("{name}#{i}"), d: m, w }),
                Err(e) => *failed.entry(e).or_default() += 1,
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

    let db = aqr_fsb::setups::database().expect("database");
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
        for &k in &hits {
            let books: Vec<_> = db.skeletons()[k].codebooks().iter().map(|&b| db.codebook(b as usize)).collect();
            let setup = Setup::parse(&s.assemble(&books), ident).expect("parsed before");
            let (exact, tried) = score(s, &setup, ms, usize::MAX);
            println!("  skeleton {k}, all packets: {exact}/{tried} exact");
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
