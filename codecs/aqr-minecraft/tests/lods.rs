//! Distant Horizons' databases through `dh-lods` and `zstd-frames`: the parents made of their
//! children (of those that come before them), the file made again natively, whole and streamed,
//! on one thread and on several.

#[path = "common/dh.rs"]
mod dh_db;

use aqr_formula::Node;

fn slice<'a>(f: &'a aqr_formula::Formula, n: &Node) -> &'a [u8] {
    match *n {
        Node::Slice { stream, start, len } => &f.streams[stream].data[start as usize..][..len as usize],
        _ => panic!("an input not a slice"),
    }
}

#[test]
fn parents_made_of_their_children() {
    for n in [2, 4] {
        let db = dh_db::database(n);
        let blobs = aqr_sqlite::blobs::blobs(&db, 8, &|h| h.starts_with(&aqr_zstd::MAGIC)).unwrap();
        let f = aqr_minecraft::lods_formula::encode(&db, &blobs, 2).expect("taken");
        let Node::Apply(z) = &f.root else { panic!("zstd-frames") };
        let Node::Apply(l) = &z.inputs[3] else { panic!("dh-lods") };
        assert_eq!(l.module, aqr_minecraft::lods_formula::MODULE);
        let li: Vec<&[u8]> = l.inputs.iter().map(|i| slice(&f, i)).collect();
        // (the parents' contents not stored: less than the contents)
        assert!((li[1].len() as u64) < l.len, "{n}: stored {} of {}", li[1].len(), l.len);
        for threads in [1, 3] {
            let contents = aqr_minecraft::lods::decode(&l.params, &li, l.len, threads).unwrap();
            let mut zi: Vec<&[u8]> = z.inputs[..3].iter().map(|i| slice(&f, i)).collect();
            zi.push(&contents);
            let out = aqr_zstd::frames::decode(&z.params, &zi, z.len, 2).unwrap();
            assert!(out == db, "{n} {threads}");
        }
        // corrupt steps fail, never panic
        let mut steps = li[0].to_vec();
        for i in (0..steps.len()).step_by(steps.len() / 50 + 1) {
            steps[i] ^= 0x41;
            let bad = [&steps[..], li[1], li[2]];
            let _ = aqr_minecraft::lods::decode(&l.params, &bad, l.len, 2);
            steps[i] ^= 0x41;
        }
    }
}

/// The formula's pool held to less than the children and parents need at once: children are let go
/// (their quadrants stored instead) and parents made no more, as the budget shrinks; whatever is
/// left still makes the file again, and less is derived the less the pool may hold.
#[test]
fn pool_held_down() {
    use aqr_minecraft::lods_formula::{HELD, MODULE, POOL};
    let db = dh_db::database(8);
    let blobs = aqr_sqlite::blobs::blobs(&db, 8, &|h| h.starts_with(&aqr_zstd::MAGIC)).unwrap();
    let mut stored = Vec::new();
    for held in [usize::MAX >> 1, 100_000, 70_000, 1000] {
        let _g = HELD.set_here(held);
        let Some(f) = aqr_minecraft::lods_formula::encode(&db, &blobs, 1) else {
            // (nothing left to derive: no parent keeps a child)
            assert!(held <= 60_000, "{held}");
            stored.push(None);
            continue;
        };
        let Node::Apply(z) = &f.root else { panic!("zstd-frames") };
        let Node::Apply(l) = &z.inputs[3] else { panic!("dh-lods") };
        assert_eq!(l.module, MODULE);
        let li: Vec<&[u8]> = l.inputs.iter().map(|i| slice(&f, i)).collect();
        for threads in [1, 3] {
            let contents = aqr_minecraft::lods::decode(&l.params, &li, l.len, threads).unwrap();
            let mut zi: Vec<&[u8]> = z.inputs[..3].iter().map(|i| slice(&f, i)).collect();
            zi.push(&contents);
            assert!(aqr_zstd::frames::decode(&z.params, &zi, z.len, 2).unwrap() == db, "{held}");
        }
        stored.push(Some(li[1].len()));
    }
    let made: Vec<usize> = stored.iter().flatten().copied().collect();
    assert!(made.len() >= 2, "{stored:?}");
    // (a smaller pool stores no less)
    assert!(made.windows(2).all(|w| w[0] <= w[1]), "{stored:?}");
    assert!(made.last() > made.first(), "{stored:?}");
    // a pool that what is held cannot fit: the budget halved to the least
    let _g = (HELD.set_here(usize::MAX >> 1), POOL.set_here(1000));
    if let Some(f) = aqr_minecraft::lods_formula::encode(&db, &blobs, 1) {
        let Node::Apply(z) = &f.root else { panic!("zstd-frames") };
        assert!(z.len as usize == db.len());
    }
}
