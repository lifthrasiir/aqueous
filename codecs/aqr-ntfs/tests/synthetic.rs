//! A volume made here, of what the fixture of ntfs-3g lacks: attribute lists, the runs of a
//! stream (and of the MFT itself) in extension records, an initialized size short of the data.

// (an extent of one range is a list of one range, not a range of indices)
#![allow(clippy::single_range_in_vec_init)]

const CLUSTER: usize = 4096;
const RECORD: usize = 1024;

/// Mapping pairs of `runs` (clusters, first cluster), of one byte of length and two of offset.
fn mapping(runs: &[(u64, u64)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut last = 0i64;
    for &(n, lcn) in runs {
        out.push(0x21);
        out.push(n as u8);
        out.extend(((lcn as i64 - last) as i16).to_le_bytes());
        last = lcn as i64;
    }
    out.push(0);
    out
}

fn pad8(v: &mut Vec<u8>) {
    v.resize(v.len().div_ceil(8) * 8, 0);
}

/// A `$FILE_NAME` attribute of `name` in directory record `parent` (Win32's namespace).
fn file_name(parent: u64, name: &str) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let mut value = vec![0u8; 66];
    value[..8].copy_from_slice(&(parent | 1 << 48).to_le_bytes());
    value[64] = units.len() as u8;
    value[65] = 1;
    units.iter().for_each(|u| value.extend(u.to_le_bytes()));
    let mut a = vec![0u8; 24];
    a[..4].copy_from_slice(&0x30u32.to_le_bytes());
    a[16..20].copy_from_slice(&(value.len() as u32).to_le_bytes());
    a[20..22].copy_from_slice(&24u16.to_le_bytes());
    a.extend(value);
    pad8(&mut a);
    let n = a.len() as u32;
    a[4..8].copy_from_slice(&n.to_le_bytes());
    a
}

/// A non-resident `$DATA` attribute named `name`, of VCNs from `vcn`, its runs `runs`, of `data`
/// and `initialized` bytes.
fn data(name: &str, vcn: u64, runs: &[(u64, u64)], data: u64, initialized: u64) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let mut a = vec![0u8; 64];
    a[..4].copy_from_slice(&0x80u32.to_le_bytes());
    a[8] = 1;
    a[9] = units.len() as u8;
    a[10..12].copy_from_slice(&64u16.to_le_bytes());
    units.iter().for_each(|u| a.extend(u.to_le_bytes()));
    pad8(&mut a);
    let at = a.len() as u16;
    a[32..34].copy_from_slice(&at.to_le_bytes());
    let clusters: u64 = runs.iter().map(|r| r.0).sum();
    a[16..24].copy_from_slice(&vcn.to_le_bytes());
    a[24..32].copy_from_slice(&(vcn + clusters - 1).to_le_bytes());
    a[40..48].copy_from_slice(&(data.div_ceil(CLUSTER as u64) * CLUSTER as u64).to_le_bytes());
    a[48..56].copy_from_slice(&data.to_le_bytes());
    a[56..64].copy_from_slice(&initialized.to_le_bytes());
    a.extend(mapping(runs));
    pad8(&mut a);
    let n = a.len() as u32;
    a[4..8].copy_from_slice(&n.to_le_bytes());
    a
}

/// A record of `attrs` (in use; a directory where `directory`), an extension record of `base`
/// (its reference: the record and its sequence number) where it is not 0, its fixups applied.
fn record(base: u64, directory: bool, attrs: &[Vec<u8>]) -> Vec<u8> {
    let mut r = vec![0u8; RECORD];
    r[..4].copy_from_slice(b"FILE");
    r[4..6].copy_from_slice(&48u16.to_le_bytes());
    r[6..8].copy_from_slice(&3u16.to_le_bytes());
    r[20..22].copy_from_slice(&56u16.to_le_bytes());
    r[22..24].copy_from_slice(&(1 | (directory as u16) << 1).to_le_bytes());
    r[32..40].copy_from_slice(&base.to_le_bytes());
    let mut at = 56;
    for a in attrs {
        r[at..at + a.len()].copy_from_slice(a);
        at += a.len();
    }
    r[at..at + 4].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
    at += 8;
    r[24..28].copy_from_slice(&(at as u32).to_le_bytes());
    r[28..32].copy_from_slice(&(RECORD as u32).to_le_bytes());
    // (the update sequence number 1, the bytes it takes the place of in the array)
    r[48..50].copy_from_slice(&1u16.to_le_bytes());
    for i in 1..3 {
        let p = i * 512 - 2;
        r[48 + 2 * i] = r[p];
        r[48 + 2 * i + 1] = r[p + 1];
        r[p..p + 2].copy_from_slice(&1u16.to_le_bytes());
    }
    r
}

/// The volume: 64 clusters; the MFT of 32 records at clusters 2 to 7 (records 0 to 23) and 20 to
/// 21 (24 to 31: runs in extension record 18). Record 17 is `docs/big.bin`, whose runs go on in
/// extension record 25; record 19 `a.txt` with a stream `s`; record 20 `init.bin`, initialized to
/// 5000 of its 8192 bytes. Each cluster of data is filled with its number.
fn volume() -> Vec<u8> {
    let mut d = vec![0u8; 64 * CLUSTER];
    for (c, chunk) in d.chunks_mut(CLUSTER).enumerate() {
        chunk.fill(c as u8);
    }
    let b = &mut d[..512];
    b.fill(0);
    b[..3].copy_from_slice(&[0xeb, 0x52, 0x90]);
    b[3..11].copy_from_slice(b"NTFS    ");
    b[11..13].copy_from_slice(&512u16.to_le_bytes());
    b[13] = 8;
    b[21] = 0xf8;
    b[40..48].copy_from_slice(&(64 * 8u64 - 1).to_le_bytes());
    b[48..56].copy_from_slice(&2u64.to_le_bytes());
    b[64] = 0xf6;
    b[510..512].copy_from_slice(&[0x55, 0xaa]);
    let mut records: Vec<(usize, Vec<u8>)> = vec![
        (0, record(0, false, &[file_name(5, "$MFT"), data("", 0, &[(6, 2)], 32 * RECORD as u64, 32 * RECORD as u64)])),
        (5, record(0, true, &[file_name(5, ".")])),
        (16, record(0, true, &[file_name(5, "docs")])),
        (
            17,
            record(
                0,
                false,
                &[file_name(16, "big.bin"), data("", 0, &[(1, 30), (1, 32)], 5 * 4096 - 100, 5 * 4096 - 100)],
            ),
        ),
        (18, record(1 << 48, false, &[data("", 6, &[(2, 20)], 0, 0)])),
        (19, record(0, false, &[file_name(5, "a.txt"), data("s", 0, &[(1, 50)], 3000, 3000)])),
        (20, record(0, false, &[file_name(5, "init.bin"), data("", 0, &[(2, 52)], 8192, 5000)])),
        (25, record(17 | 1 << 48, false, &[data("", 2, &[(1, 34), (2, 40)], 0, 0)])),
    ];
    for (i, r) in records.drain(..) {
        let at = match i {
            ..24 => 2 * CLUSTER + i * RECORD,
            _ => 20 * CLUSTER + (i - 24) * RECORD,
        };
        d[at..at + RECORD].copy_from_slice(&r);
    }
    d
}

#[test]
fn extension_records() {
    let d = volume();
    let mut files: Vec<_> = aqr_ntfs::files(&aqr_disk::Plain(&d))
        .into_iter()
        .map(|f| (String::from_utf8(f.name).unwrap(), f.extents))
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let c = |n: usize| n * CLUSTER;
    assert_eq!(
        files,
        [
            ("a.txt:s".to_string(), vec![c(50)..c(50) + 3000]),
            ("docs/big.bin".into(), vec![c(30)..c(31), c(32)..c(33), c(34)..c(35), c(40)..c(41) + 4096 - 100]),
            ("init.bin".into(), vec![c(52)..c(52) + 5000]),
        ]
    );
    let opt = aqr_disk::formula::Options { file_systems: &[aqr_ntfs::FS], kind: |_| aqr_formula::Kind::Unknown };
    let f = aqr_disk::formula::encode_with(&d, &opt).unwrap();
    let none = |_: &aqr_formula::Apply, _: &[&[u8]]| -> aqr_formula::Result<Vec<u8>> { unreachable!("no module") };
    assert!(f.eval(&none).unwrap() == d);
}

/// A record whose fixups do not match is not read: its file is left in the rest.
#[test]
fn fixups_checked() {
    let mut d = volume();
    // (the end of record 19's first 512 bytes)
    let at = 2 * CLUSTER + 19 * RECORD + 510;
    d[at] ^= 1;
    let files = aqr_ntfs::files(&aqr_disk::Plain(&d));
    assert!(files.iter().all(|f| f.name != b"a.txt:s"));
    assert_eq!(files.len(), 2);
}
