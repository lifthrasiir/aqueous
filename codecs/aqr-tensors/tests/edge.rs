//! Edge cases of the containers (ZIP64 `torch.save` files, header lies, damaged inputs): never a
//! panic, a hang or an allocation the data cannot hold; a file taken is rebuilt as it is.

use aqr_formula::{Formula, Node};
use aqr_tensors::formula::{self, MODULE};
use aqr_tensors::module::decode;
use aqr_testkit::Refusing;
use std::time::Duration;

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 30);

fn eval(f: &Formula) -> Vec<u8> {
    let Node::Concat(nodes) = &f.root else { panic!("not a concatenation") };
    let mut out = Vec::new();
    for n in nodes {
        match n {
            Node::Apply(a) => {
                assert_eq!(a.module, MODULE);
                let ins: Vec<&[u8]> = (a.inputs.iter())
                    .map(|n| match n {
                        &Node::Slice { stream, start, len } => {
                            &f.streams[stream].data[start as usize..][..len as usize]
                        }
                        _ => panic!("not a slice"),
                    })
                    .collect();
                out.extend(decode(&a.params, &ins, a.len, 1).unwrap());
            }
            &Node::Slice { stream, start, len } => {
                out.extend_from_slice(&f.streams[stream].data[start as usize..][..len as usize])
            }
            _ => panic!("a node of another kind"),
        }
    }
    out
}

/// `d` is not taken, or rebuilt as it is.
fn holds(d: &[u8]) {
    if let Some(f) = formula::encode(d, 1) {
        assert!(eval(&f) == d, "rebuilt otherwise");
    }
}

fn u16_at(d: &[u8], at: usize) -> usize {
    u16::from_le_bytes([d[at], d[at + 1]]) as usize
}

fn u32_at(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(d[at..at + 4].try_into().unwrap())
}

/// Which fields of the central directory go to ZIP64's extra field.
#[derive(Clone, Copy)]
struct Z64 {
    sizes: bool,
    offset: bool,
    /// The end record's too: count and directory's place (with the locator and the record).
    end: bool,
}

/// `d` (made by `synth::pth`: no comment, no extras in the central directory) with its central
/// directory in ZIP64's form.
fn zip64(d: &[u8], z: Z64) -> Vec<u8> {
    let eocd = d.len() - 22;
    assert_eq!(&d[eocd..eocd + 4], b"PK\x05\x06");
    let (count, cd) = (u16_at(d, eocd + 10), u32_at(d, eocd + 16) as usize);
    let mut out = d[..cd].to_vec();
    let mut at = cd;
    for _ in 0..count {
        assert_eq!(&d[at..at + 4], b"PK\x01\x02");
        let nl = u16_at(d, at + 28);
        let mut e = d[at..at + 46 + nl].to_vec();
        let (size, local) = (u32_at(d, at + 24) as u64, u32_at(d, at + 42) as u64);
        let mut extra = Vec::new();
        if z.sizes {
            e[20..28].fill(0xff);
            extra.extend_from_slice(&size.to_le_bytes());
            extra.extend_from_slice(&size.to_le_bytes());
        }
        if z.offset {
            e[42..46].fill(0xff);
            extra.extend_from_slice(&local.to_le_bytes());
        }
        if !extra.is_empty() {
            // (an unknown field before it)
            let mut x = vec![0x55, 0x55, 2, 0, 9, 9, 1, 0];
            x.extend_from_slice(&(extra.len() as u16).to_le_bytes());
            x.extend_from_slice(&extra);
            e[30..32].copy_from_slice(&(x.len() as u16).to_le_bytes());
            e.extend_from_slice(&x);
        }
        out.extend_from_slice(&e);
        at += 46 + nl;
    }
    let end = out.len();
    let cd_len = end - cd;
    let mut tail = d[eocd..].to_vec();
    if z.end {
        let mut r = b"PK\x06\x06".to_vec();
        r.extend_from_slice(&44u64.to_le_bytes());
        r.extend_from_slice(&[45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        r.extend_from_slice(&(count as u64).to_le_bytes());
        r.extend_from_slice(&(count as u64).to_le_bytes());
        r.extend_from_slice(&(cd_len as u64).to_le_bytes());
        r.extend_from_slice(&(cd as u64).to_le_bytes());
        out.extend_from_slice(&r);
        out.extend_from_slice(b"PK\x06\x07\0\0\0\0");
        out.extend_from_slice(&(end as u64).to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        tail[8..12].fill(0xff);
        tail[16..20].fill(0xff);
    } else {
        tail[12..16].copy_from_slice(&(cd_len as u32).to_le_bytes());
    }
    out.extend_from_slice(&tail);
    out
}

fn coded_bytes(f: &Formula) -> usize {
    f.streams.iter().filter(|s| s.class != formula::HEADER).map(|s| s.data.len()).sum()
}

/// ZIP64 central directories (sizes, offsets, the end record, each alone and together): the same
/// tensors as the file of the plain one, rebuilt.
#[test]
fn pth_zip64() {
    let d = aqr_tensors::synth::pth(3, 2);
    let plain = aqr_tensors::tensors::parse(&d).unwrap();
    let f0 = formula::encode(&d, 1).unwrap();
    for (name, z) in [
        ("sizes", Z64 { sizes: true, offset: false, end: false }),
        ("offsets", Z64 { sizes: false, offset: true, end: false }),
        ("both", Z64 { sizes: true, offset: true, end: false }),
        ("end", Z64 { sizes: false, offset: false, end: true }),
        ("all", Z64 { sizes: true, offset: true, end: true }),
    ] {
        let v = zip64(&d, z);
        let t = aqr_tensors::tensors::parse(&v).unwrap_or_else(|| panic!("{name}: not parsed"));
        assert_eq!(t.tensors.len(), plain.tensors.len(), "{name}");
        let f = formula::encode(&v, 2).unwrap_or_else(|| panic!("{name}: not taken"));
        assert!(eval(&f) == v, "{name}");
        // (taken as well as the plain)
        assert!(coded_bytes(&f) <= coded_bytes(&f0) + 1024, "{name}: {} against {}", coded_bytes(&f), coded_bytes(&f0));
    }
}

/// ZIP64's variants that do not hold: the file is not taken, or rebuilt as it is.
#[test]
fn pth_zip64_malformed() {
    let d = aqr_tensors::synth::pth(3, 1);
    let v = zip64(&d, Z64 { sizes: true, offset: true, end: true });
    let n = v.len();
    let loc = n - 22 - 20;
    let rec = loc - 56;
    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    let mut m = |name, f: &dyn Fn(&mut Vec<u8>)| {
        let mut c = v.clone();
        f(&mut c);
        cases.push((name, c));
    };
    m("no locator", &|c| c[loc..loc + 4].fill(0));
    m("no record", &|c| c[rec..rec + 4].fill(0));
    m("record past the end", &|c| c[loc + 8..loc + 16].copy_from_slice(&u64::MAX.to_le_bytes()));
    m("record at the end", &|c| c[loc + 8..loc + 16].copy_from_slice(&(n as u64).to_le_bytes()));
    m("record in the last bytes", &|c| c[loc + 8..loc + 16].copy_from_slice(&(n as u64 - 2).to_le_bytes()));
    m("count huge", &|c| c[rec + 32..rec + 40].copy_from_slice(&u64::MAX.to_le_bytes()));
    m("count past the directory", &|c| c[rec + 32..rec + 40].copy_from_slice(&(1u64 << 40).to_le_bytes()));
    m("count one short", &|c| c[rec + 32..rec + 40].copy_from_slice(&1u64.to_le_bytes()));
    m("directory at the end", &|c| c[rec + 48..rec + 56].copy_from_slice(&(n as u64).to_le_bytes()));
    m("directory at 2^63", &|c| c[rec + 48..rec + 56].copy_from_slice(&(1u64 << 63).to_le_bytes()));
    m("directory at u64::MAX", &|c| c[rec + 48..rec + 56].copy_from_slice(&u64::MAX.to_le_bytes()));
    m("directory at 0", &|c| c[rec + 48..rec + 56].fill(0));
    // (the first entry's extra field: sizes huge, offset huge, a length past the field)
    let cd = (0..n - 3).find(|&p| v[p..p + 4] == *b"PK\x01\x02").unwrap();
    let nl = u16_at(&v, cd + 28);
    let x = cd + 46 + nl + 6;
    assert_eq!(&v[x..x + 2], &[1, 0]);
    m("size huge", &|c| c[x + 4..x + 12].copy_from_slice(&u64::MAX.to_le_bytes()));
    m("size 2^63", &|c| c[x + 4..x + 12].copy_from_slice(&(1u64 << 63).to_le_bytes()));
    m("sizes differ", &|c| c[x + 12..x + 20].copy_from_slice(&7u64.to_le_bytes()));
    m("offset huge", &|c| c[x + 20..x + 28].copy_from_slice(&u64::MAX.to_le_bytes()));
    m("offset 2^63", &|c| c[x + 20..x + 28].copy_from_slice(&(1u64 << 63).to_le_bytes()));
    m("offset in the directory", &|c| c[x + 20..x + 28].copy_from_slice(&(rec as u64).to_le_bytes()));
    m("field length 0", &|c| c[x + 2..x + 4].fill(0));
    m("field length short", &|c| c[x + 2] = 8);
    m("field length past", &|c| c[x + 2..x + 4].copy_from_slice(&0xfff0u16.to_le_bytes()));
    m("unknown field past", &|c| c[x - 6..x - 4].copy_from_slice(&0xffffu16.to_le_bytes()));
    m("extra length past", &|c| c[cd + 30..cd + 32].copy_from_slice(&0xffffu16.to_le_bytes()));
    m("name length past", &|c| c[cd + 28..cd + 30].copy_from_slice(&0xffffu16.to_le_bytes()));
    for (name, c) in cases {
        aqr_testkit::within(Duration::from_secs(20), || {
            aqr_testkit::case(name);
            let _ = aqr_tensors::tensors::parse(&c);
            holds(&c);
        })
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    // (a ZIP64 end record without its locator)
    let mut c = v.clone();
    c.drain(loc..loc + 20);
    assert!(aqr_tensors::tensors::parse(&c).is_none());
}

/// Damage of each container's files, every way: not taken, or rebuilt as it is.
#[test]
fn damaged_containers() {
    let pth = aqr_tensors::synth::pth(4, 1);
    let files = [
        ("gguf", aqr_tensors::synth::file(4, 1)),
        ("pth", pth.clone()),
        ("pth64", zip64(&pth, Z64 { sizes: true, offset: true, end: true })),
        ("safetensors", aqr_tensors::synth::safetensors(4, 1)),
    ];
    for (name, d) in files {
        eprintln!("{name}: {} bytes", d.len());
        aqr_testkit::damaged(&d, 40, 400, Duration::from_secs(120), holds);
    }
}
