//! flate-set as a part module (`module::decode_part`, through aqr-api's emulation) gives what its
//! whole decode does.

use aqr_flate::formula::{Derived, decode};
use aqr_flate::module::{SET_RECORDS_WINDOW, SET_WINDOW, decode_part, index};

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

#[test]
fn part_as_whole() {
    let text: Vec<u8> = (0..400_000)
        .flat_map(|i| format!("{} {}\n", i % 977, i % 13).into_bytes())
        .collect();
    // streams small and large (2 MiB of data or more), alone and together
    let datas: Vec<Vec<u8>> = vec![
        text[..1000].to_vec(),
        text.clone(),
        text[..50_000].to_vec(),
        Vec::new(),
    ];
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
        let idx = index(
            &set.iter()
                .map(|&i| (parts[i].0.len(), parts[i].1.len(), parts[i].2))
                .collect::<Vec<_>>(),
        );
        for derived in [
            Derived::None,
            Derived::Zip,
            Derived::Gzip,
            Derived::ZipBefore,
        ] {
            let len = set
                .iter()
                .map(|&i| (parts[i].2 + derived.bytes()) as u64)
                .sum();
            let inputs = [&data[..], &records[..], &idx[..]];
            let whole = decode(&derived.params(), &res, &inputs, len, 2).unwrap();
            let part = aqr_api::emu::apply(&entry(), &inputs, &res, &derived.params(), &[len]);
            let part = part.map_err(|f| f.what).unwrap().pop().unwrap();
            assert!(part == whole, "{set:?} {derived:?}");
        }
    }
}
