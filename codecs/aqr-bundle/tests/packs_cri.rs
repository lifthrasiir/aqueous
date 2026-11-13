//! Packs written from the formats' descriptions: AFS2, ACB (@UTF), CPK, AFS.

use aqr_bundle::packs::*;

// ---- builders

enum Cell {
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    Str(&'static str),
    Data(Vec<u8>),
}

fn ty(c: &Cell) -> u8 {
    match c {
        Cell::U8(_) => 0,
        Cell::U16(_) => 2,
        Cell::U32(_) => 4,
        Cell::U64(_) => 6,
        Cell::Str(_) => 0xa,
        Cell::Data(_) => 0xb,
    }
}

struct Pools {
    strings: Vec<u8>,
    data: Vec<u8>,
}

impl Pools {
    fn string(&mut self, s: &str) -> u32 {
        let at = self.strings.len() as u32;
        self.strings.extend_from_slice(s.as_bytes());
        self.strings.push(0);
        at
    }

    fn put(&mut self, c: &Cell, out: &mut Vec<u8>) {
        match c {
            Cell::U8(v) => out.push(*v),
            Cell::U16(v) => out.extend(v.to_be_bytes()),
            Cell::U32(v) => out.extend(v.to_be_bytes()),
            Cell::U64(v) => out.extend(v.to_be_bytes()),
            Cell::Str(s) => {
                let at = self.string(s);
                out.extend(at.to_be_bytes());
            }
            Cell::Data(b) => {
                out.extend((self.data.len() as u32).to_be_bytes());
                out.extend((b.len() as u32).to_be_bytes());
                self.data.extend_from_slice(b);
            }
        }
    }
}

/// A table: `zeros` are columns of no storage, `consts` constant ones, `cols` per-row ones
/// (the cell gives the type); `rows` hold the cells of `cols` only.
fn utf(name: &str, zeros: &[&str], consts: &[(&str, Cell)], cols: &[(&str, Cell)], rows: &[Vec<Cell>]) -> Vec<u8> {
    let mut p = Pools { strings: Vec::new(), data: Vec::new() };
    p.string("<NULL>");
    let tname = p.string(name);
    let mut schema = Vec::new();
    for z in zeros {
        schema.push(0x10);
        schema.extend(p.string(z).to_be_bytes());
    }
    for (n, c) in consts {
        schema.push(0x30 | ty(c));
        schema.extend(p.string(n).to_be_bytes());
        p.put(c, &mut schema);
    }
    for (n, c) in cols {
        schema.push(0x50 | ty(c));
        schema.extend(p.string(n).to_be_bytes());
    }
    let mut body = Vec::new();
    let mut width = 0;
    for r in rows {
        let mut row = Vec::new();
        for c in r {
            p.put(c, &mut row);
        }
        width = row.len();
        body.extend(row);
    }
    let rows_off = 0x18 + schema.len();
    let strings_off = rows_off + body.len();
    let data_off = strings_off + p.strings.len();
    let size = data_off + p.data.len();
    let mut t = b"@UTF".to_vec();
    t.extend((size as u32).to_be_bytes());
    t.extend(1u16.to_be_bytes());
    t.extend((rows_off as u16).to_be_bytes());
    t.extend((strings_off as u32).to_be_bytes());
    t.extend((data_off as u32).to_be_bytes());
    t.extend(tname.to_be_bytes());
    t.extend(((zeros.len() + consts.len() + cols.len()) as u16).to_be_bytes());
    t.extend((width as u16).to_be_bytes());
    t.extend((rows.len() as u32).to_be_bytes());
    t.extend(schema);
    t.extend(body);
    t.extend(p.strings);
    t.extend(p.data);
    t
}

fn mask(d: &mut [u8]) {
    let mut m = 0x655fu32;
    for b in d {
        *b ^= m as u8;
        m = m.wrapping_mul(0x4115);
    }
}

fn hca(n: usize) -> Vec<u8> {
    let mut v = b"HCA\0".to_vec();
    v.extend((0..n).map(|i| i as u8));
    v
}

/// An AFS2 of `(id, bytes)`; `osz`/`isz` the sizes of offsets and ids.
fn afs2(items: &[(u32, Vec<u8>)], osz: usize, isz: usize, align: usize) -> Vec<u8> {
    let mut d = b"AFS2".to_vec();
    d.extend([2, osz as u8, isz as u8, 0]);
    d.extend((items.len() as u32).to_le_bytes());
    d.extend((align as u16).to_le_bytes());
    d.extend(0u16.to_le_bytes());
    for (id, _) in items {
        d.extend(&id.to_le_bytes()[..isz]);
    }
    let table = d.len();
    d.resize(table + (items.len() + 1) * osz, 0);
    let mut offs = Vec::new();
    for (_, b) in items {
        // The offset is where the previous file ended, before the padding.
        offs.push(d.len());
        d.resize(d.len().div_ceil(align) * align, 0);
        d.extend(b);
    }
    offs.push(d.len());
    for (i, o) in offs.iter().enumerate() {
        d[table + i * osz..table + (i + 1) * osz].copy_from_slice(&o.to_le_bytes()[..osz]);
    }
    d
}

fn names(f: &[aqr_disk::File]) -> Vec<String> {
    f.iter().map(|f| String::from_utf8_lossy(&f.name).into_owned()).collect()
}

fn bytes(d: &[u8], f: &aqr_disk::File) -> Vec<u8> {
    f.extents.iter().flat_map(|r| d[r.clone()].to_vec()).collect()
}

/// Every prefix of `d` is None or has files within the prefix.
fn cuts(d: &[u8], files: impl Fn(&[u8]) -> Option<Vec<aqr_disk::File>>) {
    for n in 0..d.len() {
        if let Some(f) = files(&d[..n]) {
            for f in f {
                assert!(f.extents.iter().all(|r| r.start <= r.end && r.end <= n));
            }
        }
    }
}

// ---- AWB

#[test]
fn awb_variants() {
    let items =
        vec![(0, hca(100)), (1, vec![0x80, 0, 1, 2, 3]), (7, b"hello".to_vec()), (9, vec![0xc8, 0xc3, 0xc1, 0x80, 1])];
    for (osz, isz, align) in [(4, 4, 0x20), (2, 2, 4), (4, 2, 1), (2, 4, 0x10)] {
        let d = afs2(&items, osz, isz, align);
        assert!(cri_awb::starts(&d));
        let f = cri_awb::files(&d).unwrap();
        assert_eq!(names(&f), ["0.hca", "1.adx", "7.bin", "9.hca"], "{osz} {isz} {align}");
        for (f, (_, b)) in f.iter().zip(&items) {
            assert_eq!(&bytes(&d, f), b);
        }
        cuts(&d, cri_awb::files);
    }
}

#[test]
fn awb_not() {
    assert!(cri_awb::files(b"QUAVMAGI\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0").is_none());
    assert!(cri_awb::files(b"AFS2").is_none());
    assert!(cri_awb::files(&[]).is_none());
    // A count far past the data.
    let mut d = afs2(&[(0, hca(8))], 4, 4, 0x20);
    d[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(cri_awb::files(&d).is_none());
}

// ---- ACB

fn acb(awb: Vec<u8>) -> Vec<u8> {
    utf(
        "Header",
        &["Zero"],
        &[("Version", Cell::U32(0x01290000))],
        &[("Name", Cell::Str("snd")), ("AwbFile", Cell::Data(vec![]))],
        &[vec![Cell::Str("snd"), Cell::Data(awb)]],
    )
}

#[test]
fn acb_embedded() {
    let items = vec![(0, hca(300)), (1, hca(50)), (2, vec![0x80, 0, 9])];
    let t = acb(afs2(&items, 4, 2, 0x20));
    assert!(cri_acb::starts(&t));
    let f = cri_acb::files(&t).unwrap();
    assert_eq!(names(&f), ["0.hca", "1.hca", "2.adx"]);
    for (f, (_, b)) in f.iter().zip(&items) {
        assert_eq!(&bytes(&t, f), b);
    }
    cuts(&t, cri_acb::files);
    // Masked: not taken.
    let mut m = t.clone();
    mask(&mut m);
    assert!(cri_utf::starts(&m));
    assert!(cri_acb::files(&m).is_none());
    assert!(cri_utf::parse(&m).unwrap().encrypted);
}

#[test]
fn acb_unreadable_awb_and_empty() {
    let f = cri_acb::files(&acb(b"QUAVMAGIxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".to_vec())).unwrap();
    assert_eq!(names(&f), ["AwbFile.awb"]);
    // The streamed ones have no embedded archive; nor have non-tables.
    assert!(cri_acb::files(&acb(vec![])).is_none());
    assert!(cri_acb::files(b"@UTF").is_none());
    assert!(cri_acb::files(b"nothing at all, really").is_none());
}

#[test]
fn utf_cells() {
    let t = utf(
        "T",
        &["z"],
        &[("c", Cell::U64(1 << 40))],
        &[("a", Cell::U8(255)), ("b", Cell::U16(7)), ("s", Cell::Str("hi"))],
        &[vec![Cell::U8(255), Cell::U16(7), Cell::Str("hi")], vec![Cell::U8(1), Cell::U16(2), Cell::Str("yo")]],
    );
    let p = cri_utf::parse(&t).unwrap();
    assert_eq!(p.rows.len(), 2);
    assert_eq!(p.num(1, "a"), Some(1));
    assert_eq!(p.num(1, "c"), Some(1 << 40));
    assert_eq!(p.str(1, "s"), Some(&b"yo"[..]));
    assert_eq!(p.get(0, "z"), Some(&cri_utf::Value::Null));
    assert_eq!(&p.bytes[p.name.clone()], b"T");
    let mut m = t.clone();
    mask(&mut m);
    let q = cri_utf::parse(&m).unwrap();
    assert!(q.encrypted);
    assert_eq!(q.str(0, "s"), Some(&b"hi"[..]));
    for n in 0..t.len() {
        let _ = cri_utf::parse(&t[..n]);
        let _ = cri_utf::parse(&m[..n]);
    }
}

// ---- CPK

struct Item {
    dir: &'static str,
    name: &'static str,
    stored: Vec<u8>,
    extract: u32,
}

fn toc_cols() -> [(&'static str, Cell); 6] {
    [
        ("DirName", Cell::Str("")),
        ("FileName", Cell::Str("")),
        ("FileSize", Cell::U32(0)),
        ("ExtractSize", Cell::U32(0)),
        ("FileOffset", Cell::U64(0)),
        ("ID", Cell::U32(0)),
    ]
}

/// A CPK: header table, TOC at 0x800, contents after the TOC (aligned 0x800). `rel_toc`: the
/// FileOffsets are from the TOC (the TOC's offset under ContentOffset), else from the contents.
fn cpk(items: &[Item], rel_toc: bool, mask_header: bool) -> Vec<u8> {
    let toc_at = 0x800usize;
    let rows = |offs: &[usize]| -> Vec<Vec<Cell>> {
        items
            .iter()
            .zip(offs)
            .enumerate()
            .map(|(i, (it, o))| {
                vec![
                    Cell::Str(it.dir),
                    Cell::Str(it.name),
                    Cell::U32(it.stored.len() as u32),
                    Cell::U32(it.extract),
                    Cell::U64(*o as u64),
                    Cell::U32(i as u32),
                ]
            })
            .collect()
    };
    let toc_len = utf("CpkTocInfo", &[], &[], &toc_cols(), &rows(&vec![0; items.len()])).len();
    let content = (toc_at + 16 + toc_len).div_ceil(0x800) * 0x800;
    let mut offs = Vec::new();
    let mut pos = 0;
    for it in items {
        offs.push(if rel_toc { content - toc_at + pos } else { pos });
        pos += it.stored.len().div_ceil(16) * 16;
    }
    let toc = utf("CpkTocInfo", &[], &[], &toc_cols(), &rows(&offs));
    let mut h = header(content as u64, toc_at as u64, 0);
    if mask_header {
        mask(&mut h);
    }
    let mut d = b"CPK \xff\0\0\0".to_vec();
    d.extend((h.len() as u32).to_le_bytes());
    d.extend([0; 4]);
    d.extend(h);
    d.resize(toc_at, 0);
    d.extend(b"TOC \xff\0\0\0");
    d.extend((toc.len() as u32).to_le_bytes());
    d.extend([0; 4]);
    d.extend(toc);
    d.resize(content, 0);
    for it in items {
        d.extend(&it.stored);
        d.resize(d.len().div_ceil(16) * 16, 0);
    }
    d
}

fn header(content: u64, toc: u64, itoc: u64) -> Vec<u8> {
    utf(
        "CpkHeader",
        &[],
        &[],
        &[
            ("ContentOffset", Cell::U64(0)),
            ("TocOffset", Cell::U64(0)),
            ("ItocOffset", Cell::U64(0)),
            ("Align", Cell::U16(0)),
        ],
        &[vec![Cell::U64(content), Cell::U64(toc), Cell::U64(itoc), Cell::U16(16)]],
    )
}

fn items() -> Vec<Item> {
    vec![
        Item { dir: "bgm", name: "a.hca", stored: hca(100), extract: 104 },
        Item { dir: "", name: "root.bin", stored: b"CRILAYLAxxxxxxxx".to_vec(), extract: 5000 },
        Item { dir: "x/y", name: "z.txt", stored: b"plain text".to_vec(), extract: 10 },
    ]
}

#[test]
fn cpk_variants() {
    for (rel_toc, masked) in [(true, false), (false, false), (true, true), (false, true)] {
        let it = items();
        let d = cpk(&it, rel_toc, masked);
        assert!(cri_cpk::starts(&d));
        let f = cri_cpk::files(&d).unwrap();
        assert_eq!(names(&f), ["bgm/a.hca", "root.bin", "x/y/z.txt"], "{rel_toc} {masked}");
        for (f, i) in f.iter().zip(&it) {
            assert_eq!(bytes(&d, f), i.stored);
        }
        assert_eq!(f[0].compressed, None);
        assert_eq!(f[1].compressed, Some(aqr_disk::Compressed::Opaque { len: 5000 }));
        cuts(&d, cri_cpk::files);
    }
}

#[test]
fn cpk_itoc() {
    let (a, b) = (hca(30), b"0123456789".to_vec());
    let sub = |rows: &[(u16, u32, u32)]| {
        let r: Vec<Vec<Cell>> = rows.iter().map(|&(i, s, e)| vec![Cell::U16(i), Cell::U32(s), Cell::U32(e)]).collect();
        let cols = [("ID", Cell::U16(0)), ("FileSize", Cell::U32(0)), ("ExtractSize", Cell::U32(0))];
        utf("CpkItocL", &[], &[], &cols, &r)
    };
    // IDs 1 (b) and 0 (a) in the table; the data in the order of IDs: a, b.
    let toc = utf(
        "CpkItocInfo",
        &["DataH"],
        &[],
        &[("DataL", Cell::Data(vec![]))],
        &[vec![Cell::Data(sub(&[(1, 10, 10), (0, 34, 34)]))]],
    );
    let content = 0x1000;
    let h = header(content, 0, 0x800);
    let mut d = b"CPK \xff\0\0\0".to_vec();
    d.extend((h.len() as u32).to_le_bytes());
    d.extend([0; 4]);
    d.extend(h);
    d.resize(0x800, 0);
    d.extend(b"ITOC\xff\0\0\0");
    d.extend((toc.len() as u32).to_le_bytes());
    d.extend([0; 4]);
    d.extend(toc);
    d.resize(content as usize, 0);
    d.extend(&a);
    d.resize(content as usize + 48, 0);
    d.extend(&b);
    let f = cri_cpk::files(&d).unwrap();
    assert_eq!(names(&f), ["0.hca", "1.bin"]);
    assert_eq!(bytes(&d, &f[0]), a);
    assert_eq!(bytes(&d, &f[1]), b);
    cuts(&d, cri_cpk::files);
}

#[test]
fn cpk_not() {
    assert!(cri_cpk::files(b"CPK ").is_none());
    assert!(cri_cpk::files(b"CPK \xff\0\0\0\0\0\0\0\0\0\0\0@UTF").is_none());
    assert!(cri_cpk::files(&[0; 64]).is_none());
}

// ---- AFS

fn afs(items: &[Vec<u8>], names: Option<&[&str]>) -> Vec<u8> {
    let n = items.len();
    let mut d = b"AFS\0".to_vec();
    d.extend((n as u32).to_le_bytes());
    let table = 8 + 8 * n + 8;
    let mut pos = table.div_ceil(0x800) * 0x800;
    let mut offs = Vec::new();
    for it in items {
        offs.push(pos);
        pos = (pos + it.len()).div_ceil(0x800) * 0x800;
    }
    for (o, it) in offs.iter().zip(items) {
        d.extend((*o as u32).to_le_bytes());
        d.extend((it.len() as u32).to_le_bytes());
    }
    match names {
        Some(_) => {
            d.extend((pos as u32).to_le_bytes());
            d.extend((48 * n as u32).to_le_bytes());
        }
        None => d.extend([0; 8]),
    }
    for (o, it) in offs.iter().zip(items) {
        d.resize(*o, 0);
        d.extend(it);
    }
    if let Some(ns) = names {
        d.resize(pos, 0);
        for (i, nm) in ns.iter().enumerate() {
            let mut e = nm.as_bytes().to_vec();
            e.resize(32, 0);
            e.extend([0; 12]);
            e.extend((items[i].len() as u32).to_le_bytes());
            d.extend(e);
        }
    }
    d
}

#[test]
fn afs_named_and_not() {
    let items = vec![hca(50), vec![0x80, 0, 5, 5], b"data".to_vec()];
    let d = afs(&items, None);
    assert!(cri_afs::starts(&d));
    let f = cri_afs::files(&d).unwrap();
    assert_eq!(names(&f), ["00000.hca", "00001.adx", "00002.bin"]);
    for (f, i) in f.iter().zip(&items) {
        assert_eq!(&bytes(&d, f), i);
    }
    cuts(&d, cri_afs::files);
    let d = afs(&items, Some(&["a.hca", "b.adx", "c.bin"]));
    let f = cri_afs::files(&d).unwrap();
    assert_eq!(names(&f), ["a.hca", "b.adx", "c.bin"]);
    cuts(&d, cri_afs::files);
    assert!(cri_afs::files(b"AFS\0\xff\xff\xff\xff").is_none());
    assert!(cri_afs::files(b"AFS").is_none());
}

#[test]
fn damaged_packs_do_not_panic() {
    let awb = afs2(&[(0, hca(40)), (1, hca(9))], 4, 4, 0x20);
    let packs = [
        acb(awb.clone()),
        awb,
        cpk(&items(), true, false),
        cpk(&items(), false, true),
        afs(&[hca(50), b"data".to_vec()], Some(&["a", "b"])),
    ];
    for d in packs {
        for i in 0..d.len().min(0x900) {
            for v in [0u8, 0xff, 0x80] {
                let mut m = d.clone();
                m[i] = v;
                for f in [cri_acb::files, cri_awb::files, cri_cpk::files, cri_afs::files] {
                    for x in f(&m).unwrap_or_default() {
                        assert!(x.extents.iter().all(|r| r.start <= r.end && r.end <= m.len()));
                    }
                }
            }
        }
    }
}
