//! Tests of `pdb`: a synthetic MSF with its slack filled with a pattern, and the PDB files of
//! aqr-pdb's local corpora (where present).

use super::*;

const BS: usize = 512;

fn put(d: &mut [u8], at: usize, v: u32) {
    d[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// An MSF of block size 512: block 0 the superblock, 1 and 2 the free block maps (the first
/// active), 3 the block map, 4 the directory, then the streams, then `free` free blocks. Free
/// blocks are 0xBB, the streams' tails 0xAA, the rest of the directory and the map 0xCC.
/// Returns the file and the blocks of each stream.
fn msf(streams: &[Option<Vec<u8>>], free: usize) -> (Vec<u8>, Vec<Vec<usize>>) {
    let mut lists = Vec::new();
    let mut next = 5;
    for s in streams {
        let n = s.as_ref().map_or(0, |s| s.len().div_ceil(BS));
        lists.push((next..next + n).collect::<Vec<_>>());
        next += n;
    }
    let nb = next + free;
    let mut d = vec![0xBBu8; nb * BS];
    d[..32].copy_from_slice(b"Microsoft C/C++ MSF 7.00\r\n\x1aDS\0\0\0");
    let mut dir = Vec::new();
    dir.extend((streams.len() as u32).to_le_bytes());
    for s in streams {
        dir.extend(s.as_ref().map_or(u32::MAX, |s| s.len() as u32).to_le_bytes());
    }
    for l in &lists {
        for &b in l {
            dir.extend((b as u32).to_le_bytes());
        }
    }
    assert!(dir.len() <= BS);
    put(&mut d, 32, BS as u32);
    put(&mut d, 36, 1);
    put(&mut d, 40, nb as u32);
    put(&mut d, 44, dir.len() as u32);
    put(&mut d, 48, 0);
    put(&mut d, 52, 3);
    d[32 + 24..BS].fill(0xDD);
    // the free block maps: all free but what is used (the second, inactive, says all free)
    d[BS..3 * BS].fill(0xFF);
    for b in 0..next {
        d[BS + b / 8] &= !(1 << (b % 8));
    }
    d[3 * BS..4 * BS].fill(0xCC);
    put(&mut d, 3 * BS, 4);
    d[4 * BS..5 * BS].fill(0xCC);
    d[4 * BS..][..dir.len()].copy_from_slice(&dir);
    for (s, l) in streams.iter().zip(&lists) {
        if let Some(s) = s {
            for (k, &b) in l.iter().enumerate() {
                let n = (s.len() - k * BS).min(BS);
                d[b * BS..][..n].copy_from_slice(&s[k * BS..][..n]);
                d[b * BS + n..(b + 1) * BS].fill(0xAA);
            }
        }
    }
    (d, lists)
}

fn sample() -> (Vec<u8>, Vec<Vec<usize>>) {
    let a: Vec<u8> = (0..1300).map(|i| (i % 251) as u8 + 1).collect();
    let b: Vec<u8> = (0..BS).map(|i| (i % 7) as u8 + 1).collect();
    msf(&[Some(a), None, Some(b), Some(Vec::new()), Some(vec![9; 10])], 3)
}

fn run(x: &[u8]) -> Option<(Rewrite, Option<Vec<u8>>)> {
    rewrite(x, &parse_any("slack").unwrap(), &Options::default())
}

fn slack_of(x: &[u8]) -> Result<Option<Vec<u8>>, String> {
    pdb::slack(x, &Options::default())
}

#[test]
fn synthetic_zeroes_slack_only() {
    let (x, lists) = sample();
    let (w, r) = run(&x).unwrap();
    assert!(matches!(w, Rewrite::Made { format: Format::Pdb, .. }), "{w:?}");
    let r = r.unwrap();
    assert_eq!(r.len(), x.len());
    check::pdb(&x, &r).unwrap();
    let nb = x.len() / BS;
    // the free blocks
    for b in nb - 3..nb {
        assert!(r[b * BS..(b + 1) * BS].iter().all(|&v| v == 0));
    }
    // the first stream: 1300 bytes in 3 blocks, the tail past 1300 - 1024 = 276
    let l = &lists[0];
    assert_eq!(r[l[2] * BS..][..276], x[l[2] * BS..][..276]);
    assert!(r[l[2] * BS + 276..(l[2] + 1) * BS].iter().all(|&v| v == 0));
    assert_eq!(r[l[0] * BS..(l[0] + 1) * BS], x[l[0] * BS..(l[0] + 1) * BS]);
    // a stream of a whole block is kept, a short one has its tail zeroed
    let l = &lists[2];
    assert_eq!(r[l[0] * BS..(l[0] + 1) * BS], x[l[0] * BS..(l[0] + 1) * BS]);
    let l = &lists[4];
    assert!(r[l[0] * BS + 10..(l[0] + 1) * BS].iter().all(|&v| v == 0));
    // the superblock, the maps, the directory's used part are kept; the rest of the directory
    // and of the block map are zero
    assert_eq!(r[..3 * BS], x[..3 * BS]);
    assert_eq!(r[3 * BS..3 * BS + 4], x[3 * BS..3 * BS + 4]);
    assert!(r[3 * BS + 4..4 * BS].iter().all(|&v| v == 0));
    let dsz = u32::from_le_bytes(x[44..48].try_into().unwrap()) as usize;
    assert_eq!(r[4 * BS..][..dsz], x[4 * BS..][..dsz]);
    assert!(r[4 * BS + dsz..5 * BS].iter().all(|&v| v == 0));
    // again: nothing
    assert_eq!(slack_of(&r), Ok(None));
    assert!(run(&r).is_none());
}

#[test]
fn nothing_to_change_is_none() {
    let (mut x, _) = msf(&[Some(vec![1; 2 * BS])], 0);
    // (the superblock's rest, the block map's and the directory's)
    x[56..BS].fill(0);
    x[3 * BS + 4..4 * BS].fill(0);
    let dsz = u32::from_le_bytes(x[44..48].try_into().unwrap()) as usize;
    x[4 * BS + dsz..5 * BS].fill(0);
    assert_eq!(slack_of(&x), Ok(None));
}

#[test]
fn tampering_fails_the_check() {
    let (x, lists) = sample();
    let r = slack_of(&x).unwrap().unwrap();
    let tamper = |at: usize, v: u8| {
        let mut t = r.clone();
        t[at] = v;
        check::pdb(&x, &t)
    };
    assert!(tamper(lists[0][0] * BS + 5, 0).is_err());
    assert!(tamper(lists[0][2] * BS + 275, 0).is_err());
    assert!(tamper(4 * BS + 4, 0).is_err());
    assert!(tamper(4 * BS, 7).is_err());
    assert!(tamper(3 * BS, 6).is_err());
    assert!(tamper(BS + 100, 0).is_err());
    assert!(tamper(2 * BS + 100, 0).is_err());
    assert!(tamper(40, 0).is_err());
    assert!(tamper(lists[0][2] * BS + 300, 1).is_err(), "slack changed to a non-zero");
    let n = x.len();
    assert!(tamper(n - 1, 1).is_err());
    assert!(check::pdb(&x, &r[..n - BS]).is_err());
    // zeroed stream data
    let mut t = r.clone();
    t[lists[2][0] * BS..][..BS].fill(0);
    assert!(check::pdb(&x, &t).is_err());
    // X as R is fine
    check::pdb(&x, &x).unwrap();
}

#[test]
fn inconsistent_files_are_refused() {
    let (x, lists) = sample();
    // a block used twice: stream 2's block in stream 4's list
    let mut t = x.clone();
    let dsz = u32::from_le_bytes(x[44..48].try_into().unwrap()) as usize;
    let at = 4 * BS + dsz - 4;
    put(&mut t, at, lists[2][0] as u32);
    assert!(slack_of(&t).unwrap_err().contains("twice"));
    assert!(check::pdb(&t, &t).is_err());
    // a used block marked free
    let mut t = x.clone();
    let b = lists[2][0];
    t[BS + b / 8] |= 1 << (b % 8);
    assert!(slack_of(&t).unwrap_err().contains("free"));
    // out of range
    let mut t = x.clone();
    put(&mut t, at, 1000);
    assert!(slack_of(&t).unwrap_err().contains("out of"));
    assert!(check::pdb(&t, &t).is_err());
    // a superblock or map block used by a stream
    let mut t = x.clone();
    put(&mut t, at, 2);
    assert!(slack_of(&t).is_err());
    // the length disagrees
    assert!(slack_of(&x[..x.len() - BS]).is_err());
    let mut t = x.clone();
    t.extend([0; BS]);
    assert!(slack_of(&t).is_err());
    // the block size, the magic, the active map
    for (at, v) in [(32, 100), (32, 8192), (36, 3), (36, 0), (0, b'X' as u32)] {
        let mut t = x.clone();
        put(&mut t, at, v);
        assert!(slack_of(&t).is_err(), "{at} {v}");
    }
    // a directory that does not parse: a size that does not match its blocks
    let mut t = x.clone();
    put(&mut t, 44, dsz as u32 - 4);
    assert!(slack_of(&t).is_err());
    let mut t = x.clone();
    put(&mut t, 4 * BS, 1000);
    assert!(slack_of(&t).is_err());
    // the block map in the superblock's place
    let mut t = x.clone();
    put(&mut t, 52, 0);
    assert!(slack_of(&t).is_err());
    assert!(slack_of(&[]).is_err());
}

fn local(dirs: &[&str]) -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    for d in dirs {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../codecs/aqr-pdb/corpus").join(d);
        if let Ok(rd) = std::fs::read_dir(p) {
            v.extend(rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "pdb")));
        }
    }
    v.sort();
    v
}

#[test]
fn real_pdbs() {
    let files = local(&["local", "ms"]);
    if files.is_empty() {
        eprintln!("no PDB corpus here: skipped");
        return;
    }
    let tool = std::process::Command::new("which").arg("llvm-pdbutil").output().ok().filter(|o| o.status.success());
    let scratch = std::env::temp_dir().join(format!("aqr-lossy-pdb-{}.pdb", std::process::id()));
    for p in files {
        let x = std::fs::read(&p).unwrap();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let Some((w, r)) = run(&x) else {
            eprintln!("{name}: none ({} bytes)", x.len());
            continue;
        };
        let (Rewrite::Made { .. }, Some(r)) = (&w, r) else {
            eprintln!("{name}: {w:?}");
            continue;
        };
        check::pdb(&x, &r).unwrap();
        let zeros = |d: &[u8]| d.iter().filter(|&&b| b == 0).count();
        let changed = x.iter().zip(&r).filter(|(a, b)| a != b).count();
        eprintln!("{name}: {} bytes, {changed} zeroed ({:.2}%)", x.len(), 100.0 * changed as f64 / x.len() as f64);
        assert!(zeros(&r) >= zeros(&x));
        // aqr-pdb reads the same streams
        if let (Some((sx, ex)), Some((sr, er))) = (aqr_pdb::msf::encode_streams(&x), aqr_pdb::msf::encode_streams(&r)) {
            assert_eq!(sx[0], sr[0], "{name}: streams differ");
            assert_eq!(ex, er);
        }
        assert_eq!(slack_of(&r), Ok(None), "{name}: again");
        if tool.is_some() {
            std::fs::write(&scratch, &r).unwrap();
            let o =
                std::process::Command::new("llvm-pdbutil").args(["dump", "-summary"]).arg(&scratch).output().unwrap();
            assert!(o.status.success(), "{name}: llvm-pdbutil: {}", String::from_utf8_lossy(&o.stderr));
        }
    }
    let _ = std::fs::remove_file(scratch);
}

#[test]
fn stream_zero_may_be_marked_free() {
    // (the previous directory: its blocks are free in the map and kept)
    let (mut x, lists) = msf(&[Some(vec![5; 700]), Some(vec![6; 20])], 2);
    for &b in &lists[0] {
        x[BS + b / 8] |= 1 << (b % 8);
    }
    let r = slack_of(&x).unwrap().unwrap();
    check::pdb(&x, &r).unwrap();
    assert_eq!(r[lists[0][0] * BS..][..700.min(BS)], x[lists[0][0] * BS..][..BS]);
    assert_eq!(r[lists[0][1] * BS..][..188], x[lists[0][1] * BS..][..188]);
    // another stream's is refused
    let (mut x, lists) = msf(&[Some(vec![5; 700]), Some(vec![6; 20])], 2);
    x[BS + lists[1][0] / 8] |= 1 << (lists[1][0] % 8);
    assert!(slack_of(&x).is_err());
}
