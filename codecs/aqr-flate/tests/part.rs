//! flate-set as a part module (`module::decode_part`, through aqr-api's emulation) gives what its
//! whole decode does.

use aqr_flate::formula::{Derived, decode};
use aqr_flate::module::{SET_RECORDS_WINDOW, SET_WINDOW, decode_part, index, index_before};

fn entry() -> aqr_api::emu::Entry {
    aqr_api::emu::Entry {
        kind: aqr_api::emu::Kind::Part,
        windows: &[
            aqr_api::Window::Input(0, SET_WINDOW),
            aqr_api::Window::Input(1, SET_RECORDS_WINDOW),
            aqr_api::Window::Input(2, SET_RECORDS_WINDOW),
            aqr_api::Window::Output(0, SET_WINDOW),
        ],
        arity: 7,
        call: |a| decode_part(a[0], a[1], a[2], a[3], a[4], a[5], a[6]),
    }
}

/// The CRC-32 of `d`.
fn crc_of(d: &[u8]) -> u32 {
    let mut t = Vec::new();
    Derived::Zip.put(d, 0, &mut t);
    u32::from_le_bytes(t[..4].try_into().unwrap())
}

#[test]
fn part_as_whole() {
    let text: Vec<u8> = (0..400_000).flat_map(|i| format!("{} {}\n", i % 977, i % 13).into_bytes()).collect();
    // streams small and large (2 MiB of data or more), alone and together
    let datas: Vec<Vec<u8>> = vec![text[..1000].to_vec(), text.clone(), text[..50_000].to_vec(), Vec::new()];
    let z = "zlib:level=6".parse::<flate_zoo::Encoder>().unwrap();
    let parts: Vec<(Vec<u8>, Vec<u8>, usize)> = datas
        .iter()
        .map(|d| {
            let raw = z.compress(d).unwrap();
            let (data, record) = aqr_flate::deconstruct(&raw).unwrap();
            (data, record, raw.len())
        })
        .collect();
    let profile = aqr_flate::DEFAULT_PROFILE;
    let res = aqr_flate::formula::resources(profile).unwrap();
    let res: Vec<&[u8]> = res.iter().map(|r| &r[..]).collect();
    for set in [&[0][..], &[1], &[0, 1, 2, 3], &[3, 1, 0]] {
        let data: Vec<u8> = set.iter().flat_map(|&i| parts[i].0.clone()).collect();
        let records: Vec<u8> = set.iter().flat_map(|&i| parts[i].1.clone()).collect();
        let lens: Vec<(usize, usize, usize)> =
            set.iter().map(|&i| (parts[i].0.len(), parts[i].1.len(), parts[i].2)).collect();
        let idx = index(&lens);
        // (with the CRC-32s of the large streams' data, for ZipBefore)
        let lens_crc: Vec<_> = set.iter().zip(&lens).map(|(&i, l)| (l.0, l.1, l.2, crc_of(&parts[i].0))).collect();
        let idx_before = index_before(&lens_crc);
        for derived in [Derived::None, Derived::Zip, Derived::Gzip, Derived::Zlib, Derived::ZipBefore] {
            let len = set.iter().map(|&i| (parts[i].2 + derived.bytes()) as u64).sum();
            let idx = if derived.before() { &idx_before } else { &idx };
            let inputs = [&data[..], &records[..], &idx[..]];
            let whole = decode(&derived.params(), &res, &inputs, len, 2).unwrap();
            let part = aqr_api::emu::apply(&entry(), &inputs, &res, &derived.params(), &[len]);
            let part = part.map_err(|f| f.what).unwrap().pop().unwrap();
            assert!(part == whole, "{set:?} {derived:?}");
        }
    }
}

/// ZipBefore with a large stream's CRC-32 given: the same output whole and as a part, and a
/// wrong CRC-32, or none, fails.
#[test]
fn before_with_given_crc() {
    let text: Vec<u8> = (0..400_000).flat_map(|i| format!("{} {}\n", i % 977, i % 13).into_bytes()).collect();
    assert!(text.len() > 2 << 20);
    let raw = "zlib:level=6".parse::<flate_zoo::Encoder>().unwrap().compress(&text).unwrap();
    let (data, record) = aqr_flate::deconstruct(&raw).unwrap();
    let res = aqr_flate::formula::resources(aqr_flate::DEFAULT_PROFILE).unwrap();
    let res: Vec<&[u8]> = res.iter().map(|r| &r[..]).collect();
    let derived = Derived::ZipBefore;
    let len = (raw.len() + derived.bytes()) as u64;
    let crc = crc_of(&data);
    let try_index = |idx: Vec<u8>| {
        let inputs = [&data[..], &record[..], &idx[..]];
        let whole = decode(&derived.params(), &res, &inputs, len, 2);
        let part = aqr_api::emu::apply(&entry(), &inputs, &res, &derived.params(), &[len]);
        (whole.ok(), part.ok().map(|mut p| p.pop().unwrap()))
    };
    let good = index_before(&[(data.len(), record.len(), raw.len(), crc)]);
    assert!(!good.is_empty());
    let (whole, part) = try_index(good);
    let mut want = Vec::new();
    derived.put(&data, raw.len(), &mut want);
    want.extend_from_slice(&raw);
    assert!(whole.as_ref() == Some(&want) && part == whole);
    let bad = index_before(&[(data.len(), record.len(), raw.len(), crc ^ 1)]);
    assert_eq!(try_index(bad), (None, None));
    // (a single large stream with no index: no CRC-32)
    assert_eq!(try_index(Vec::new()), (None, None));
}

/// Damaged inputs of flate-set (the data, the records, the index) fail or succeed alike whole
/// and as a part, with the same output where they succeed: never a panic.
#[test]
fn damaged_set_inputs_are_alike() {
    let text: Vec<u8> = (0..3000).flat_map(|i| format!("{} {}\n", i % 977, i % 13).into_bytes()).collect();
    let z = "zlib:level=6".parse::<flate_zoo::Encoder>().unwrap();
    let parts: Vec<(Vec<u8>, Vec<u8>, usize)> = [&text[..700], &text[..9000]]
        .iter()
        .map(|d| {
            let raw = z.compress(d).unwrap();
            let (data, record) = aqr_flate::deconstruct(&raw).unwrap();
            (data, record, raw.len())
        })
        .collect();
    let res = aqr_flate::formula::resources(aqr_flate::DEFAULT_PROFILE).unwrap();
    let res: Vec<&[u8]> = res.iter().map(|r| &r[..]).collect();
    let data: Vec<u8> = parts.iter().flat_map(|p| p.0.clone()).collect();
    let records: Vec<u8> = parts.iter().flat_map(|p| p.1.clone()).collect();
    let lens: Vec<_> = parts.iter().map(|p| (p.0.len(), p.1.len(), p.2)).collect();
    let idx = index(&lens);
    let derived = Derived::Zip;
    let len: u64 = parts.iter().map(|p| (p.2 + derived.bytes()) as u64).sum();
    let run = |inputs: [&[u8]; 3], len: u64| {
        let whole = decode(&derived.params(), &res, &inputs, len, 1).ok();
        let part =
            aqr_api::emu::apply(&entry(), &inputs, &res, &derived.params(), &[len]).ok().map(|mut p| p.pop().unwrap());
        assert!(whole == part, "whole {:?} part {:?}", whole.map(|w| w.len()), part.map(|p| p.len()));
    };
    run([&data, &records, &idx], len);
    for (which, input) in [&data, &records, &idx].into_iter().enumerate() {
        aqr_testkit::damaged(input, 24, 12, std::time::Duration::from_secs(30), |v| {
            let mut inputs = [&data[..], &records[..], &idx[..]];
            inputs[which] = v;
            run(inputs, len);
        });
    }
    // (the declared output length wrong either way)
    for l in [0, len - 1, len + 1, len + 20] {
        run([&data, &records, &idx], l);
    }
}
