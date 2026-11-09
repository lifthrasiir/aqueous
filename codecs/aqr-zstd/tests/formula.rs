use aqr_formula::{Apply, Formula, Kind, Node};
use aqr_zstd::formula::take;

fn eval(f: &Formula) -> Vec<u8> {
    f.eval(&|_: &Apply, _: &[&[u8]]| -> aqr_formula::Result<Vec<u8>> { unreachable!("no module") }).unwrap()
}

fn data(n: u32, seed: u32) -> Vec<u8> {
    (0..n).map(|i| ((i / 3).wrapping_mul(seed) >> 5 & 0x3f) as u8 + b' ').collect()
}

/// Three frames (one of them past a skippable frame, in blocks of several sizes) after other
/// bytes, and ranges of their contents taken: within a frame, across two, the start and the
/// end, a frame none takes left out. Through an archive too, the ranges' nodes in another
/// order.
#[test]
#[allow(clippy::single_range_in_vec_init)]
fn ranges_taken() {
    let contents = [data(300_000, 7), data(1000, 11), data(500_000, 13)];
    let mut file = b"before".to_vec();
    let mut frames = Vec::new();
    for (k, c) in contents.iter().enumerate() {
        if k == 2 {
            file.extend_from_slice(&[0x50, 0x2a, 0x4d, 0x18, 2, 0, 0, 0, 9, 9]);
        }
        let z = zstd::bulk::compress(c, 1 + k as i32).unwrap();
        let fr = aqr_zstd::parse(&z).unwrap();
        let blocks = &fr.frames().next().unwrap().blocks;
        frames.push(((file.len() + blocks.start) as u64..(file.len() + blocks.end) as u64, c.len() as u64));
        file.extend_from_slice(&z);
    }
    let all = contents.concat();
    let n = all.len() as u64;
    let cases: [&[std::ops::Range<u64>]; 5] = [
        &[10..20_000, 200_000..300_500],
        &[0..n],
        &[0..1, n - 1..n],
        &[300_000..301_000],
        &[5..300_000, 300_000..300_001, 301_000..n - 3],
    ];
    for wanted in cases {
        let mut f = Formula::default();
        let stream = f.streams.len();
        let whole = f.push_as("zst", Kind::Compressed, file.clone());
        let nodes = take(&mut f, stream, &frames, wanted);
        assert_eq!(nodes.len(), wanted.len());
        let want: Vec<u8> = wanted.iter().flat_map(|r| all[r.start as usize..r.end as usize].to_vec()).collect();
        f.root = Node::Concat(nodes.iter().cloned().chain([whole.clone()]).collect());
        assert!(eval(&f) == [&want[..], &file].concat(), "{wanted:?}");
        // (a frame none of the ranges is in not decompressed)
        let held = |r: &std::ops::Range<u64>| r.start < 300_000 || r.end > 301_000;
        let patched = f.shared.iter().filter(|s| s.apply().is_none()).count();
        assert!(patched <= 3 && (wanted.iter().any(held) || patched == 1), "{wanted:?}");
        let mut g = f.clone();
        g.root = Node::Concat([whole].into_iter().chain(nodes.into_iter().rev()).collect());
        let mut want = file.clone();
        wanted.iter().rev().for_each(|r| want.extend_from_slice(&all[r.start as usize..r.end as usize]));
        let c = aqr_formula::aqr::encode(&g, None, aqr_formula::aqr::Level::Zstd(3)).unwrap();
        let natives = aqr_formula::aqr::natives(&[], &|_: &Apply, _: &[&[u8]]| -> aqr_formula::Result<Vec<u8>> {
            unreachable!("no module")
        });
        assert!(aqr_formula::aqr::decode(&c, &natives).unwrap() == want, "{wanted:?}");
    }
}
