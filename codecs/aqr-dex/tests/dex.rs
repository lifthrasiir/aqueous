//! DEX files written here (no Android tools needed) through the formula and the module: strings
//! of every UTF-16 length, prototypes, fields, methods, classes with their data and code (try
//! blocks among it); code and classes' data laid out in other orders than the classes', LEB128s
//! not in the shortest form, wrong checksums and sections the module does not know, kept as
//! they are where they cannot be taken; streams corrupted, which never panic.

use aqr_common::rng::Rng;
use aqr_dex::dex::{self, mode, s};
use aqr_dex::{encode, formula};
use aqr_formula::{Apply, Formula};
use sha1::{Digest, Sha1};

fn eval(f: &Formula) -> aqr_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, formula::MODULE);
        formula::decode(&a.params, inputs, a.len).map_err(|e| aqr_formula::Error::new(e.0))
    })
}

/// How a file is written apart from its contents.
#[derive(Default, Clone, Copy)]
struct Shape {
    /// Code items of neighbouring classes swapped.
    code_order: bool,
    /// The data of two classes swapped in its section.
    data_order: bool,
    /// A LEB128 of a class's data in two bytes where one would do.
    long_leb: bool,
    /// The checksum and the signature wrong.
    bad_sums: bool,
    /// A section of a type the format does not have, and bytes not zero between sections.
    unknown: bool,
}

fn uleb(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn sleb(out: &mut Vec<u8>, mut v: i32) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0) {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn pad4(d: &mut Vec<u8>, fill: u8) {
    while !d.len().is_multiple_of(4) {
        d.push(fill);
    }
}

fn shorty(desc: &str) -> char {
    match desc.as_bytes()[0] {
        b'[' => 'L',
        c => c as char,
    }
}

/// An instruction array: instructions of any opcode with any fields (a `nop` with a zero high
/// byte), and now and then a payload of each kind at the end.
fn instructions(rng: &mut Rng) -> Vec<u16> {
    let mut u = Vec::new();
    for _ in 0..rng.below(12) {
        let op = 1 + rng.below(255) as u16;
        let (n, _, _) = aqr_dex::code::format(op as u8);
        u.push(op | (rng.u64() as u16) << 8);
        for _ in 1..n {
            // (small values mostly, as indices and registers are)
            u.push(if rng.below(2) == 0 { rng.below(8) as u16 } else { rng.u64() as u16 });
        }
    }
    if rng.below(2) == 0 {
        u.push(0);
    }
    match rng.below(4) {
        0 => {
            let n = rng.below(4) as u16;
            u.extend([0x0100, n, 7, 0]);
            u.extend((0..2 * n).map(|_| rng.u64() as u16));
        }
        1 => {
            let n = rng.below(4) as u16;
            u.extend([0x0200, n]);
            u.extend((0..4 * n).map(|_| rng.u64() as u16));
        }
        2 => {
            let (w, n) = (1 + rng.below(4) as u16, rng.below(5) as u16);
            u.extend([0x0300, w, n, 0]);
            u.extend((0..(w * n).div_ceil(2)).map(|_| rng.u64() as u16));
        }
        _ => {}
    }
    if u.is_empty() {
        u.push(0x000e);
    }
    u
}

/// A DEX file of classes made from `seed`.
fn write(seed: u64, shape: Shape) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    let classes = 3 + rng.below(12) as usize;
    let mut descs: Vec<String> =
        ["V", "I", "J", "Z", "Ljava/lang/Object;", "[I"].iter().map(|s| s.to_string()).collect();
    for k in 0..classes {
        descs.push(format!("La/C{k};"));
    }
    descs.push("[La/C0;".into());
    // Prototypes: a return type and up to three parameters, as descriptors.
    let mut protos: Vec<(usize, Vec<usize>)> = (0..4 + rng.below(10))
        .map(|_| {
            let ret = rng.below(descs.len() as u64) as usize;
            let params = (0..rng.below(4)).map(|_| 1 + rng.below(descs.len() as u64 - 1) as usize).collect();
            (ret, params)
        })
        .collect();
    let names = ["<init>", "run", "x", "\u{e9}t\u{e9}", "\u{d55c}\u{ae00}", "\u{1f600}", "value", "A.java"];
    let mut strings: Vec<Vec<u8>> = descs.iter().map(|d| d.as_bytes().to_vec()).collect();
    for (r, p) in &protos {
        strings
            .push(std::iter::once(&descs[*r]).chain(p.iter().map(|&t| &descs[t])).map(|d| shorty(d) as u8).collect());
    }
    let mut name_bytes = Vec::new();
    for n in names {
        // (MUTF-8: characters past U+FFFF as their surrogates, three bytes each)
        let mut b = Vec::new();
        for c in n.encode_utf16() {
            match c {
                0..0x80 => b.push(c as u8),
                0x80..0x800 => b.extend([0xc0 | (c >> 6) as u8, 0x80 | (c & 0x3f) as u8]),
                _ => b.extend([0xe0 | (c >> 12) as u8, 0x80 | (c >> 6 & 0x3f) as u8, 0x80 | (c & 0x3f) as u8]),
            }
        }
        name_bytes.push(b.clone());
        strings.push(b);
    }
    strings.sort();
    strings.dedup();
    let sidx = |s: &[u8]| strings.iter().position(|t| t == s).unwrap() as u32;
    let mut types: Vec<u32> = descs.iter().map(|d| sidx(d.as_bytes())).collect();
    types.sort();
    let tidx = |d: &str| types.iter().position(|&t| t == sidx(d.as_bytes())).unwrap() as u16;
    let mut proto_rows: Vec<(u32, u32, Vec<u16>)> = protos
        .drain(..)
        .map(|(r, p)| {
            let sh: Vec<u8> =
                std::iter::once(&descs[r]).chain(p.iter().map(|&t| &descs[t])).map(|d| shorty(d) as u8).collect();
            (sidx(&sh), tidx(&descs[r]) as u32, p.iter().map(|&t| tidx(&descs[t])).collect())
        })
        .collect();
    proto_rows.sort_by(|a, b| (a.1, &a.2).cmp(&(b.1, &b.2)));
    proto_rows.dedup_by(|a, b| (a.1, &a.2) == (b.1, &b.2));
    let class_t: Vec<u16> = (0..classes).map(|k| tidx(&format!("La/C{k};"))).collect();
    let name_ids: Vec<u32> = name_bytes.iter().map(|b| sidx(b)).collect();
    let mut fields = Vec::new();
    let mut methods = Vec::new();
    for &c in &class_t {
        for _ in 0..rng.below(4) {
            fields.push((
                c,
                1 + rng.below(types.len() as u64 - 1) as u16,
                name_ids[rng.below(name_ids.len() as u64) as usize],
            ));
        }
        for _ in 0..rng.below(6) {
            methods.push((
                c,
                rng.below(proto_rows.len() as u64) as u16,
                name_ids[rng.below(name_ids.len() as u64) as usize],
            ));
        }
    }
    for t in [&mut fields, &mut methods] {
        t.sort_by_key(|&(c, x, n)| (c, n, x));
        t.dedup();
    }

    // Type lists: the prototypes' parameters and the classes' interfaces.
    let ifaces: Vec<Vec<u16>> = (0..classes)
        .map(|_| (0..rng.below(3)).map(|_| class_t[rng.below(classes as u64) as usize]).collect())
        .collect();
    let mut lists: Vec<Vec<u16>> =
        proto_rows.iter().map(|p| p.2.clone()).chain(ifaces.iter().cloned()).filter(|l| !l.is_empty()).collect();
    lists.sort();
    lists.dedup();

    // The data section: code items, type lists, string data, classes' data; then the map.
    let header_len = 0x70;
    let ids_len = 4 * strings.len()
        + 4 * types.len()
        + 12 * proto_rows.len()
        + 8 * fields.len()
        + 8 * methods.len()
        + 32 * classes;
    let mut d = vec![0u8; header_len + ids_len];
    let data_off = d.len();
    // Each class's methods, direct then virtual, with code or none.
    let mut class_methods: Vec<Vec<(u32, u32, Option<usize>)>> = vec![Vec::new(); classes];
    let mut code_items = Vec::new();
    for (k, &c) in class_t.iter().enumerate() {
        for (i, m) in methods.iter().enumerate().filter(|(_, m)| m.0 == c) {
            let _ = m;
            let has_code = rng.below(5) != 0;
            class_methods[k].push((
                i as u32,
                if rng.below(2) == 0 { 1 } else { 0x10001 },
                has_code.then(|| {
                    code_items.push(k);
                    code_items.len() - 1
                }),
            ));
        }
    }
    let mut order: Vec<usize> = (0..code_items.len()).collect();
    if shape.code_order {
        for i in (1..order.len()).step_by(3) {
            order.swap(i - 1, i);
        }
    }
    let mut code_off = vec![0u32; code_items.len()];
    let code_start = d.len();
    for &j in &order {
        pad4(&mut d, 0);
        code_off[j] = d.len() as u32;
        let units = instructions(&mut rng);
        let insns = units.len() as u32;
        let tries = (rng.below(3) == 0) as u16;
        for v in [1 + rng.below(8) as u16, rng.below(3) as u16, rng.below(3) as u16, tries] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.extend_from_slice(&0u32.to_le_bytes());
        d.extend_from_slice(&insns.to_le_bytes());
        for u in &units {
            d.extend_from_slice(&u.to_le_bytes());
        }
        if tries > 0 {
            if insns % 2 == 1 {
                d.extend_from_slice(&[0, 0]);
            }
            d.extend_from_slice(&0u32.to_le_bytes());
            d.extend_from_slice(&(insns as u16).to_le_bytes());
            d.extend_from_slice(&1u16.to_le_bytes());
            uleb(&mut d, 1);
            let size = rng.below(3) as i32 - 1;
            sleb(&mut d, size);
            for _ in 0..size.unsigned_abs() {
                uleb(&mut d, class_t[0] as u32);
                uleb(&mut d, rng.below(insns as u64) as u32);
            }
            if size <= 0 {
                uleb(&mut d, 0);
            }
        }
    }
    let code_end = d.len();
    if shape.unknown {
        d.extend_from_slice(&[0xee; 3]);
    }
    pad4(&mut d, 0);
    let lists_start = d.len();
    let mut list_off = Vec::new();
    for l in &lists {
        pad4(&mut d, 0);
        list_off.push(d.len() as u32);
        d.extend_from_slice(&(l.len() as u32).to_le_bytes());
        for t in l {
            d.extend_from_slice(&t.to_le_bytes());
        }
    }
    let list_of = |l: &[u16]| if l.is_empty() { 0 } else { list_off[lists.iter().position(|x| x == l).unwrap()] };
    let strings_start = d.len();
    let mut string_off = Vec::new();
    for s in &strings {
        string_off.push(d.len() as u32);
        uleb(&mut d, s.iter().filter(|&&b| b & 0xc0 != 0x80).count() as u32);
        d.extend_from_slice(s);
        d.push(0);
    }
    let unknown_start = d.len();
    if shape.unknown {
        d.extend_from_slice(b"\x01\x02unknown section");
    }
    let data_start = d.len();
    let mut data_order: Vec<usize> = (0..classes).filter(|&k| !class_methods[k].is_empty() || k % 2 == 0).collect();
    let with_data = data_order.clone();
    if shape.data_order && data_order.len() >= 3 {
        data_order.swap(0, 2);
    }
    let mut class_data_off = vec![0u32; classes];
    for (n, &k) in data_order.iter().enumerate() {
        class_data_off[k] = d.len() as u32;
        let cf: Vec<usize> = (0..fields.len()).filter(|&i| fields[i].0 == class_t[k]).collect();
        let (st, inst) = cf.split_at(cf.len() / 2);
        let ms = &class_methods[k];
        let (direct, virt) = ms.split_at(ms.len() / 2);
        if shape.long_leb && n == 0 {
            d.extend_from_slice(&[st.len() as u8 | 0x80, 0]);
        } else {
            uleb(&mut d, st.len() as u32);
        }
        for v in [inst.len(), direct.len(), virt.len()] {
            uleb(&mut d, v as u32);
        }
        for list in [st, inst] {
            let mut prev = 0;
            for &i in list {
                uleb(&mut d, i as u32 - prev);
                uleb(&mut d, 8);
                prev = i as u32;
            }
        }
        for list in [direct, virt] {
            let mut prev = 0;
            for &(i, flags, code) in list {
                uleb(&mut d, i - prev);
                uleb(&mut d, flags);
                uleb(&mut d, code.map_or(0, |j| code_off[j]));
                prev = i;
            }
        }
    }
    let data_end = d.len();
    pad4(&mut d, 0);
    let map_off = d.len();

    // The ids.
    let mut p = header_len;
    let mut put = |d: &mut Vec<u8>, b: &[u8]| {
        d[p..p + b.len()].copy_from_slice(b);
        p += b.len();
    };
    let string_ids = header_len;
    for o in &string_off {
        put(&mut d, &o.to_le_bytes());
    }
    let type_ids = string_ids + 4 * strings.len();
    for t in &types {
        put(&mut d, &t.to_le_bytes());
    }
    let proto_ids = type_ids + 4 * types.len();
    for (sh, r, params) in &proto_rows {
        for v in [*sh, *r, list_of(params)] {
            put(&mut d, &v.to_le_bytes());
        }
    }
    let field_ids = proto_ids + 12 * proto_rows.len();
    let method_ids = field_ids + 8 * fields.len();
    for &(c, x, n) in fields.iter().chain(&methods) {
        put(&mut d, &c.to_le_bytes());
        put(&mut d, &x.to_le_bytes());
        put(&mut d, &n.to_le_bytes());
    }
    let class_defs = method_ids + 8 * methods.len();
    let source = sidx(b"A.java");
    for k in 0..classes {
        let sup = if k == 0 { u32::MAX } else { tidx("Ljava/lang/Object;") as u32 };
        let src = if k % 3 == 2 { u32::MAX } else { source };
        for v in [class_t[k] as u32, 1, sup, list_of(&ifaces[k]), src, 0, class_data_off[k], 0] {
            put(&mut d, &v.to_le_bytes());
        }
    }
    assert!(with_data.iter().all(|&k| class_data_off[k] != 0));

    // The map.
    let mut map: Vec<(u16, u32, u32)> = vec![
        (0x0000, 1, 0),
        (0x0001, strings.len() as u32, string_ids as u32),
        (0x0002, types.len() as u32, type_ids as u32),
        (0x0003, proto_rows.len() as u32, proto_ids as u32),
        (0x0004, fields.len() as u32, field_ids as u32),
        (0x0005, methods.len() as u32, method_ids as u32),
        (0x0006, classes as u32, class_defs as u32),
        (0x2001, code_items.len() as u32, code_start as u32),
        (0x1001, lists.len() as u32, lists_start as u32),
        (0x2002, strings.len() as u32, strings_start as u32),
        (0x2000, data_order.len() as u32, data_start as u32),
        (0x1000, 1, map_off as u32),
    ];
    if shape.unknown {
        map.push((0x7777, 1, unknown_start as u32));
    }
    map.retain(|e| e.1 > 0);
    map.sort_by_key(|e| e.2);
    let _ = (code_end, data_end);
    d.extend_from_slice(&(map.len() as u32).to_le_bytes());
    for (t, n, o) in &map {
        d.extend_from_slice(&t.to_le_bytes());
        d.extend_from_slice(&[0, 0]);
        d.extend_from_slice(&n.to_le_bytes());
        d.extend_from_slice(&o.to_le_bytes());
    }

    // The header.
    let len = d.len() as u32;
    d[..8].copy_from_slice(b"dex\n035\0");
    let fields_h = [
        len,
        0x70,
        0x12345678,
        0,
        0,
        map_off as u32,
        strings.len() as u32,
        string_ids as u32,
        types.len() as u32,
        type_ids as u32,
        proto_rows.len() as u32,
        proto_ids as u32,
        fields.len() as u32,
        if fields.is_empty() { 0 } else { field_ids as u32 },
        methods.len() as u32,
        if methods.is_empty() { 0 } else { method_ids as u32 },
        classes as u32,
        class_defs as u32,
        len - data_off as u32,
        data_off as u32,
    ];
    for (i, v) in fields_h.iter().enumerate() {
        d[0x20 + 4 * i..0x24 + 4 * i].copy_from_slice(&v.to_le_bytes());
    }
    let sig = Sha1::digest(&d[32..]);
    d[12..32].copy_from_slice(&sig);
    let sum = aqr_common::adler32::hash(&d[12..]);
    d[8..12].copy_from_slice(&sum.to_le_bytes());
    if shape.bad_sums {
        d[8] ^= 1;
        d[20] ^= 1;
    }
    d
}

/// The modes of the map's items, by type, from the head stream.
fn modes(st: &[Vec<u8>]) -> Vec<(u16, u8)> {
    let head = streams(st)[s::HEAD];
    let count = dex::le32(head, dex::HEADER + 1).unwrap() as usize;
    let items = dex::items(&head[dex::HEADER + 5..][..12 * count]);
    items.iter().map(|it| it.ty).zip(head[dex::HEADER + 5 + 12 * count..].iter().copied()).collect()
}

fn check(d: &[u8]) -> Vec<Vec<u8>> {
    let f = formula::encode(d, 1).expect("taken");
    assert_eq!(eval(&f).unwrap(), d);
    encode::split(d).unwrap()
}

/// The streams of the module's inputs `st`.
fn streams(st: &[Vec<u8>]) -> Vec<&[u8]> {
    dex::unpack(&st.iter().map(|v| v.as_slice()).collect::<Vec<_>>()).unwrap()
}

/// Files of every shape come back; what can be taken is.
#[test]
fn files() {
    for seed in 0..200 {
        let d = write(seed, Shape::default());
        let st = check(&d);
        for (t, m) in modes(&st) {
            assert_eq!(m == mode::TAKEN, dex::takes(t), "seed {seed}: {t:#x} {m}");
        }
        assert_eq!(streams(&st)[s::HEAD][dex::HEADER], 3, "checksum and signature derived");
        assert!(streams(&st)[s::REST].iter().all(|&b| b == 0), "only padding in the rest");
        for (k, shape) in [
            Shape { code_order: true, ..Shape::default() },
            Shape { data_order: true, ..Shape::default() },
            Shape { code_order: true, data_order: true, unknown: true, ..Shape::default() },
        ]
        .into_iter()
        .enumerate()
        {
            let st = check(&write(seed, shape));
            for (t, m) in modes(&st) {
                assert_eq!(m == mode::TAKEN, dex::takes(t), "seed {seed}, shape {k}: {t:#x} {m}");
            }
        }
    }
}

/// What the module would not rebuild as it is is kept: a class's data with a LEB128 longer than
/// it needs, and checksums that are wrong.
#[test]
fn kept() {
    for seed in 0..50 {
        let st = check(&write(seed, Shape { long_leb: true, ..Shape::default() }));
        let m = modes(&st);
        assert!(m.contains(&(0x2000, mode::RAW)) && m.contains(&(0x0006, mode::TAKEN)), "seed {seed}");
        let st = check(&write(seed, Shape { bad_sums: true, ..Shape::default() }));
        assert_eq!(streams(&st)[s::HEAD][dex::HEADER], 0);
    }
}

/// Not DEX files, or ones the module does not take.
#[test]
fn not_taken() {
    let d = write(1, Shape::default());
    assert!(encode::split(&d[..d.len() - 1]).is_none(), "cut short");
    let mut v41 = d.clone();
    v41[4..7].copy_from_slice(b"041");
    assert!(encode::split(&v41).is_none(), "a container");
    assert!(encode::split(b"dex\n035\0").is_none());
    assert!(encode::split(&[0; 200]).is_none());
}

/// Corrupted streams make the module fail or give a file of the length, never panic.
#[test]
fn corrupt() {
    let d = write(3, Shape { code_order: true, data_order: true, ..Shape::default() });
    let st = encode::split(&d).unwrap();
    let mut rng = Rng::new(7);
    for _ in 0..20000 {
        let mut bad = st.clone();
        let k = rng.below(st.len() as u64) as usize;
        if bad[k].is_empty() {
            continue;
        }
        let at = rng.below(bad[k].len() as u64) as usize;
        match rng.below(3) {
            0 => bad[k].truncate(at),
            1 => bad[k][at] ^= 1 << rng.below(8),
            _ => bad[k][at] = rng.u64() as u8,
        }
        let refs: Vec<&[u8]> = bad.iter().map(|v| v.as_slice()).collect();
        if let Ok(m) = formula::decode(&[], &refs, d.len() as u64) {
            assert_eq!(m.len(), d.len());
        }
    }
}
