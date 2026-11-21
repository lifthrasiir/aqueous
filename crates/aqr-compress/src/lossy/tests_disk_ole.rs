//! Tests of `disk::ole`: compound files made by a small writer, slack of stale data.

use super::*;

use std::ops::Range;

const END: u32 = 0xffff_fffe;
const FREE: u32 = 0xffff_ffff;

/// A compound file made, and where its stale bytes are.
struct Made {
    file: Vec<u8>,
    ss: usize,
    /// The stale ranges (merged), as the writer knows them.
    stale: Vec<Range<usize>>,
    /// The streams (name, contents).
    streams: Vec<(String, Vec<u8>)>,
}

fn pattern(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed) | 1).collect()
}

/// A file of sectors of `ss` bytes (512: version 3, 4096: version 4) with the streams of `lens`
/// (the first of them in a storage), stale bytes (non-zero) in every place that is free where
/// `stale` is true (zeros else).
fn make(ss: usize, lens: &[usize], stale: bool) -> Made {
    let fill = |tag: u8| if stale { tag } else { 0 };
    let streams: Vec<(String, Vec<u8>)> =
        lens.iter().enumerate().map(|(i, &n)| (format!("S{i}"), pattern(n, i as u8 + 1))).collect();
    let (big, small): (Vec<_>, Vec<_>) = streams.iter().enumerate().partition(|(_, s)| s.1.len() >= 4096);
    let nentries = 2 + streams.len();
    let nd = nentries.div_ceil(ss / 128);
    let mut sectors: Vec<Vec<u8>> = vec![vec![0; ss]; 1 + nd]; // the FAT, the directory
    let mut fat = vec![FREE; ss / 4];
    let mut stale_at: Vec<(usize, usize)> = Vec::new(); // (sector, from) to the sector's end
    let mut stale_whole: Vec<usize> = Vec::new();
    let mut starts = vec![END; streams.len()];
    fat[0] = 0xffff_fffd;
    for i in 0..nd {
        fat[1 + i] = if i + 1 < nd { 2 + i as u32 } else { END };
    }
    // appends a chain of `data`, its tail past the end of stale `tag`; the first sector
    let put = |sectors: &mut Vec<Vec<u8>>,
               fat: &mut Vec<u32>,
               stale_at: &mut Vec<(usize, usize)>,
               data: &[u8],
               tag: u8|
     -> u32 {
        let first = sectors.len() as u32;
        let n = data.len().div_ceil(ss);
        for i in 0..n {
            let mut s = vec![fill(tag); ss];
            let part = &data[i * ss..((i + 1) * ss).min(data.len())];
            s[..part.len()].copy_from_slice(part);
            if part.len() < ss {
                stale_at.push((sectors.len(), part.len()));
            }
            fat[sectors.len()] = if i + 1 < n { sectors.len() as u32 + 1 } else { END };
            sectors.push(s);
        }
        first
    };
    // the mini stream: the small ones, a free mini sector between the first and the rest
    let mut mini: Vec<u8> = Vec::new();
    let mut mini_fat = vec![FREE; ss / 4];
    let mut mini_stale: Vec<Range<usize>> = Vec::new();
    for (k, &(i, s)) in small.iter().enumerate() {
        if k == 1 {
            mini_stale.push(mini.len()..mini.len() + 64);
            mini.extend(vec![fill(0xcc); 64]);
        }
        let n = s.1.len().div_ceil(64);
        let m0 = mini.len() / 64;
        for j in 0..n {
            mini_fat[m0 + j] = if j + 1 < n { (m0 + j + 1) as u32 } else { END };
        }
        starts[i] = m0 as u32;
        mini.extend(&s.1);
        let pad = n * 64 - s.1.len();
        if pad > 0 {
            mini_stale.push(mini.len()..mini.len() + pad);
        }
        mini.extend(vec![fill(0xdd); pad]);
    }
    // the mini FAT, then the mini stream's sectors
    let mini_fat_bytes: Vec<u8> = mini_fat.iter().flat_map(|v| v.to_le_bytes()).collect();
    let (mfs, mfc) =
        if small.is_empty() { (END, 0) } else { (put(&mut sectors, &mut fat, &mut stale_at, &mini_fat_bytes, 0), 1) };
    let mini_first = (!mini.is_empty()).then(|| put(&mut sectors, &mut fat, &mut stale_at, &mini, 0xbb));
    // free sectors, the big streams, a free sector
    for _ in 0..2 {
        stale_whole.push(sectors.len());
        sectors.push(vec![fill(0xaa); ss]);
    }
    for (k, &(i, s)) in big.iter().enumerate() {
        starts[i] = put(&mut sectors, &mut fat, &mut stale_at, &s.1, 0x99);
        if k == 0 {
            stale_whole.push(sectors.len());
            sectors.push(vec![fill(0xaa); ss]);
        }
    }
    stale_whole.push(sectors.len());
    sectors.push(vec![fill(0xaa); ss]);
    // the directory
    let mut dir = vec![0u8; nd * ss];
    let mut entry = |i: usize, name: &str, kind: u8, right: u32, child: u32, start: u32, size: u64| {
        let e = &mut dir[128 * i..128 * i + 128];
        for (j, u) in name.encode_utf16().enumerate() {
            e[2 * j..2 * j + 2].copy_from_slice(&u.to_le_bytes());
        }
        e[64..66].copy_from_slice(&(2 * name.encode_utf16().count() as u16 + 2).to_le_bytes());
        e[66] = kind;
        e[68..72].copy_from_slice(&FREE.to_le_bytes());
        e[72..76].copy_from_slice(&right.to_le_bytes());
        e[76..80].copy_from_slice(&child.to_le_bytes());
        e[116..120].copy_from_slice(&start.to_le_bytes());
        e[120..128].copy_from_slice(&size.to_le_bytes());
    };
    entry(0, "Root Entry", 5, FREE, 1, mini_first.unwrap_or(END), mini.len() as u64);
    entry(1, "Storage", 1, FREE, if streams.is_empty() { FREE } else { 2 }, 0, 0);
    for (i, s) in streams.iter().enumerate() {
        let right = if i + 1 < streams.len() { 3 + i as u32 } else { FREE };
        entry(2 + i, &s.0, 2, right, FREE, starts[i], s.1.len() as u64);
    }
    for i in 0..nd {
        sectors[1 + i] = dir[i * ss..(i + 1) * ss].to_vec();
    }
    sectors[0] = fat.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut file = vec![0u8; ss];
    file[..8].copy_from_slice(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]);
    file[24..26].copy_from_slice(&0x3eu16.to_le_bytes());
    let (version, shift) = if ss == 512 { (3u16, 9u16) } else { (4, 12) };
    file[26..28].copy_from_slice(&version.to_le_bytes());
    file[28..30].copy_from_slice(&0xfffeu16.to_le_bytes());
    file[30..32].copy_from_slice(&shift.to_le_bytes());
    file[32..34].copy_from_slice(&6u16.to_le_bytes());
    if ss == 4096 {
        file[40..44].copy_from_slice(&(nd as u32).to_le_bytes());
    }
    file[44..48].copy_from_slice(&1u32.to_le_bytes());
    file[48..52].copy_from_slice(&1u32.to_le_bytes());
    file[56..60].copy_from_slice(&4096u32.to_le_bytes());
    file[60..64].copy_from_slice(&mfs.to_le_bytes());
    file[64..68].copy_from_slice(&(mfc as u32).to_le_bytes());
    file[68..72].copy_from_slice(&END.to_le_bytes());
    file[76..80].copy_from_slice(&0u32.to_le_bytes());
    file[80..76 + 109 * 4].fill(0xff);
    // the mini stream's place in the file
    let mut extra: Vec<Range<usize>> = Vec::new();
    let mini_sectors = mini_first.map(|f| f as usize);
    for r in &mini_stale {
        let at = (mini_sectors.expect("a mini stream") + 1) * ss + r.start;
        extra.push(at..at + r.len());
    }
    for s in &sectors {
        file.extend(s);
    }
    let mut stale: Vec<Range<usize>> = stale_at
        .iter()
        .map(|&(s, from)| (s + 1) * ss + from..(s + 2) * ss)
        .chain(stale_whole.iter().map(|&s| (s + 1) * ss..(s + 2) * ss))
        .chain(extra)
        .collect();
    stale.retain(|r| !r.is_empty());
    stale.sort_by_key(|r| r.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for r in stale {
        match merged.last_mut() {
            Some(l) if l.end == r.start => l.end = r.end,
            _ => merged.push(r),
        }
    }
    Made { file, ss, stale: merged, streams }
}

/// The streams of compound file `d` by a plain reader (one FAT sector), in the directory's order.
fn read(d: &[u8]) -> Vec<(String, Vec<u8>)> {
    let le32 = |b: &[u8], p: usize| u32::from_le_bytes(b[p..p + 4].try_into().unwrap());
    let ss = if d[30] == 9 { 512 } else { 4096 };
    let sec = |n: u32| &d[(n as usize + 1) * ss..(n as usize + 2) * ss];
    let fat_sector = le32(d, 76);
    let fat: Vec<u32> = (0..ss / 4).map(|i| le32(sec(fat_sector), 4 * i)).collect();
    let chain = |mut s: u32, fat: &[u32]| {
        let mut out = Vec::new();
        while s < 0xffff_fff0 {
            out.push(s);
            s = fat[s as usize];
        }
        out
    };
    let dir: Vec<u8> = chain(le32(d, 48), &fat).iter().flat_map(|&n| sec(n).to_vec()).collect();
    let mini_fat: Vec<u32> =
        chain(le32(d, 60), &fat).iter().flat_map(|&n| (0..ss / 4).map(move |i| le32(sec(n), 4 * i))).collect();
    let root = &dir[..128];
    let mini: Vec<u8> = chain(le32(root, 116), &fat).iter().flat_map(|&n| sec(n).to_vec()).collect();
    let mut out = Vec::new();
    for e in dir.chunks(128).skip(1).filter(|e| e[66] == 2) {
        let units: Vec<u16> = (0..(u16::from_le_bytes([e[64], e[65]]) as usize / 2 - 1))
            .map(|i| u16::from_le_bytes([e[2 * i], e[2 * i + 1]]))
            .collect();
        let (start, size) = (le32(e, 116), u64::from_le_bytes(e[120..128].try_into().unwrap()) as usize);
        let data: Vec<u8> = if size >= 4096 {
            chain(start, &fat).iter().flat_map(|&n| sec(n).to_vec()).take(size).collect()
        } else {
            chain(start, &mini_fat)
                .iter()
                .flat_map(|&m| mini[m as usize * 64..m as usize * 64 + 64].to_vec())
                .take(size)
                .collect()
        };
        assert_eq!(data.len(), size);
        out.push((String::from_utf16(&units).unwrap(), data));
    }
    out
}

fn slack(d: &[u8]) -> Option<(Rewrite, Option<Vec<u8>>)> {
    rewrite(d, &Lossy::parse("slack").unwrap(), &Options::default())
}

fn files(d: &[u8]) -> Vec<(Vec<u8>, Vec<Range<usize>>)> {
    aqr_disk::files(d, crate::bundle::FILE_SYSTEMS).into_iter().map(|f| (f.name, f.extents)).collect()
}

/// Everything holds for `m`: R zeros exactly the stale bytes, everything read is kept.
fn holds(m: &Made) {
    let (made, r) = slack(&m.file).expect("slack is found");
    let r = r.unwrap_or_else(|| panic!("not made: {made:?}"));
    assert!(matches!(made, Rewrite::Made { .. }), "{made:?}");
    assert_eq!(r.len(), m.file.len());
    check::disk(&m.file, &r).unwrap();
    let mut expect = m.file.clone();
    for s in &m.stale {
        expect[s.clone()].fill(0);
    }
    assert!(expect == r, "R is not X with its stale bytes zeroed");
    assert!(m.stale.iter().map(|s| s.len()).sum::<usize>() > 0);
    assert_eq!(read(&m.file), m.streams);
    assert_eq!(read(&r), m.streams);
    assert_eq!(files(&m.file), files(&r));
    assert_eq!(files(&r).len(), m.streams.iter().filter(|s| s.1.len() >= 4096).count());
    assert_eq!(m.ss, if m.file[30] == 9 { 512 } else { 4096 });
}

#[test]
fn slack_of_version_3() {
    // (1 mini, 3 minis, 2 bigs, one of exactly 5 sectors, a tiny one)
    holds(&make(512, &[100, 4096 + 700, 1, 9000, 2560 + 1536, 70], true));
}

#[test]
fn slack_of_version_4() {
    holds(&make(4096, &[100, 5000, 3, 12288, 4000], true));
}

#[test]
fn only_mini_or_only_big() {
    holds(&make(512, &[10, 200, 3000], true));
    holds(&make(512, &[4097], true));
}

#[test]
fn nothing_to_zero() {
    for ss in [512, 4096] {
        let m = make(ss, &[100, 5000, 3, 4096, 9000], false);
        assert!(slack(&m.file).is_none());
    }
}

/// The file of a tampered `make`, with what rewrite makes of it: kept as it is.
fn kept(d: &[u8], reason: &str) {
    match slack(d) {
        Some((Rewrite::Kept { reason: r, .. }, None)) => assert!(r.contains(reason), "{r}"),
        other => panic!("{other:?}"),
    }
}

fn set32(d: &mut [u8], at: usize, v: u32) {
    d[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

#[test]
fn chain_loop_is_turned_down() {
    let mut m = make(512, &[5000, 6000], true);
    // sector 0 the FAT; the loop: the last sector of the first big stream to its first
    let fat_at = 512;
    let ends: Vec<usize> = (0..128).filter(|&i| le(&m.file, fat_at + 4 * i) == END).collect();
    let big_end = *ends.iter().rev().nth(1).unwrap();
    let big_start = big_end - 9;
    set32(&mut m.file, fat_at + 4 * big_end, big_start as u32);
    kept(&m.file, "looping");
}

fn le(d: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(d[p..p + 4].try_into().unwrap())
}

#[test]
fn double_claim_is_turned_down() {
    let mut m = make(512, &[5000, 6000], true);
    // the second stream's entry starts where the first does (entry 3 of the directory sector 1)
    let first = le(&m.file, 2 * 512 + 128 * 2 + 116);
    set32(&mut m.file, 2 * 512 + 128 * 3 + 116, first);
    kept(&m.file, "crossing");
}

#[test]
fn fat_count_mismatch_is_turned_down() {
    let mut m = make(512, &[5000, 6000, 100], true);
    set32(&mut m.file, 44, 2); // two FAT sectors, one listed
    kept(&m.file, "FAT sector");
    let mut m = make(512, &[5000, 6000, 100], true);
    set32(&mut m.file, 72, 1); // a DIFAT sector the FAT does not need
    kept(&m.file, "DIFAT");
}

#[test]
fn size_and_chain_disagree_is_turned_down() {
    let mut m = make(512, &[5000, 6000], true);
    // the first stream's size, 5000 -> 6000: its chain is of 10 sectors, the size takes 12
    m.file[2 * 512 + 128 * 2 + 120..][..8].copy_from_slice(&6000u64.to_le_bytes());
    kept(&m.file, "size");
}

#[test]
fn orphan_chain_is_turned_down() {
    let mut m = make(512, &[5000, 6000], true);
    // the second stream forgotten by its entry: its sectors in the FAT, held by none
    m.file[2 * 512 + 128 * 3 + 66] = 0;
    kept(&m.file, "no chain");
}

#[test]
fn mini_fat_past_stream_is_turned_down() {
    let mut m = make(512, &[100, 200], true);
    // the mini FAT is the sector after the directory (sector 2): an entry past the 7 mini sectors
    set32(&mut m.file, 3 * 512 + 4 * 50, 1);
    kept(&m.file, "past the mini stream");
}

#[test]
fn partial_sector_and_header_kept() {
    let mut m = make(512, &[100, 5000], true);
    m.file.extend(pattern(300, 5));
    let (_, r) = slack(&m.file).unwrap();
    let r = r.unwrap();
    assert_eq!(r[r.len() - 300..], m.file[m.file.len() - 300..]);
    assert_eq!(r[..512], m.file[..512]);
    check::disk(&m.file, &r).unwrap();
}

#[test]
fn olefile_reads_it() {
    let py = std::process::Command::new("python3").args(["-c", "import olefile"]).output();
    if !py.is_ok_and(|o| o.status.success()) {
        return;
    }
    let m = make(512, &[100, 5000, 70], true);
    let (_, r) = slack(&m.file).unwrap();
    let p = std::env::temp_dir().join(format!("aqr-ole-{}.bin", std::process::id()));
    std::fs::write(&p, r.unwrap()).unwrap();
    let out = std::process::Command::new("python3")
        .args(["-c", "import olefile,sys\no=olefile.OleFileIO(sys.argv[1])\nfor n in o.listdir(): print('/'.join(n), len(o.openstream(n).read()))", p.to_str().unwrap()])
        .output()
        .unwrap();
    std::fs::remove_file(&p).unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    for (n, d) in &m.streams {
        assert!(text.contains(&format!("{n} {}", d.len())), "{text}");
    }
}
