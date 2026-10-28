use aqr_bundle::formula::{self, Format, Options};
use aqr_bundle::layout::{self, Info, Rec, Role};
use aqr_common::rng::{Rng, noise};
use aqr_formula::{Formula, Kind, Node};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn fixtures() -> Vec<(String, Vec<u8>)> {
    let mut names: Vec<String> = std::fs::read_dir(format!("{}/tests/data", env!("CARGO_MANIFEST_DIR")))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names.into_iter().map(|n| (n.clone(), fixture(&n))).collect()
}

/// The module's output from the formula's inputs: the contents (of the streams their slices
/// take) and the layout.
fn decode(f: &Formula) -> aqr_bundle::Result<Vec<u8>> {
    let Node::Apply(a) = &f.root else { panic!("not an application") };
    let ins: Vec<Vec<u8>> = a.inputs.iter().map(|n| eval(n, f)).collect();
    let ins: Vec<&[u8]> = ins.iter().map(|i| &i[..]).collect();
    formula::decode(&a.params, &ins, a.len, 1)
}

fn eval(n: &Node, f: &Formula) -> Vec<u8> {
    match n {
        &Node::Slice { stream, start, len } => f.streams[stream].data[start as usize..][..len as usize].to_vec(),
        Node::Concat(ns) => ns.iter().flat_map(|n| eval(n, f)).collect(),
        _ => panic!("{n:?}"),
    }
}

/// Encodes `d`, checking that the module gives it back; the records of its layout.
fn roundtrip_with(d: &[u8], opt: &Options) -> (Formula, Vec<Info>) {
    let f = formula::encode_with(d, opt).expect("encoded");
    assert!(decode(&f).unwrap() == d, "not rebuilt");
    let Node::Apply(a) = &f.root else { unreachable!() };
    let (data, lay) = (eval(&a.inputs[0], &f), eval(&a.inputs[1], &f));
    let mut infos = Vec::new();
    layout::build_with(&lay, &data, d.len() as u64, &mut |i| infos.push(i)).unwrap();
    (f, infos)
}

fn roundtrip(d: &[u8]) -> Vec<Info> {
    roundtrip_with(d, &Options::default()).1
}

fn members(infos: &[Info]) -> usize {
    infos.iter().filter(|i| i.tag >= layout::AR).count()
}

fn gaps(infos: &[Info]) -> usize {
    infos.iter().filter(|i| i.tag == layout::GAP).count()
}

/// Every fixture is rebuilt, all of it read into members (but for ar's global header), the
/// files' contents in the data.
#[test]
fn fixtures_rebuilt() {
    for (name, d) in fixtures() {
        let format = formula::detect(&d).unwrap_or_else(|| panic!("{name}: not detected"));
        let infos = roundtrip(&d);
        let expect_gaps = (format == Format::Ar) as usize;
        assert_eq!(gaps(&infos), expect_gaps, "{name}: {infos:?}");
        assert!(members(&infos) >= 2, "{name}: {infos:?}");
        assert_eq!(formula::extent(format, &d), Some(d.len()), "{name}");
        // the files' contents are data, as they are
        let recs = formula::records(format, &d).unwrap();
        if name == "macos.a" {
            continue;
        }
        let noise = recs
            .iter()
            .filter_map(Rec::body)
            .find(|b| b.role == Role::File && (b.name.ends_with(b"noise.bin") || b.name == b"odd.bin"));
        let noise = noise.unwrap_or_else(|| panic!("{name}: no noise.bin"));
        assert_eq!(noise.role, Role::File);
        assert_eq!(d[noise.span.clone()].len(), 1001, "{name}");
    }
}

/// What each reader tells of the members of the fixtures: names (long ones included) and roles.
#[test]
fn names_and_roles() {
    let long = "a-name-longer-than-one-hundred-bytes-".repeat(4);
    let names = |n: &str| -> Vec<(String, Role)> {
        let d = fixture(n);
        formula::records(formula::detect(&d).unwrap(), &d)
            .unwrap()
            .iter()
            .filter_map(Rec::body)
            .map(|b| (String::from_utf8_lossy(&b.name).into_owned(), b.role))
            .collect()
    };
    for f in ["bsdtar-pax.tar", "bsdtar-gnutar.tar", "py-gnu.tar", "py-pax.tar"] {
        let ns = names(f);
        assert!(ns.contains(&(format!("sub/{long}.txt"), Role::File)), "{f}: {ns:?}");
        assert!(ns.iter().any(|n| n.1 == Role::Meta), "{f}: {ns:?}");
    }
    for f in ["llvm-gnu.a", "mingw.a", "bsdtar-argnu.a"] {
        let ns = names(f);
        assert!(ns.contains(&("odd.bin".into(), Role::File)), "{f}: {ns:?}");
    }
    for f in ["llvm-gnu.a", "mingw.a"] {
        let ns = names(f);
        assert!(ns.contains(&("a-member-with-a-long-name.o".into(), Role::File)), "{f}: {ns:?}");
        assert!(ns.contains(&("/".into(), Role::Table)), "{f}: {ns:?}");
        assert!(ns.contains(&("//".into(), Role::Meta)), "{f}: {ns:?}");
    }
    for f in ["llvm-bsd.a", "macos.a"] {
        let ns = names(f);
        assert!(ns.contains(&("a-member-with-a-long-name.o".into(), Role::File)), "{f}: {ns:?}");
        assert!(ns.iter().any(|n| n.0.starts_with("__.SYMDEF") && n.1 == Role::Table), "{f}: {ns:?}");
    }
    let ns = names("py-pax-size.tar");
    assert!(ns.contains(&("noise.bin".into(), Role::File)), "{ns:?}");
    for f in ["bsdtar-newc.cpio", "bsdtar-odc.cpio", "bsdtar-bin.cpio", "bsdcpio-newc.cpio", "py-crc.cpio"] {
        let ns = names(f);
        assert!(ns.contains(&("a.txt".into(), Role::File)), "{f}: {ns:?}");
        assert_eq!(ns.last().unwrap().0, "TRAILER!!!", "{f}");
    }
}

/// The derived fields cost nothing: the layout of a tar archive is smaller than its headers.
#[test]
fn headers_predicted() {
    let d = fixture("py-gnu.tar");
    let f = formula::encode(&d, 1).unwrap();
    let Node::Apply(a) = &f.root else { unreachable!() };
    let lay = eval(&a.inputs[1], &f);
    let recs = formula::records(Format::Tar, &d).unwrap();
    // the size and checksum fields of every header XOR to zero
    let mut at = 0;
    let mut infos = 0;
    while at < lay.len() {
        match lay[at] {
            layout::TAR => {
                let flags = lay[at + 1];
                let mut p = at + 2;
                let n = aqr_common::varint::get(&lay, &mut p).unwrap() as usize;
                assert!(lay[p + 124..p + 136].iter().all(|&b| b == 0), "size");
                assert!(lay[p + 148..p + 156].iter().all(|&b| b == 0), "checksum");
                at = p + 512 + if flags & layout::INLINE != 0 { n } else { 0 };
                assert_eq!(flags & layout::PAD, 0);
                infos += 1;
            }
            layout::ZEROS => {
                let mut p = at + 1;
                aqr_common::varint::get(&lay, &mut p).unwrap();
                at = p;
            }
            t => panic!("tag {t}"),
        }
    }
    assert_eq!(infos, recs.iter().filter(|r| r.body().is_some()).count());
}

/// A tar header of `name`, of `size` in its field (as given), `ty`, and its checksum.
fn tar_header(name: &str, size: &[u8], ty: u8) -> Vec<u8> {
    let mut h = vec![0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    h[100..108].copy_from_slice(b"0000644\0");
    h[124..124 + size.len()].copy_from_slice(size);
    h[136..148].copy_from_slice(b"14123456701\0");
    h[156] = ty;
    h[257..265].copy_from_slice(b"ustar\x0000");
    let sum: u32 = h.iter().map(|&b| b as u32).sum::<u32>() + 8 * b' ' as u32;
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    h
}

/// Odd tars: sizes of other spellings and base-256, garbage in padding, a link with a size and
/// no body, a signed checksum, a truncated member, and junk after the end.
#[test]
fn odd_tars() {
    let body = noise(700);
    let pad = |d: &mut Vec<u8>, fill: u8| d.resize(d.len().next_multiple_of(512), fill);
    let mut d = Vec::new();
    for size in [&b"00000001274\0"[..], b"     1274 \0\0", b"1274", b"\x80\0\0\0\0\0\0\0\0\0\x02\xbc"] {
        d.extend(tar_header("f", size, b'0'));
        d.extend(&body);
        pad(&mut d, 0);
    }
    d.extend(tar_header("g", b"00000001274\0", b'0'));
    d.extend(&body);
    pad(&mut d, 0xaa);
    // a hard link whose size tells of no body
    d.extend(tar_header("h", b"00000001274\0", b'1'));
    // a signed checksum (a byte of the name over 127)
    let mut h = tar_header("\u{e9}", b"00000000001\0", b'0');
    let signed: i32 =
        h.iter().enumerate().map(|(i, &b)| if (148..156).contains(&i) { 32 } else { b as i8 as i32 }).sum();
    h[148..156].copy_from_slice(format!("{signed:06o}\0 ").as_bytes());
    d.extend(h);
    d.extend(b"x");
    pad(&mut d, 0);
    d.extend([0; 1024]);
    let end = d.len();
    d.extend(b"junk after the end");
    let infos = roundtrip(&d);
    assert_eq!(members(&infos), 7, "{infos:?}");
    assert_eq!(gaps(&infos), 1);
    assert_eq!(formula::extent(Format::Tar, &d), Some(end));
    // cut anywhere: rebuilt, whatever is read of it
    for cut in (512..d.len()).step_by(97) {
        if let Some(f) = formula::encode(&d[..cut], 1) {
            assert!(decode(&f).unwrap() == d[..cut], "cut at {cut}");
        }
    }
}

/// Odd ar archives: a member of odd size without its pad byte, a BSD name, junk after the end.
#[test]
fn odd_ars() {
    let member = |name: &str, size: usize| format!("{name:<16}{:<12}{:<6}{:<6}{:<8}{size:<10}`\n", 0, 0, 0, 644);
    let mut d = b"!<arch>\n".to_vec();
    d.extend(member("a/", 3).bytes());
    d.extend(b"abc");
    // no pad byte
    d.extend(member("b/", 5).bytes());
    d.extend(b"12345\n");
    d.extend(member("#1/7", 10).bytes());
    d.extend(b"c.o\0\0\0\0xyz");
    d.extend(member("d/", 1).bytes());
    d.extend(b"q\0");
    let end = d.len();
    d.extend(b"junk");
    let infos = roundtrip(&d);
    assert_eq!(members(&infos), 4, "{infos:?}");
    assert_eq!(formula::extent(Format::Ar, &d), Some(end));
    let recs = formula::records(Format::Ar, &d).unwrap();
    let names: Vec<&[u8]> = recs.iter().filter_map(Rec::body).map(|b| &b.name[..]).collect();
    assert_eq!(names, [&b"a"[..], b"b", b"c.o", b"d"]);
}

/// cpio archives one after another with zeros between, then other data.
#[test]
fn concatenated_cpio() {
    let a = fixture("bsdtar-newc.cpio");
    let b = fixture("py-crc.cpio");
    let mut d = [&a[..], &b[..], &[0x1f, 0x8b, 8, 0][..]].concat();
    d.extend(noise(100));
    let infos = roundtrip(&d);
    assert_eq!(gaps(&infos), 1);
    assert_eq!(formula::extent(Format::Cpio, &d), Some(a.len() + b.len()));
}

/// The contents are streams by kind, slices of their own past [`formula::OWN_MIN`].
#[test]
fn kinds() {
    let d = fixture("py-gnu.tar");
    let opt = Options { kind: |n| if n.ends_with(b".txt") { Kind::Text } else { Kind::Unknown }, ..Options::default() };
    let (f, _) = roundtrip_with(&d, &opt);
    let classes: Vec<&str> = f.streams.iter().map(|s| &s.class[..]).collect();
    assert_eq!(classes, ["raw", "bundle/layout", "bundle/text"]);
    // large members apart
    let mut big = Vec::new();
    for (i, n) in [100, 300 << 10, 200, 300 << 10, 50].into_iter().enumerate() {
        big.extend(tar_header(&format!("f{i}"), format!("{n:011o}\0").as_bytes(), b'0'));
        big.extend(noise(n));
        big.resize(big.len().next_multiple_of(512), 0);
    }
    big.extend([0; 1024]);
    let (f, _) = roundtrip_with(&big, &Options::default());
    let Node::Apply(a) = &f.root else { unreachable!() };
    let Node::Concat(parts) = &a.inputs[0] else { panic!("{:?}", a.inputs[0]) };
    let lens: Vec<u64> = parts.iter().map(Node::len).collect();
    assert_eq!(lens, [100, 300 << 10, 200, 300 << 10, 50]);
}

/// Nothing to take: not an archive, or one without a member.
#[test]
fn turned_down() {
    assert!(formula::encode(b"!<arch>\n", 1).is_none());
    assert!(formula::encode(&[0; 4096], 1).is_none());
    assert!(formula::encode(b"hello", 1).is_none());
    assert_eq!(formula::detect(&noise(10000)), None);
}

/// Corrupt layouts fail, and never panic.
#[test]
fn corrupt_layouts() {
    let mut rng = Rng::new(7);
    for (name, d) in fixtures() {
        let f = formula::encode(&d, 1).unwrap();
        let Node::Apply(a) = &f.root else { unreachable!() };
        let (data, lay) = (eval(&a.inputs[0], &f), eval(&a.inputs[1], &f));
        for _ in 0..300 {
            let mut l = lay.clone();
            for _ in 0..1 + rng.below(3) {
                let i = rng.below(l.len() as u64) as usize;
                l[i] = rng.u64() as u8;
            }
            if rng.below(4) == 0 {
                l.truncate(rng.below(l.len() as u64) as usize);
            }
            let out = std::panic::catch_unwind(|| formula::decode(&[], &[&data, &l], d.len() as u64, 1));
            assert!(out.is_ok(), "{name}: panicked");
        }
        assert!(formula::decode(&[], &[&data, &lay], d.len() as u64 + 1, 1).is_err());
        assert!(formula::decode(&[], &[&data[..data.len().saturating_sub(1)], &lay], d.len() as u64, 1).is_err());
        assert!(formula::decode(&[1], &[&data, &lay], d.len() as u64, 1).is_err());
    }
}
