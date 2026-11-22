use super::*;

/// An archive of a single file, `quine.aqr`, that is the archive itself
/// (`scripts/make-quine.py`): its formula takes the stored bytes of a resource block twice
/// (ResourceBytes), splits one of them and swaps the pieces around the other.
/// Data repeating itself farther than a sample of it shows (a stream of files in several
/// versions, say) may pay: its repeats cost nothing, as they archive in a window of all of
/// it. Data that does not, does not.
#[test]
fn pays_for_repeats_afar() {
    let noise = |seed: u64, n: usize| {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (x >> 56) as u8
            })
            .collect::<Vec<u8>>()
    };
    let probe = aqr_formula::probe::Probe::flat(archivers(Level::MID));
    let enc = Encoder::new(Level::MID);
    let pays = |d: &[u8]| probe.pays(&enc, 5 << 20, || Some(0), d, || 0.0, Kind::Unknown);
    let a = noise(1, 3 << 20);
    assert!(pays(&[&a[..], &a[..]].concat()));
    assert!(!pays(&[&a[..], &noise(2, 3 << 20)[..]].concat()));
    // (a piece repeating within itself, which a sample shows, is no more)
    let runs: Vec<u8> = (0..128).flat_map(|i| noise(i, 2000).repeat(32)).collect();
    let repeated = aqr_formula::probe::repeated(&runs, &enc);
    assert!(repeated < 0.05, "{repeated}");
}

#[test]
fn quine_holds_itself() {
    let c = include_bytes!("../testdata/quine.aqr");
    let Value::Tree(tree, contents) = decode_value(c, &NoModules).unwrap() else {
        panic!("a tree expected");
    };
    assert_eq!(tree.entries.len(), 1);
    assert_eq!(tree.entries[0].name, b"quine.aqr");
    let EntryNode::File(i) = tree.entries[0].node else {
        panic!("a file expected");
    };
    assert_eq!(contents[i], c);
}

#[test]
fn compressed_len_counts() {
    let mut rng = aqr_common::rng::Rng::new(3);
    let random: Vec<u8> = (0..100_000).map(|_| rng.below(256) as u8).collect();
    let text: Vec<u8> = (0..300_000u32).flat_map(|i| format!("{} {} ", i % 977, i / 13).into_bytes()).collect();
    for d in [&b""[..], b"abc", &random, &text] {
        for (level, threads) in [(1, 1), (3, 4), (9, 2)] {
            let c = compressed(&mut CCtx::create(), &[d], level, threads).unwrap();
            let n = compressed_len(&mut CCtx::create(), &[d], level, threads).unwrap();
            assert_eq!(n, c.as_ref().map(Vec::len), "{} bytes, level {level}", d.len());
            assert_eq!(archived_raw_len(d, n), archived_raw(d, c.as_deref()));
        }
    }
}
use aqr_formula::Stream;

thread_local! {
    /// The streams compressed ([`compressed_to`]), counted.
    pub(super) static COMPRESSED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) const REV: Module = Module::new("rev", 1);

pub(super) const CUT: Module = Module::new("cut", 1);

/// `rev` reverses the concatenation of its inputs, with parameters 1, 2, 3; `cut` cuts it
/// into outputs of the sizes declared.
struct RevCut;

impl crate::Modules for RevCut {
    fn apply(&self, a: &aqr_formula::Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        assert_eq!((a.module, &a.params[..], a.resources.len()), (REV, &[1, 2, 3][..], 0));
        Ok(inputs.concat().into_iter().rev().collect())
    }

    fn apply_shared(&self, s: &aqr_formula::Shared, inputs: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
        if s.apply().is_none_or(|a| a.module != CUT) {
            return Err(Error::new("not cut"));
        }
        let (all, mut at) = (inputs.concat(), 0);
        let mut next = |n: u64| {
            let a = at.min(all.len());
            at += n as usize;
            all[a..at.min(all.len())].to_vec()
        };
        Ok(s.sizes.iter().map(|&n| next(n)).collect())
    }
}

pub(super) fn natives() -> Natives<'static, impl crate::Modules + Sync> {
    super::natives(&[REV, CUT], &RevCut)
}

/// A module written as a shorthand block (its registered hash alone, no module bytes): the
/// decompressor reconstructs it from the hash through the runner, skipping the decompression
/// and hashing of a full module block, and decodes the archive the same. Without a registry
/// that knows the hash, the archive cannot be decoded.
#[test]
fn shorthand_module() {
    use sha2::Digest;
    let mut f = Formula::default();
    let d: Vec<u8> = (0..2000u32).map(|i| (i * 11 % 251) as u8).collect();
    let a = f.push("a", d.clone());
    f.root = Node::apply(REV, vec![1, 2, 3], vec![a], d.len() as u64);
    let want: Vec<u8> = d.iter().rev().copied().collect();
    // REV's module with a class of tasks, which its Apply declares a task limit for
    fn wasm_of_rev() -> Vec<u8> {
        let mut w = stand_in(REV);
        w.extend_from_slice(&[0, 16, 14]);
        w.extend_from_slice(b"aqr_task_slots");
        w.push(4);
        w
    }
    let wasm = wasm_of_rev();
    assert_eq!(task_classes(&wasm), 1);
    let hash: [u8; 32] = sha2::Sha512_256::digest(&wasm).into();
    // the archive names REV by its registered hash (a shorthand module block, type 0x60)
    fn reg(m: Module) -> Option<Wasm> {
        use sha2::Digest;
        let hash = sha2::Sha512_256::digest(wasm_of_rev()).into();
        (m == REV).then_some(Wasm::Registered { hash, classes: 1 })
    }
    let c = Encoder::new(Level::Zstd(1)).with_modules(reg).encode(&f, Some(checksum(&want))).unwrap();

    // a runner that reconstructs REV's module from the hash, then runs it (as natives does)
    struct Short<R> {
        inner: R,
        hash: [u8; 32],
        wasm: Vec<u8>,
    }
    impl<R: Runner> Runner for Short<R> {
        fn load(&self, w: &[u8]) -> Decoded<Box<dyn Loaded + '_>> {
            self.inner.load(w)
        }
        fn registered(&self, h: &[u8; 32]) -> Decoded<Vec<u8>> {
            if *h == self.hash { Ok(self.wasm.clone()) } else { resource("unknown registered module") }
        }
    }
    assert!(decode(&c, &Short { inner: natives(), hash, wasm }).unwrap() == want);
    // the default runner knows no registered modules, so the shorthand cannot be resolved
    assert!(matches!(decode(&c, &natives()), Err(Failure::Resource(_))));
}

/// Patches in archives: of a slice, and of an output of an application of several outputs
/// against another; one damaged, or of another length, is invalid.
#[test]
fn patches() {
    let noise = aqr_common::rng::noise(10_000);
    let mut target = noise[..6000].to_vec();
    target[100..200].fill(1);
    let mut f = Formula::default();
    let a = f.push("a", noise.clone());
    let d = f.push_as("d", Kind::Compressed, aqr_formula::patch::make(&noise[..6000], &target, 3).unwrap());
    let cut = aqr_formula::Apply {
        module: CUT,
        resources: vec![],
        params: vec![],
        inputs: vec![a],
        len: 0,
        memory_limit: aqr_formula::NO_LIMIT,
        task_limits: Vec::new(),
    };
    let [o0, o1] = <[Node; 2]>::try_from(f.share(cut, vec![6000, 4000])).unwrap();
    f.root = Node::concat([o1, Node::patch(o0, d, 6000)]);
    let want = [&noise[6000..], &target[..]].concat();
    let c = encode(&f, None, Level::Zstd(3)).unwrap();
    assert!(decode(&c, &natives()).unwrap() == want);
    let mut bad = f.clone();
    bad.streams[1].data[3] ^= 0x40;
    assert!(matches!(decode(&encode(&bad, None, Level::Zstd(3)).unwrap(), &natives()), Err(Failure::Invalid(_))));
    let Node::Concat(ns) = &mut f.root else { unreachable!() };
    let Node::Patch(p) = &mut ns[1] else { unreachable!() };
    p.len -= 1;
    assert!(matches!(decode(&encode(&f, None, Level::Zstd(3)).unwrap(), &natives()), Err(Failure::Invalid(_))));
}

/// Applications of several outputs: their outputs taken anywhere, by the root or by a later
/// one, each brought up the stack where it is taken. The archive gives what the formula
/// does; one whose outputs are not taken once each is not made.
#[test]
fn shared_outputs() {
    let mut f = Formula::default();
    let a = f.push("a", b"abcdefgh".to_vec());
    let cut = |inputs| aqr_formula::Apply {
        module: CUT,
        resources: vec![],
        params: vec![],
        inputs,
        len: 0,
        memory_limit: aqr_formula::NO_LIMIT,
        task_limits: Vec::new(),
    };
    let [o0, o1] = <[Node; 2]>::try_from(f.share(cut(vec![a]), vec![3, 5])).unwrap();
    let [p0, p1] = <[Node; 2]>::try_from(f.share(cut(vec![o1, Node::slice(0, 0, 2)]), vec![1, 6])).unwrap();
    f.root = Node::concat([p1, Node::slice(0, 7, 1), o0, Node::apply(REV, vec![1, 2, 3], vec![p0], 1)]);
    let c = encode(&f, None, Level::Zstd(3)).unwrap();
    assert_eq!(decode(&c, &natives()).unwrap(), b"efghabhabcd");
    // the outputs of the second taken in the other order
    let Node::Concat(ns) = &mut f.root else { unreachable!() };
    ns.swap(0, 3);
    let c = encode(&f, None, Level::Zstd(3)).unwrap();
    assert_eq!(decode(&c, &natives()).unwrap(), b"dhabcefghab");
    let mut twice = f.clone();
    twice.root = Node::concat([twice.root.clone(), Node::Output { shared: 0, index: 0, len: 3 }]);
    assert!(encode(&twice, None, Level::Zstd(3)).is_err());
}

/// A patch's output split ([`aqr_formula::Of::Patch`]): Zstandard blocks decompressed (of an empty
/// source), their pieces taken in another order, one dropped (the source of a patch of an
/// empty raw block). The archive gives what the formula does.
#[test]
fn patch_outputs() {
    let data: Vec<u8> = (0..300_000u32).map(|i| (i / 7 % 251) as u8).collect();
    let mut f = Formula::default();
    let blocks = f.push_as("zstd", Kind::Compressed, aqr_formula::patch::make(b"", &data, 3).unwrap());
    let drop = f.push("drop", vec![1, 0, 0]);
    let empty = Node::concat([]);
    let [o0, o1, o2] = <[Node; 3]>::try_from(f.split_patch(empty, blocks, vec![10, 200_000, 99_990])).unwrap();
    let drop = Node::Patch(Box::new(aqr_formula::Patch { source: o1, patch: drop, len: 0 }));
    f.root = Node::concat([o2, drop, Node::slice(0, 0, 4), o0]);
    let want = [&data[200_010..], &f.streams[0].data[..4], &data[..10]].concat();
    let none = |_: &aqr_formula::Apply, _: &[&[u8]]| -> aqr_formula::Result<Vec<u8>> { unreachable!("no module") };
    assert!(f.eval(&none).unwrap() == want);
    let c = encode(&f, None, Level::Zstd(3)).unwrap();
    assert!(decode(&c, &natives()).unwrap() == want);
    // an output not taken; a patch's output not split
    f.root = Node::concat([
        Node::Output { shared: 0, index: 2, len: 99_990 },
        Node::Output { shared: 0, index: 0, len: 10 },
    ]);
    assert!(f.check_outputs().is_err());
    f.shared[0].split = false;
    assert!(f.check_outputs().is_err());
}

/// A node's bytes split ([`aqr_formula::Of::Node`]): a stream's, then an application's, the pieces
/// taken in another order between other bytes, as a file's are in an image. The archive
/// gives what the formula does.
#[test]
fn node_outputs() {
    let data: Vec<u8> = (0..300_000u32).map(|i| (i / 7 % 251) as u8).collect();
    let mut f = Formula::default();
    let file = f.push_as("file", Kind::Unknown, data.clone());
    f.push("rest", b"0123456789".to_vec());
    let [o0, o1, o2] = <[Node; 3]>::try_from(f.split_node(file, vec![10, 200_000, 99_990])).unwrap();
    let rev = Node::apply(REV, vec![1, 2, 3], vec![Node::slice(1, 0, 10)], 10);
    let [r0, r1] = <[Node; 2]>::try_from(f.split_node(rev, vec![4, 6])).unwrap();
    f.root = Node::concat([o2, Node::slice(1, 0, 3), r1, o0, r0, o1]);
    let want = [&data[200_010..], b"012", b"543210", &data[..10], b"9876", &data[10..200_010]].concat();
    let rev = |_: &aqr_formula::Apply, i: &[&[u8]]| Ok(i.concat().into_iter().rev().collect());
    assert!(f.eval(&rev).unwrap() == want);
    let c = encode(&f, None, Level::Zstd(3)).unwrap();
    assert!(decode(&c, &natives()).unwrap() == want);
    f.shared[0].split = false;
    assert!(f.check_outputs().is_err());
}

/// Streams merged by class without moving their bytes ([`aqr_formula::Formula::merged`]) archive
/// as those merged ([`aqr_formula::Formula::merge_classes`]), byte for byte, the merged stream
/// over several blocks too; and the encoder keeps none of the blocks it wrote.
#[test]
fn merged_in_pieces() {
    let mut f = Formula::default();
    let noise: Vec<u8> = (0..3 * BLOCK as u32 / 2).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
    let a = f.push("a", noise[..BLOCK].to_vec());
    let b = f.push("b", b"elsewhere".to_vec());
    let a2 = f.push("a", noise[BLOCK..].to_vec());
    let a3 = f.push("a", b"third".to_vec());
    let rev = Node::apply(REV, vec![1, 2, 3], vec![Node::slice(2, 10, 100)], 100);
    f.root = Node::concat([a, Node::slice(2, 5, BLOCK as u64 / 2 - 5), b, rev, a3, a2]);
    let want = f.eval(&|_: &aqr_formula::Apply, i: &[&[u8]]| Ok(i.concat().into_iter().rev().collect())).unwrap();
    let sum = Some(checksum(&want));
    let mut whole = f.clone();
    whole.merge_classes();
    let enc = Encoder::new(Level::Zstd(1));
    let one = enc.encode(&whole, sum).unwrap();
    let (merged, groups) = f.merged();
    let whole = |i: usize| (i, 0, f.streams[i].data.len() as u64);
    assert_eq!(groups, [vec![whole(0), whole(2), whole(3)], vec![whole(1)]]);
    let pieces: Vec<Vec<&[u8]>> = groups
        .iter()
        .map(|g| g.iter().map(|&(i, a, b)| &f.streams[i].data[a as usize..b as usize]).collect())
        .collect();
    let enc = Encoder::new(Level::Zstd(1));
    assert!(enc.encode_pieces(&merged, &pieces, sum).unwrap() == one);
    assert!(enc.streams.borrow().is_empty());
    assert!(decode(&one, &natives()).unwrap() == want);
}

/// Formulas sealed into a sink one after another, then embedded in one of a stream of its
/// own, archive and give their bytes back: the second's slice of the same bytes as one of
/// the first's, and its slice starting with them, written once (but a slice of part of one
/// sealed is not found); a stream of [`Kind::Compressed`] a stream of the sink of its own.
#[test]
fn sealed() {
    let same: Vec<u8> = (0..64u8).collect();
    let mut first = Formula::default();
    let a = first.push("a", same.clone());
    let b = first.push("b", b"of the first".to_vec());
    let c = first.push_as("c", Kind::Compressed, b"compressed".to_vec());
    first.root = Node::concat([a, b, c]);
    let mut second = Formula::default();
    second.push("a", [&b"own "[..], &same, &same, b" and more"].concat());
    second.root = Node::concat([Node::slice(0, 4, 64), Node::slice(0, 0, 4), Node::slice(0, 68, 73)]);
    let id = |_: &aqr_formula::Apply, i: &[&[u8]]| Ok(i.concat());
    let want = [first.eval(&id).unwrap(), second.eval(&id).unwrap(), b"top".to_vec()].concat();
    let enc = Encoder::new(Level::Zstd(1));
    let sink = sink::Sink::new(&enc, 16);
    sink.seal(&mut first).unwrap();
    sink.seal(&mut second).unwrap();
    assert!(first.streams.iter().chain(&second.streams).all(|s| s.data.sunk_in().is_some()));
    let mut top = Formula::default();
    let own = top.push("own", b"top".to_vec());
    let (x, y) = (top.embed(first), top.embed(second));
    top.root = Node::concat([x, y, own]);
    let sunk = sink.close().unwrap();
    // "a": the 64 bytes once, "own " (under the least shared), " and more" after them
    let lens: Vec<(&str, u64)> = sunk.iter().map(|s| (&s.class[..], s.len)).collect();
    assert_eq!(lens, [("a", 64 + 4 + 9), ("b", 12), ("c", 10)]);
    let (mut merged, groups) = top.merged();
    merged.streams.extend(sunk.iter().map(|s| aqr_formula::Stream {
        class: s.class.clone(),
        kind: s.kind,
        content: false,
        apart: false,
        data: aqr_formula::Bytes::sunk(0, s.len as usize),
    }));
    let pieces: Vec<Vec<&[u8]>> = groups
        .iter()
        .map(|g| g.iter().map(|&(i, a, b)| &top.streams[i].data[a as usize..b as usize]).collect())
        .collect();
    let mut out = Vec::new();
    enc.encode_sunk_to(&merged, &pieces, &sunk, Some(checksum(&want)), &mut out).unwrap().unwrap();
    assert!(decode(&out, &natives()).unwrap() == want);
}

/// Bytes written at offsets, in memory.
#[derive(Default)]
struct Placed(Mutex<Vec<u8>>);

impl sink::Place for Placed {
    fn write_at(&self, at: u64, d: &[u8]) -> io::Result<()> {
        let mut v = self.0.lock().unwrap();
        let end = at as usize + d.len();
        if v.len() < end {
            v.resize(end, 0);
        }
        v[at as usize..end].copy_from_slice(d);
        Ok(())
    }
}

/// A direct sink writes its blocks as they fill, in the order they were filled, a stream of
/// a formula's own (of [`Kind::Compressed`], or kept apart) as soon as the formula is sealed:
/// by the end it holds the blocks of the streams it shares, not full, at most. With the
/// module and formula blocks in the room left before them and a spacer of the rest, the
/// archive gives the formulas' output back, the same whatever the threads.
#[test]
fn sealed_direct() {
    let noise = |n: usize, seed: u32| aqr_common::rng::Rng::new(seed as u64 + 1).bytes(n);
    let parts: Vec<Formula> = (0..5)
        .map(|i| {
            let mut f = Formula::default();
            let a = f.push_as("syms", Kind::Compressed, noise(BLOCK / 2 + i * 1000, i as u32));
            let b = f.push("side", b"of a part, some text. ".repeat(10_000 + i * 100));
            let c = f.push("apart", noise(3000 + i, 7 * i as u32));
            f.streams[2].apart = true;
            f.root = Node::concat([b, a, c]);
            f
        })
        .collect();
    let id = |_: &aqr_formula::Apply, i: &[&[u8]]| Ok(i.concat());
    let want: Vec<u8> = parts.iter().flat_map(|f| f.eval(&id).unwrap()).chain(*b"top").collect();
    let room = 8192u64;
    let archive = |threads: usize| {
        let enc = Encoder::with_threads(Level::Zstd(1), threads);
        let place = Arc::new(Placed::default());
        let at = HEADER_MAGIC.len() as u64 + room;
        let sink = sink::Sink::direct(&enc, 1024, place.clone(), at, 2);
        let mut top = Formula::default();
        let mut nodes = Vec::new();
        for mut f in parts.clone() {
            sink.seal(&mut f).unwrap();
            assert!(f.streams.iter().all(|s| s.data.sunk_in().is_some()));
            nodes.push(top.embed(f));
        }
        // (each part's own streams written, but the 2 that may wait to be compressed)
        let before = (place.0.lock().unwrap().len() as u64).saturating_sub(at);
        assert!(before >= 3 * BLOCK as u64 / 2, "{before} bytes written before the end");
        nodes.push(top.push("own", b"top".to_vec()));
        top.root = Node::concat(nodes);
        sink.seal(&mut top).unwrap();
        let (sunk, end) = sink.close_direct().unwrap();
        let (mut merged, groups) = top.merged();
        assert!(groups.is_empty());
        merged.streams.extend(sunk.iter().enumerate().map(|(k, s)| aqr_formula::Stream {
            class: s.class.clone(),
            kind: s.kind,
            content: false,
            apart: false,
            data: aqr_formula::Bytes::sunk(k, s.len as usize),
        }));
        let head = enc.direct_head(&merged, &sunk, Some(checksum(&want))).unwrap();
        assert!(head.len() as u64 + 2 <= room);
        use sink::Place;
        place.write_at(0, &HEADER_MAGIC).unwrap();
        place.write_at(HEADER_MAGIC.len() as u64, &head).unwrap();
        place.write_at((HEADER_MAGIC.len() + head.len()) as u64, &kill(room as usize - head.len())).unwrap();
        place.write_at(end, &footer_bytes((end - HEADER_MAGIC.len() as u64) as usize)).unwrap();
        Arc::into_inner(place).unwrap().0.into_inner().unwrap()
    };
    let one = archive(1);
    assert!(decode(&one, &natives()).unwrap() == want);
    assert!(archive(4) == one);
}

/// Streams written to a sink, a piece at a time (over a block's end), archive as those held
/// whole, byte for byte: all of them, or those after the first (at a level picking levels by
/// samples too).
#[test]
fn sunk_as_held() {
    let mut f = Formula::default();
    let noise: Vec<u8> = (0..3 * BLOCK as u32 / 2).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
    let text: Vec<u8> = b"some text, some more text. ".iter().copied().cycle().take(BLOCK + 1000).collect();
    let a = f.push("a", noise.clone());
    let b = f.push_as("b", Kind::Text, text.clone());
    let c = f.push("c", b"short".to_vec());
    f.root = Node::concat([b, Node::slice(0, BLOCK as u64 - 7, 20), c, a]);
    let want = f.eval(&|_: &aqr_formula::Apply, i: &[&[u8]]| Ok(i.concat())).unwrap();
    let sum = Some(checksum(&want));
    for level in [Level::Zstd(1), Level::MID] {
        let enc = Encoder::new(level);
        let held = enc.encode(&f, sum).unwrap();
        for own in [0, 1] {
            let sink = sink::Sink::new(&enc, 1024);
            for st in &f.streams[own..] {
                let s = sink.stream(st.class.clone(), st.kind);
                let d = &st.data[..];
                let (x, y) = (d.len() / 3, 2 * d.len() / 3);
                sink.append(s, &[&d[..x], &d[x..y]]).unwrap();
                sink.append(s, &[&d[y..]]).unwrap();
            }
            let sunk = sink.close().unwrap();
            let pieces: Vec<Vec<&[u8]>> = f.streams[..own].iter().map(|s| vec![&s.data[..]]).collect();
            let mut out = Vec::new();
            enc.encode_sunk_to(&f, &pieces, &sunk, sum, &mut out).unwrap().unwrap();
            assert!(out == held, "{level:?}, {own} held");
        }
        assert!(decode(&held, &natives()).unwrap() == want);
    }
}

/// A stream of more than [`BLOCK`] bytes is several non-leaf blocks: slices within one,
/// over a boundary and of all of it come back, and the archive has a block for each.
/// The blocks `f` slices listed, of its streams in order.
fn ordinals_of(f: &Formula) -> Vec<Vec<Option<u64>>> {
    let mut nodes: Vec<&Node> = f.shared.iter().flat_map(|s| s.inputs()).collect();
    nodes.push(&f.root);
    listed(&sliced(f.streams.len(), |s| f.streams[s].data.len(), nodes), 0..f.streams.len()).0
}

/// A formula slicing some blocks of a stream alone: only those are written and listed.
#[test]
fn some_blocks_of_a_stream() {
    let mut f = Formula::default();
    let n = 3 * BLOCK + 1000;
    let d: Vec<u8> = (0..n as u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
    f.push("a", d.clone());
    let b = BLOCK as u64;
    let parts = [(5, 10), (2 * b + 10, 990), (3 * b, 7)];
    f.root = Node::concat(parts.iter().map(|&(s, l)| Node::slice(0, s, l)));
    let want: Vec<u8> = parts.iter().flat_map(|&(s, l)| d[s as usize..][..l as usize].to_vec()).collect();
    let c = encode(&f, Some(checksum(&want)), Level::Zstd(1)).unwrap();
    assert!(decode(&c, &natives()).unwrap() == want);
    assert_eq!(open(&c, &natives()).unwrap().parsed().unwrap().non_leaf.len(), 3);
}

#[test]
fn streams_of_several_blocks() {
    let mut f = Formula::default();
    let n = 2 * BLOCK + 1000;
    let d: Vec<u8> = (0..n as u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
    f.push("a", d.clone());
    let b = BLOCK as u64;
    let parts = [(5, 10), (b - 3, 7), (2 * b + 10, 990), (0, n as u64), (b, b)];
    f.root = Node::concat(parts.iter().map(|&(s, l)| Node::slice(0, s, l)));
    let want: Vec<u8> = parts.iter().flat_map(|&(s, l)| d[s as usize..][..l as usize].to_vec()).collect();
    assert!(
        description_bound(&f.root) >= {
            let ordinals = ordinals_of(&f);
            let mut out = Vec::new();
            Emit {
                ordinals: &ordinals,
                literals: &[],
                modules: &[],
                classes: &[],
                resources: &[],
                out: &mut out,
                stack: vec![],
            }
            .node(&f.root);
            out.len()
        }
    );
    let c = encode(&f, Some(checksum(&want)), Level::Zstd(1)).unwrap();
    assert!(decode(&c, &natives()).unwrap() == want);
    assert_eq!(open(&c, &natives()).unwrap().parsed().unwrap().non_leaf.len(), 3);
}

/// The slices of streams of few bytes are Literal instructions where `formula:literal-max`
/// allows (those streams no blocks), of the others slices of their blocks; the output the
/// same either way.
#[test]
fn small_streams_as_literals() {
    let mut f = Formula::default();
    let big: Vec<u8> = (0..5000u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
    f.push("a", b"tiny header".to_vec());
    f.push("b", big);
    f.push("c", b"xy".to_vec());
    let rev = Node::apply(REV, vec![1, 2, 3], vec![Node::slice(0, 4, 7), Node::slice(2, 0, 2)], 9);
    f.root = Node::concat([Node::slice(0, 0, 4), Node::slice(1, 0, 5000), rev, Node::slice(0, 0, 4)]);
    let want = f.eval(&|_: &aqr_formula::Apply, i: &[&[u8]]| Ok(i.concat().into_iter().rev().collect())).unwrap();
    // (stream 0 sliced for 15 bytes, stream 2 for 2)
    for (max, blocks) in [(0, 3), (14, 2), (15, 1), (1 << 20, 0)] {
        let _max = LITERAL_MAX.set_here(max);
        let c = encode(&f, Some(checksum(&want)), Level::Zstd(1)).unwrap();
        assert_eq!(decode(&c, &natives()).unwrap(), want, "at {max}");
        assert_eq!(open(&c, &natives()).unwrap().parsed().unwrap().non_leaf.len(), blocks, "at {max}");
    }
}

/// A file whose formula is a concatenation (of slices, applications and a concatenation in
/// it) is written out part by part, as decoding gives it; its checksum is checked as it is
/// written, and a wrong one fails.
#[test]
fn written_part_by_part() {
    let mut f = Formula::default();
    let d: Vec<u8> = (0..3 * BLOCK as u32 + 77).map(|i| (i % 253) as u8).collect();
    f.push("a", d.clone());
    let rev = |start, len| Node::apply(REV, vec![1, 2, 3], vec![Node::slice(0, start, len)], len);
    let inner = Node::concat([Node::slice(0, 5, 100), rev(BLOCK as u64 - 10, 20), Node::slice(0, 0, 3)]);
    f.root = Node::concat([Node::slice(0, 2 * BLOCK as u64, BLOCK as u64 + 77), inner, rev(7, 1000)]);
    let want = f.eval(&|_: &aqr_formula::Apply, i: &[&[u8]]| Ok(i.concat().into_iter().rev().collect())).unwrap();
    let c = encode(&f, Some(checksum(&want)), Level::Zstd(1)).unwrap();
    let runner = natives();
    let a = open(&c, &runner).unwrap();
    let (mut out, mut parts) = (Vec::new(), 0);
    let n = a
        .write_file(2, &mut |p: &[u8]| {
            out.extend_from_slice(p);
            parts += 1;
            Ok(())
        })
        .unwrap();
    assert!(out == want && n == want.len() as u64);
    assert!(parts >= 5, "{parts} parts");
    // a checksum of something else
    let c = encode(&f, Some(checksum(b"other")), Level::Zstd(1)).unwrap();
    let a = open(&c, &runner).unwrap();
    assert_eq!(a.write_file(2, &mut |_: &[u8]| Ok(())), invalid("output checksum mismatch"));
}

/// A file of many applications concatenated (some of applications, of concatenations, of
/// splits) is written out alike whether its parts are evaluated ahead on workers or not.
#[test]
fn parts_made_ahead() {
    let mut f = Formula::default();
    let d: Vec<u8> = (0..2 * BLOCK as u32 + 99).map(|i| (i * 7 % 251) as u8).collect();
    let a = f.push("a", d.clone());
    let rev = |inputs: Vec<Node>, len| Node::apply(REV, vec![1, 2, 3], inputs, len);
    let split = aqr_formula::Apply {
        module: REV,
        resources: vec![],
        params: vec![1, 2, 3],
        inputs: vec![a],
        len: 0,
        memory_limit: aqr_formula::NO_LIMIT,
        task_limits: Vec::new(),
    };
    let [o0, o1] = <[Node; 2]>::try_from(f.split(split, vec![40, d.len() as u64 - 40])).unwrap();
    let mut parts = vec![rev(vec![o1], d.len() as u64 - 40)];
    for k in 0..40u64 {
        let at = k * 3001 % (d.len() as u64 - 2000);
        parts.push(match k % 4 {
            0 => rev(vec![Node::slice(0, at, 1000)], 1000),
            1 => rev(vec![rev(vec![Node::slice(0, at, 500)], 500), Node::slice(0, at + 7, 30)], 530),
            2 => rev(vec![Node::concat([Node::slice(0, at, 10), Node::slice(0, at + 99, 20)])], 30),
            _ => Node::slice(0, at, 77),
        });
    }
    parts.push(o0);
    f.root = Node::concat(parts);
    let want = f.eval(&|_: &aqr_formula::Apply, i: &[&[u8]]| Ok(i.concat().into_iter().rev().collect())).unwrap();
    let c = encode(&f, Some(checksum(&want)), Level::Zstd(1)).unwrap();
    let runner = natives();
    let a = open(&c, &runner).unwrap();
    for threads in [1, 4, 16] {
        let mut out = Vec::new();
        let n = a
            .write_file(threads, &mut |p: &[u8]| {
                out.extend_from_slice(p);
                Ok(())
            })
            .unwrap();
        assert!(out == want && n == want.len() as u64, "{threads} threads");
    }
}

/// Outputs split from the one output of a module (Split instructions), taken as those of
/// several outputs are.
#[test]
fn split_outputs() {
    let mut f = Formula::default();
    let a = f.push("a", b"abcdefgh".to_vec());
    let rev = aqr_formula::Apply {
        module: REV,
        resources: vec![],
        params: vec![1, 2, 3],
        inputs: vec![a],
        len: 0,
        memory_limit: aqr_formula::NO_LIMIT,
        task_limits: Vec::new(),
    };
    let [o0, o1, o2] = <[Node; 3]>::try_from(f.split(rev, vec![3, 0, 5])).unwrap();
    f.root = Node::concat([o2, Node::slice(0, 0, 1), o1, o0]);
    assert!(f.to_string().starts_with("shared #0 rev@1 split 3 0 5"));
    let c = encode(&f, None, Level::Zstd(3)).unwrap();
    assert_eq!(decode(&c, &natives()).unwrap(), b"edcbaahgf");
}

/// The outputs of a node of several (a split, an application of several outputs) pulled
/// out of their turn, through a packed byte sequence too, are held until they are (beyond
/// what memory holds of them, in a file): decoded and written out alike.
#[test]
fn pulled_out_of_turn() {
    let mut f = Formula::default();
    let d: Vec<u8> = (0..BLOCK as u32 + 5000).map(|i| (i * 13 % 247) as u8).collect();
    let n = d.len() as u64;
    let a = f.push("a", d.clone());
    let rev = |inputs| aqr_formula::Apply {
        module: REV,
        resources: vec![],
        params: vec![1, 2, 3],
        inputs,
        len: 0,
        memory_limit: aqr_formula::NO_LIMIT,
        task_limits: Vec::new(),
    };
    let [o0, o1, o2] = <[Node; 3]>::try_from(f.split(rev(vec![a]), vec![100, n - 300, 200])).unwrap();
    let cut = aqr_formula::Apply { module: CUT, params: vec![], ..rev(vec![Node::slice(0, 7, 3000)]) };
    let [s0, s1] = <[Node; 2]>::try_from(f.share(cut, vec![1000, 2000])).unwrap();
    f.root = Node::concat([o2, Node::pack([s1, o0, Node::slice(0, 1, 9)]), o1, s0]);
    let r: Vec<u8> = d.iter().rev().copied().collect();
    let (n, s) = (n as usize, &d[7..3007]);
    let packed = packed::pack(&[&s[1000..], &r[..100], &d[1..10]]);
    let want = [&r[n - 200..], &packed, &r[100..n - 200], &s[..1000]].concat();
    let c = encode(&f, Some(checksum(&want)), Level::Zstd(1)).unwrap();
    let runner = natives();
    let files = spool::FILES.load(Ordering::Relaxed);
    assert!(decode(&c, &runner).unwrap() == want);
    // (more held than in memory, in a file)
    assert!(spool::FILES.load(Ordering::Relaxed) > files);
    let a = open(&c, &runner).unwrap();
    for threads in [1, 4] {
        let mut out = Vec::new();
        a.write_file(threads, &mut |p: &[u8]| {
            out.extend_from_slice(p);
            Ok(())
        })
        .unwrap();
        assert!(out == want, "{threads} threads");
    }
}

/// Runs `cut@1` as a part module, its inputs pulled and its outputs pushed [`PIECE`] bytes
/// at a time (failing midway where its parameters are `[0xff]`), and `rev@1` whole.
struct PartCut;

const PIECE: u64 = 100_000;

/// The applications [`PartCut`] made as part modules.
static PARTS: AtomicUsize = AtomicUsize::new(0);

/// Held by each test running [`PartCut`], as one counts [`PARTS`].
static PART_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The memory and task limits of the applications of `rev@1` with a memory limit of 1234.
static LIMITS: std::sync::Mutex<Vec<Vec<u64>>> = std::sync::Mutex::new(Vec::new());

struct Stand {
    cut: bool,
}

impl Runner for PartCut {
    fn load(&self, wasm: &[u8]) -> Decoded<Box<dyn Loaded + '_>> {
        match stand_in_name(wasm) {
            Some("cut@1") => Ok(Box::new(Stand { cut: true })),
            Some("rev@1") => Ok(Box::new(Stand { cut: false })),
            _ => resource("an unknown module"),
        }
    }
}

impl Loaded for Stand {
    fn apply(
        &self,
        _: &[u8],
        _: &[Resource],
        inputs: &[&[u8]],
        sizes: &[u64],
        memory_limit: u64,
        task_limits: &[u64],
    ) -> Decoded<Vec<Vec<u8>>> {
        assert!(!self.cut, "a part module applied whole");
        if memory_limit == 1234 {
            LIMITS.lock().unwrap().push(task_limits.to_vec());
        }
        assert_eq!(sizes.len(), 1);
        Ok(vec![inputs.concat().into_iter().rev().collect()])
    }

    fn is_part(&self) -> bool {
        self.cut
    }

    fn apply_part(
        &self,
        params: &[u8],
        _: &[Resource],
        inputs: &mut [&mut dyn PartIn],
        outputs: &mut [&mut dyn PartOut],
        sizes: &[u64],
        _: u64,
        _: &[u64],
    ) -> Decoded<()> {
        PARTS.fetch_add(1, Ordering::Relaxed);
        if params == [0xaa] {
            // (met by the others: applied one after another, they would wait in vain)
            RENDEZVOUS.fetch_add(1, Ordering::SeqCst);
            let start = std::time::Instant::now();
            while RENDEZVOUS.load(Ordering::SeqCst) < 2 {
                if start.elapsed().as_secs() >= 10 {
                    return invalid("not applied at the same time");
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        let (mut j, mut left, mut pulled) = (0, sizes[0], 0u64);
        for i in inputs {
            let mut at = 0;
            while at < i.len() {
                let hi = (at + PIECE).min(i.len());
                let mut buf = Vec::new();
                i.pull(hi, &mut |d| buf.extend_from_slice(d))?;
                (at, pulled) = (hi, pulled + hi - at);
                if params == [0xff] && pulled > 1 << 20 {
                    return invalid("failed midway");
                }
                if params == [0xee] {
                    // (more than declared, of the one output)
                    buf.extend_from_slice(&[0; 10]);
                    outputs[0].push(&buf)?;
                    continue;
                }
                let mut d = &buf[..];
                while !d.is_empty() {
                    while left == 0 {
                        j += 1;
                        left = *sizes.get(j).ok_or_else(|| Failure::Invalid("inputs too long".into()))?;
                    }
                    let n = d.len().min(left as usize);
                    outputs[j].push(&d[..n])?;
                    (d, left) = (&d[n..], left - n as u64);
                }
            }
        }
        Ok(())
    }
}

/// The task limits of an application go into the archive as an Apply instruction and come
/// back to the module as they were; one of none is an Apply instruction.
#[test]
fn task_limits() {
    let _alone = PART_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let mut f = Formula::default();
    let d: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
    f.push("a", d.clone());
    let rev = |task_limits: Vec<u64>| {
        Node::Apply(Box::new(aqr_formula::Apply {
            module: REV,
            resources: vec![],
            params: vec![],
            inputs: vec![Node::slice(0, 0, 1000)],
            len: 1000,
            memory_limit: 1234,
            task_limits,
        }))
    };
    f.root = Node::concat([rev(vec![7, 9]), rev(vec![7, 9])]);
    let r: Vec<u8> = d.iter().rev().copied().collect();
    let want = [&r[..], &r].concat();
    // (a module of two classes of tasks)
    fn two_classes(m: Module) -> Option<Wasm> {
        let mut w = stand_in(m);
        let name = b"aqr_task_slots";
        w.extend_from_slice(&[0, name.len() as u8 + 3, name.len() as u8]);
        w.extend_from_slice(name);
        w.extend_from_slice(&[8, 4]);
        Some(Wasm::Bytes(Cow::Owned(w)))
    }
    let c = Encoder::new(Level::Zstd(1)).with_modules(two_classes).encode(&f, Some(checksum(&want))).unwrap();
    let mut wrong = f.clone();
    if let Node::Concat(ns) = &mut wrong.root {
        let Node::Apply(a) = &mut ns[0] else { unreachable!() };
        a.task_limits = vec![7, 8, 9];
    }
    let bad = Encoder::new(Level::Zstd(1)).with_modules(two_classes).encode(&wrong, Some(checksum(&want))).unwrap();
    assert!(matches!(decode(&bad, &PartCut), Err(Failure::Invalid(m)) if m.contains("task limits")));
    LIMITS.lock().unwrap().clear();
    assert!(decode(&c, &PartCut).unwrap() == want);
    let mut seen = std::mem::take(&mut *LIMITS.lock().unwrap());
    seen.sort();
    assert_eq!(seen, [vec![7, 9], vec![7, 9]]);
}

/// An application whose minimal bound (the memory limit and the sum of the task limits) is
/// over the decoder's limit, 512 MiB by default, fails as a resource failure before its
/// module is applied; one of exactly that is applied.
#[test]
fn minimal_bound_limit() {
    let _alone = PART_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(aqr_formula::application_limit(), 512 << 20);
    let d: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
    // (a module of two classes of tasks)
    fn two_classes(m: Module) -> Option<Wasm> {
        let mut w = stand_in(m);
        let name = b"aqr_task_slots";
        w.extend_from_slice(&[0, name.len() as u8 + 3, name.len() as u8]);
        w.extend_from_slice(name);
        w.extend_from_slice(&[8, 4]);
        Some(Wasm::Bytes(Cow::Owned(w)))
    }
    let at = |memory_limit: u64, task_limits: Vec<u64>| {
        let mut f = Formula::default();
        f.push("a", d.clone());
        f.root = Node::Apply(Box::new(aqr_formula::Apply {
            module: REV,
            resources: vec![],
            params: vec![],
            inputs: vec![Node::slice(0, 0, 1000)],
            len: 1000,
            memory_limit,
            task_limits,
        }));
        let want: Vec<u8> = d.iter().rev().copied().collect();
        let c = Encoder::new(Level::Zstd(1)).with_modules(two_classes).encode(&f, Some(checksum(&want))).unwrap();
        decode(&c, &PartCut).map(|r| r == want)
    };
    let m = 1 << 20;
    assert!(at(512 * m, vec![0, 0]).unwrap());
    assert!(at(256 * m, vec![128 * m, 128 * m]).unwrap());
    assert!(matches!(at(512 * m + 1, vec![0, 0]), Err(Failure::Resource(t)) if t.contains("--application-bound")));
    assert!(matches!(at(m, vec![512 * m, 0]), Err(Failure::Resource(_))));
}

/// Part modules applied as their outputs are pulled, each on a thread of its own: one of a
/// single output in another's input (more than its queue holds, so that it waits for it to
/// be pulled), whose outputs are pulled out of their turn; decoded and written out alike.
/// One failing midway fails the archive, the one it pulls of let go of rather than left
/// waiting.
#[test]
fn part_modules() {
    let _alone = PART_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let mut f = Formula::default();
    let n = (24 << 20) + 77u64;
    let d: Vec<u8> = (0..n as u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    let a = f.push("a", d.clone());
    let cut = |inputs, params| aqr_formula::Apply {
        module: CUT,
        resources: vec![],
        params,
        inputs,
        len: 0,
        memory_limit: aqr_formula::NO_LIMIT,
        task_limits: Vec::new(),
    };
    let inner = f.share(cut(vec![a.clone()], vec![]), vec![n]).pop().unwrap();
    let [o0, o1] = <[Node; 2]>::try_from(f.share(cut(vec![inner], vec![]), vec![n - 1000, 1000])).unwrap();
    let rev = Node::apply(REV, vec![1, 2, 3], vec![Node::slice(0, 5, 100)], 100);
    f.root = Node::concat([o1, rev, o0]);
    let r: Vec<u8> = d[5..105].iter().rev().copied().collect();
    let want = [&d[n as usize - 1000..], &r, &d[..n as usize - 1000]].concat();
    let c = encode(&f, Some(checksum(&want)), Level::Zstd(1)).unwrap();
    let (before, files) = (PARTS.load(Ordering::Relaxed), spool::FILES.load(Ordering::Relaxed));
    assert!(decode(&c, &PartCut).unwrap() == want);
    assert_eq!(PARTS.load(Ordering::Relaxed) - before, 2);
    // (more queued than in memory, in a file)
    assert!(spool::FILES.load(Ordering::Relaxed) > files);
    let archive = open(&c, &PartCut).unwrap();
    for threads in [1, 4] {
        let mut out = Vec::new();
        archive
            .write_file(threads, &mut |p: &[u8]| {
                out.extend_from_slice(p);
                Ok(())
            })
            .unwrap();
        assert!(out == want, "{threads} threads");
    }
    // failing midway, of the input it pulls
    let mut f = Formula::default();
    let a = f.push("a", d);
    let inner = f.share(cut(vec![a], vec![]), vec![n]).pop().unwrap();
    f.root = f.share(cut(vec![inner], vec![0xff]), vec![n]).pop().unwrap();
    let c = encode(&f, None, Level::Zstd(1)).unwrap();
    assert_eq!(decode(&c, &PartCut), invalid("failed midway"));
}

/// A part module pushing more bytes than its output is declared to have, to a Split of it,
/// is an invalid archive.
#[test]
fn split_of_a_longer_output() {
    let _alone = PART_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let mut f = Formula::default();
    let a = f.push("a", vec![7; 1000]);
    let cut = aqr_formula::Apply {
        module: CUT,
        resources: vec![],
        params: vec![0xee],
        inputs: vec![a],
        len: 0,
        memory_limit: aqr_formula::NO_LIMIT,
        task_limits: Vec::new(),
    };
    f.root = Node::concat(f.split(cut, vec![400, 600]));
    let c = encode(&f, None, Level::Zstd(1)).unwrap();
    assert!(matches!(decode(&c, &PartCut), Err(Failure::Invalid(_))));
}

static RENDEZVOUS: AtomicUsize = AtomicUsize::new(0);

/// The applications of part modules among the parts of an output run at the same time on
/// the pool (each waits for another to have begun), their outputs written in order.
#[test]
fn parts_ahead() {
    let _alone = PART_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let mut f = Formula::default();
    let d: Vec<u8> = (0..3000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    f.push("a", d.clone());
    let cut = |k: u64| Node::apply(CUT, vec![0xaa], vec![Node::slice(0, k * 1000, 1000)], 1000);
    f.root = Node::concat([cut(0), cut(1), cut(2)]);
    let want = d.clone();
    let c = encode(&f, Some(checksum(&want)), Level::Zstd(1)).unwrap();
    let archive = open(&c, &PartCut).unwrap();
    let mut out = Vec::new();
    RENDEZVOUS.store(0, Ordering::SeqCst);
    archive
        .write_file(4, &mut |p: &[u8]| {
            out.extend_from_slice(p);
            Ok(())
        })
        .unwrap();
    assert!(out == want);
}

/// Many outputs of applications taken in order with slices between them, in a concatenation
/// in another (as aqr-wim's of a WIM file in an image): the description grows with the
/// outputs, not with their square (a Swap for each item above an output took 3,000
/// outputs to 40 MB), nor do the bytes concatenated (each item so far put together before
/// each output copied them so), and the outputs come back where they are.
#[test]
fn many_outputs() {
    let mut f = Formula::default();
    f.push("a", (0..6000u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>());
    let mut parts = vec![Node::slice(0, 0, 3)];
    let mut inner = Vec::new();
    for k in 0..3 {
        let rev = aqr_formula::Apply {
            module: REV,
            resources: vec![],
            params: vec![1, 2, 3],
            inputs: vec![Node::slice(0, k * 2000, 2000)],
            len: 0,
            memory_limit: aqr_formula::NO_LIMIT,
            task_limits: Vec::new(),
        };
        for (i, o) in f.split(rev, vec![2; 1000]).into_iter().enumerate() {
            inner.push(Node::slice(0, i as u64, 1));
            inner.push(o);
        }
    }
    parts.push(Node::concat(inner));
    parts.push(Node::slice(0, 5, 2));
    f.root = Node::concat(parts);
    let want: Vec<u8> = {
        let d = &f.streams[0].data;
        let mut w = d[..3].to_vec();
        for k in 0..3 {
            let r: Vec<u8> = d[k * 2000..k * 2000 + 2000].iter().rev().copied().collect();
            for i in 0..1000 {
                w.push(d[i]);
                w.extend_from_slice(&r[i * 2..i * 2 + 2]);
            }
        }
        w.extend_from_slice(&d[5..7]);
        w
    };
    let ordinals = ordinals_of(&f);
    let mut out = Vec::new();
    let mut e = Emit {
        ordinals: &ordinals,
        literals: &[],
        modules: &[REV],
        classes: &[],
        resources: &[],
        out: &mut out,
        stack: vec![],
    };
    for (i, s) in f.shared.iter().enumerate() {
        e.shared(i, s);
    }
    e.node(&f.root);
    assert_eq!(e.stack, [None]);
    assert!(out.len() < 3000 * 64, "{} bytes of description", out.len());
    // the bytes the Concat instructions copy: each of the outputs' a few times
    let (mut stack, mut copied) = (Vec::new(), 0usize);
    let mut r = &out[..];
    while let Some((&op, rest)) = r.split_first() {
        let mut at = 0;
        let num = |at: &mut usize| varint::get(rest, at).unwrap() as usize;
        match op {
            SLICE => {
                let (_, _, len) = (num(&mut at), num(&mut at), num(&mut at));
                stack.push(len);
            }
            APPLY => {
                let (_, nr, np) = (num(&mut at), num(&mut at), num(&mut at));
                assert_eq!((nr, np), (0, 3));
                at += np;
                let (_, nt) = (num(&mut at), num(&mut at));
                assert_eq!(nt, 0);
                let (ni, no) = (num(&mut at), num(&mut at));
                let sizes: Vec<usize> = (0..no).map(|_| num(&mut at)).collect();
                stack.truncate(stack.len() - ni);
                stack.extend(sizes);
            }
            SPLIT => {
                let n = num(&mut at);
                let sizes: Vec<usize> = (0..n).map(|_| num(&mut at)).collect();
                stack.pop();
                stack.extend(sizes);
            }
            SWAP => {
                let d = num(&mut at);
                let top = stack.len() - 1;
                stack.swap(top, top - d);
            }
            CONCAT => {
                let n = num(&mut at);
                let len: usize = stack.drain(stack.len() - n..).sum();
                copied += len;
                stack.push(len);
            }
            _ => panic!("{op}"),
        }
        r = &rest[at..];
    }
    assert_eq!(stack, [want.len()]);
    assert!(copied < want.len() * 8, "{copied} bytes copied of {}", want.len());
    let c = encode(&f, None, Level::Zstd(3)).unwrap();
    assert!(decode(&c, &natives()).unwrap() == want);
}

fn sample() -> Formula {
    let mut f = Formula::default();
    let a = f.push("a", b"hello".to_vec());
    let b = f.push("b", b"xyz".to_vec());
    f.root = Node::concat([Node::slice(0, 0, 2), Node::apply(REV, vec![1, 2, 3], vec![a, b], 8), Node::slice(1, 1, 2)]);
    f
}

const SAMPLE: &[u8] = b"hezyxollehyz";

/// Nodes of the bytes of a module block and of a resource block as stored push those of the
/// blocks the archive has: the module's own (which the nodes' must be) and the resource's as
/// given.
#[test]
fn block_bytes() {
    let module: std::sync::Arc<[u8]> = always_compressed(&mut CCtx::create(), &[&stand_in(REV)], 3, 1).unwrap().into();
    let r = stored_resource(always_compressed(&mut CCtx::create(), &[&b"resource".repeat(40)], 3, 1).unwrap()).unwrap();
    assert_eq!(&r[..], b"resource".repeat(40));
    let mut f = sample();
    f.root = Node::concat([f.root, Node::ModuleBytes(REV, module.clone()), Node::ResourceBytes(r.clone())]);
    let want = [SAMPLE, &module, r.stored_data().unwrap()].concat();
    let rev = |_: &aqr_formula::Apply, i: &[&[u8]]| Ok(i.concat().into_iter().rev().collect());
    assert_eq!(f.eval(&rev).unwrap(), want);
    let c = encode(&f, Some(checksum(&want)), Level::Zstd(3)).unwrap();
    assert_eq!(types(&c), [MODULE + 1, RESOURCE, FORMULA, NON_LEAF_RAW, NON_LEAF_RAW]);
    assert_eq!(decode(&c, &natives()).unwrap(), want);
    // the module's block of other bytes (another module's)
    let mut g = f.clone();
    let other = always_compressed(&mut CCtx::create(), &[&stand_in(CUT)], 3, 1).unwrap();
    let Node::Concat(ns) = &mut g.root else { unreachable!() };
    ns[3] = Node::ModuleBytes(REV, other.into());
    assert!(encode(&g, None, Level::Zstd(3)).is_err());
    // a module taken only so, and a resource not stored
    g.root = Node::concat([Node::slice(0, 0, 2), Node::ModuleBytes(CUT, module.clone())]);
    assert!(encode(&g, None, Level::Zstd(3)).is_err_and(|e| e.to_string().contains("not its own")));
    g.root = Node::concat([Node::slice(0, 0, 2), Node::ResourceBytes(Resource::new(b"x".to_vec()))]);
    assert!(encode(&g, None, Level::Zstd(3)).is_err_and(|e| e.to_string().contains("not stored")));
    assert!(stored_resource(b"not zstd".to_vec()).is_err());
}

/// Level 0 compresses nothing: streams in uncompressed blocks, formula and module blocks of
/// raw Zstandard blocks, of any size.
#[test]
fn stored_level() {
    let c = encode(&sample(), None, Level::STORED).unwrap();
    assert_eq!(decode(&c, &natives()).unwrap(), SAMPLE);
    let ts = types(&c);
    assert!(ts.contains(&NON_LEAF_RAW) && !ts.iter().any(|&t| [LEAF_ZSTD, NON_LEAF_ZSTD].contains(&(t & !3))));
    let noise = aqr_common::rng::noise(300_000);
    for n in [0, 1, 1000, 1025, 128 << 10, (128 << 10) + 1, 300_000] {
        let e = always_compressed(&mut CCtx::create(), &[&noise[..n]], STORED, 1).unwrap();
        assert_eq!(decoded(&e, true, true).unwrap(), &noise[..n], "{n}");
        assert!(e.len() <= n + 3 * n.div_ceil(128 << 10).max(1) + 16, "{n}");
    }
}

/// The archive of the blocks `add` appends.
fn archive(add: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut blocks = Vec::new();
    add(&mut blocks);
    let mut footer = Vec::new();
    varint::put(&mut footer, blocks.len() as u64);
    varint::put(&mut footer, 0);
    let n = (footer.len() as u16).to_le_bytes();
    [&HEADER_MAGIC[..], &blocks, &FOOTER_MAGIC, &n, &footer, &n].concat()
}

/// An archive of a formula block of `data`, after a `rev` module block and
/// before non-leaf blocks of `streams`.
fn formula(data: &[u8], streams: &[&[u8]]) -> Vec<u8> {
    archive(|out| {
        block(out, MODULE, &always_compressed(&mut CCtx::create(), &[&stand_in(REV)], 3, 1).unwrap());
        formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[data], 3, 1).unwrap());
        streams.iter().for_each(|s| {
            content_block(out, false, &[s], compressed(&mut CCtx::create(), &[s], 3, 1).unwrap().as_deref())
        });
    })
}

/// Block types of archive `c`.
pub(super) fn types(c: &[u8]) -> Vec<u64> {
    let footer = 6 + 2 + u16::from_le_bytes([c[c.len() - 2], c[c.len() - 1]]) as usize + 2;
    let d = &c[HEADER_MAGIC.len()..c.len() - footer];
    let (mut at, mut out) = (0, Vec::new());
    while at < d.len() {
        let start = at;
        let ty = varint::get(d, &mut at).unwrap();
        let w = 1 << (ty % 4);
        at += w + u64::from_le_bytes(std::array::from_fn(|i| if i < w { d[at + i] } else { 0 })) as usize;
        at += (at - start) % 2;
        out.push(ty);
    }
    out
}

#[test]
fn roundtrip() {
    let f = sample();
    assert_eq!(
        f.eval(&|_: &aqr_formula::Apply, i: &[&[u8]]| Ok(i.concat().into_iter().rev().collect())).unwrap(),
        SAMPLE
    );
    for sum in [None, Some(checksum(SAMPLE))] {
        let c = encode(&f, sum, Level::Zstd(3)).unwrap();
        assert_eq!(c[..6], HEADER_MAGIC);
        assert_eq!(types(&c), [MODULE + 1, FORMULA, NON_LEAF_RAW, NON_LEAF_RAW]);
        assert_eq!(decode(&c, &natives()).unwrap(), SAMPLE);
    }
    // Streams without slices have no blocks.
    let mut f = sample();
    f.streams.insert(
        0,
        Stream { class: "unused".into(), kind: Kind::Unknown, content: false, apart: false, data: vec![1; 100].into() },
    );
    f.root.slices_mut(&mut |s, _, _| *s += 1);
    let c = encode(&f, None, Level::Zstd(3)).unwrap();
    assert_eq!(types(&c), [MODULE + 1, FORMULA, NON_LEAF_RAW, NON_LEAF_RAW]);
    assert_eq!(decode(&c, &natives()).unwrap(), SAMPLE);
    // A module named twice has one block.
    let mut f = sample();
    let Node::Concat(ns) = &mut f.root else { unreachable!() };
    ns.push(ns[1].clone());
    let c = encode(&f, None, Level::Zstd(3)).unwrap();
    assert_eq!(types(&c), [MODULE + 1, FORMULA, NON_LEAF_RAW, NON_LEAF_RAW]);
    assert_eq!(decode(&c, &natives()).unwrap(), [SAMPLE, b"zyxolleh"].concat());
}

/// Each resource has a block, before the formula block, however many applications take it.
#[test]
fn resources() {
    const PRE: Module = Module::new("pre", 1);
    // `pre` puts its resources before its inputs.
    let pre = |a: &aqr_formula::Apply, i: &[&[u8]]| {
        assert_eq!(a.module, PRE);
        Ok([a.resources.iter().map(|r| &r[..]).collect::<Vec<_>>().concat(), i.concat()].concat())
    };
    let (r1, r2) = (Resource::new(vec![b'a'; 1000]), Resource::new(b"0123456789".repeat(10)));
    let mut f = Formula::default();
    let x = f.push("x", b"xyz".to_vec());
    f.root = Node::concat([
        Node::apply_with(PRE, vec![r1.clone(), r2.clone()], vec![], vec![x.clone()], 1103),
        Node::apply_with(PRE, vec![r2.clone()], vec![], vec![x.clone()], 103),
        Node::apply_with(PRE, vec![r1.clone()], vec![], vec![x.clone()], 1003),
    ]);
    let expect = [&r1[..], &r2, b"xyz", &r2, b"xyz", &r1, b"xyz"].concat();
    assert_eq!(f.eval(&pre).unwrap(), expect);
    let c = encode(&f, None, Level::MID).unwrap();
    assert_eq!(types(&c), [MODULE + 1, RESOURCE, RESOURCE, FORMULA, NON_LEAF_RAW]);
    assert_eq!(decode(&c, &super::natives(&[PRE], &pre)).unwrap(), expect);
    // A resource block is always compressed, even when that makes it longer.
    let noise = Resource::new(aqr_common::rng::noise(1000));
    f.root = Node::apply_with(PRE, vec![noise.clone()], vec![], vec![x], 1003);
    let c = encode(&f, None, Level::MID).unwrap();
    assert_eq!(types(&c), [MODULE + 1, RESOURCE + 1, FORMULA, NON_LEAF_RAW]);
    assert_eq!(decode(&c, &super::natives(&[PRE], &pre)).unwrap(), [&noise[..], b"xyz"].concat());
    // sized without the resources held, but archived with them
    let enc = Encoder::new(Level::MID);
    let n = enc.len(&f).unwrap();
    enc.hold_resource(&r1);
    assert_eq!(enc.len(&f).unwrap(), n);
    enc.hold_resource(&noise);
    assert_eq!(enc.len(&f).unwrap(), n - enc.resources_len(&f).unwrap());
    assert_eq!(enc.encode(&f, None).unwrap(), c);
}

/// A formula is sized as it is archived, its streams of a class merged, not each compressed
/// apart (the entries of a ZIP archive alone were sized so far beyond what they took that
/// its formula was turned down).
#[test]
fn sized_merged() {
    let mut f = Formula::default();
    let parts: Vec<Node> = (0..300)
        .map(|i| f.push("x", format!("entry {i}: the same few words once more\n").repeat(20).into_bytes()))
        .collect();
    f.root = Node::concat(parts);
    let mut m = f.clone();
    m.merge_classes();
    assert_eq!(m.streams.len(), 1);
    let enc = Encoder::new(Level::MID);
    assert_eq!(enc.len(&f).unwrap(), enc.len(&m).unwrap());
}

/// Zstandard blocks longer than their data declare by how much (`0` then the excess), and as
/// long as their data by `0 0`.
#[test]
fn longer_than_data() {
    let d = aqr_common::rng::noise(50);
    // a raw zstd block, the last: 3 bytes more
    let header = ((d.len() << 3) | 1) as u32;
    let external = |size: &[u8]| [size, &[0], &header.to_le_bytes()[..3], &d, &checksum(&d).to_le_bytes()].concat();
    assert_eq!(decoded(&external(&[0, 3]), true, true).unwrap(), d);
    assert_eq!(decoded(&external(&[0, 2]), true, true), invalid("zstd data of another size than declared"));
    assert_eq!(decoded(&external(&[0, 54]), true, true), invalid("bad size"));
    // as encoded
    let c = always_compressed(&mut CCtx::create(), &[&d], 3, 1).unwrap();
    assert_eq!(c[..2], [0, 3]);
    assert_eq!(decoded(&c, true, true).unwrap(), d);
    assert_eq!(compressed(&mut CCtx::create(), &[&d], 3, 1).unwrap(), None);
    // and an empty one
    let c = always_compressed(&mut CCtx::create(), &[&[]], 3, 1).unwrap();
    assert_eq!(decoded(&c, true, true).unwrap(), b"");
}

/// A formula of a single slice is a leaf block, compressed if that is smaller.
#[test]
fn leaf() {
    for (d, ty) in [(vec![], LEAF_RAW), (b"hello".to_vec(), LEAF_RAW), (vec![7; 100_000], LEAF_ZSTD)] {
        let c = encode(&Formula::raw("a", d.clone()), None, Level::Zstd(3)).unwrap();
        assert_eq!(types(&c), [ty]);
        assert_eq!(decode(&c, &NoModules).unwrap(), d);
    }
    let mut f = Formula::raw("a", b"0123456789".to_vec());
    f.root = Node::slice(0, 3, 4);
    assert_eq!(decode(&encode(&f, None, Level::Zstd(3)).unwrap(), &NoModules).unwrap(), b"3456");
    // The leaf block's checksum is the output's: one given must be it.
    assert!(encode(&f, Some(checksum(b"3456")), Level::Zstd(3)).is_ok());
    assert!(encode(&f, Some(checksum(b"0123")), Level::Zstd(3)).is_err());
    // An empty formula is a formula block.
    let c = encode(&Formula::default(), None, Level::Zstd(3)).unwrap();
    assert_eq!(types(&c), [FORMULA]);
    assert_eq!(decode(&c, &NoModules).unwrap(), b"");
}

/// A leaf data block may go without a checksum; a non-leaf one may not. A reserved
/// compression method is unsupported.
#[test]
fn data_block_types() {
    let d = vec![7; 1000];
    let c = compressed(&mut CCtx::create(), &[&d], 3, 1).unwrap().unwrap();
    let unchecked = &c[..c.len() - 8];
    assert_eq!(decode(&archive(|out| block(out, LEAF_RAW & !4, &d)), &NoModules).unwrap(), d);
    assert_eq!(decode(&archive(|out| block(out, LEAF_ZSTD & !4, unchecked)), &NoModules).unwrap(), d);
    for ty in [0x38, 0x3c, 0x4c] {
        assert!(
            matches!(decode(&archive(|out| block(out, ty, &c)), &NoModules), Err(Failure::Unsupported(_))),
            "{ty:#x}"
        );
    }
}

/// Neither the padding nor the space of a spacer block is read. A block type is the shortest
/// LEB128; an undefined one is ignored if non-critical (from A to F in its leading digit, padded to
/// 2 hexadecimal digits), else unsupported, even with an invalid block before it.
#[test]
fn block_framing() {
    let decode = |blocks: &[u8]| decode(&archive(|out| out.extend_from_slice(blocks)), &NoModules);
    assert_eq!(decode(b"\x20\x05hello\x00"), Ok(b"hello".to_vec()));
    assert_eq!(decode(b"\x20\x05hello\xaa"), Ok(b"hello".to_vec()));
    assert_eq!(decode(b"\x00\x02\x01\x02\x20\x05hello\x00"), Ok(b"hello".to_vec()));
    assert!(matches!(decode(b"\xa0\x00\x05hello"), Err(Failure::Invalid(_))));
    // non-critical: 0xa0, 0xf0, 0xf00
    assert_eq!(decode(b"\x20\x05hello\x00\xa0\x01\x01\x00"), Ok(b"hello".to_vec()));
    assert_eq!(decode(b"\xf0\x01\x00\x00"), Ok(Vec::new()));
    assert_eq!(decode(b"\x80\x1e\x00\x00"), Ok(Vec::new()));
    // critical: 0x78, 0x80, and 0x0c and 0x0f, whose leading digit is 0 when padded to 2 digits
    assert!(matches!(decode(b"\x78\x00"), Err(Failure::Unsupported(_))));
    assert!(matches!(decode(b"\x80\x01\x00\x00"), Err(Failure::Unsupported(_))));
    assert!(matches!(decode(b"\x0c\x00"), Err(Failure::Unsupported(_))));
    assert!(matches!(decode(&[&[0x0f][..], &[0; 8], &[0]].concat()), Err(Failure::Unsupported(_))));
    // a block-like structure (the header) as a block
    assert!(matches!(decode(b"\xf8\x04\x00\x00"), Err(Failure::Invalid(_))));
    let bad_sum = [&b"\x24\x0dhello"[..], &[0; 8], b"\x00"].concat();
    assert!(matches!(decode(&bad_sum), Err(Failure::Invalid(_))));
    assert!(matches!(decode(&[&bad_sum[..], b"\x78\x00"].concat()), Err(Failure::Unsupported(_))));
}

/// The types of block-like structures, as the spec gives them, begin their magics and are
/// invalid as blocks (not unsupported).
#[test]
fn block_like_types() {
    let enc = |ty: u64| {
        let mut v = Vec::new();
        varint::put(&mut v, ty);
        v
    };
    assert!(HEADER_MAGIC.starts_with(&enc(0x278)));
    assert!([0x69, 0x0d, 0x15, 0xaf, 0xf6, 0xb2].starts_with(&enc(0x69)));
    assert!(FOOTER_MAGIC.starts_with(&enc(0x90_5bcb_957d)));
    assert!([0xfc, 0x0f, 0x6e, 0x7b, 0x4b, 0xac].starts_with(&enc(0x7fc)));
    assert!(envelope::ENCRYPTED_HEADER_MAGIC.starts_with(&enc(0x155_04dc_a579)));
    for ty in [0x278, 0x69, 0x90_5bcb_957d, 0x7fc, 0x155_04dc_a579] {
        let mut b = enc(ty);
        b.resize(b.len() + (1 << (ty % 4)), 0);
        b.resize(b.len().next_multiple_of(2), 0);
        let r = decode(&archive(|out| out.extend_from_slice(&b)), &NoModules);
        assert!(matches!(r, Err(Failure::Invalid(_))), "{ty:#x}: {r:?}");
    }
}

/// A stream that does not compress alone compresses after its kin where they hold it but for
/// some bytes (its kin last next to it, after others), not after data unlike it, nor with
/// no kin.
#[test]
fn compresses_after_kin() {
    let a = aqr_common::rng::Rng::new(5).bytes(300_000);
    let mut b = a.clone();
    b.iter_mut().step_by(24).for_each(|x| *x ^= 0x5a);
    let (c, d) = (aqr_common::rng::Rng::new(6).bytes(300_000), aqr_common::rng::Rng::new(7).bytes(2_000_000));
    let enc = Encoder::new(Level::MID);
    assert_eq!(enc.compresses_each(&[&a, &b, &c]).unwrap(), [false, false, false]);
    let after = enc.compresses_after_each(&[&a, &b, &c, &d], &[vec![], vec![0, 3, 2], vec![3, 0], vec![]]).unwrap();
    assert_eq!(after, [false, true, false, false]);
}

/// A stream of [`Kind::Compressed`] is stored as it is where the probe level compresses
/// all of it by less than 1/32, at any level, leaf or not; else it is compressed at the
/// probe level, and apart from the same bytes of another kind.
#[test]
fn compressed_kind_light() {
    let noise = aqr_common::rng::noise(100_000);
    let same = vec![7; 100_000];
    let types = |c: &[u8]| types(c).into_iter().map(|t| t & !3).collect::<Vec<_>>();
    for level in [Level::MID, Level::Zstd(3)] {
        for (d, ty, non_leaf) in [(&noise, LEAF_RAW, NON_LEAF_RAW), (&same, LEAF_ZSTD, NON_LEAF_ZSTD)] {
            let mut f = Formula::default();
            f.root = f.push_as("a", Kind::Compressed, d.clone());
            let c = encode(&f, None, level).unwrap();
            assert_eq!(types(&c), [ty], "{level:?}");
            assert_eq!(&decode(&c, &NoModules).unwrap(), d);
            let b = f.push("b", d.clone());
            f.root = Node::concat([f.root.clone(), b]);
            let c = encode(&f, None, level).unwrap();
            assert_eq!(types(&c), [FORMULA, non_leaf, non_leaf]);
            assert_eq!(decode(&c, &NoModules).unwrap(), d.repeat(2));
        }
        // at the probe level, not the encoder's
        let mut f = Formula::default();
        f.root = f.push_as("a", Kind::Compressed, same.clone());
        let at_probe = compressed(&mut CCtx::create(), &[&same], PROBE_LEVEL.get(), 1).unwrap().unwrap();
        assert!(encode(&f, None, level).unwrap().windows(at_probe.len()).any(|w| w == at_probe));
    }
    let mut f = Formula::default();
    f.root = f.push_as("a", Kind::Compressed, same.clone());
    assert_eq!(types(&encode(&f, None, Level::STORED).unwrap()), [LEAF_RAW]);
}

/// Data that a sample does not compress is compressed all the same where it repeats from
/// afar, as the probe level over all of it finds (a video of a still picture, its
/// keyframes the same): the same noise twice, 20 MiB apart, the sample's chunks not over
/// the same bytes; of any kind.
#[test]
fn sample_missed() {
    let noise = aqr_common::rng::noise(20 << 20);
    let d = noise.repeat(2);
    for kind in [Kind::Unknown, Kind::Compressed] {
        let mut f = Formula::default();
        f.root = f.push_as("a", kind, d.clone());
        let c = encode(&f, None, Level::MID).unwrap();
        assert!(c.len() < noise.len() + (1 << 20), "{kind}: {}", c.len());
        assert!(decode(&c, &NoModules).unwrap() == d);
    }
}

/// [`Level::MID`] leaves uncompressed what the lowest level hardly shrinks, and compresses
/// the rest at 3, or higher where that saves enough for its time.
#[test]
fn mid_level() {
    let types_at = |d: &[u8], level| {
        let c = encode(&Formula::raw("a", d.to_vec()), None, level).unwrap();
        assert_eq!(decode(&c, &NoModules).unwrap(), d);
        (types(&c)[0] & !3, c.len())
    };
    // shrunk at level 1, by less than 1/32 (but more than zstd keeps blocks raw below)
    let mut noise = aqr_common::rng::noise(2 << 20);
    noise.iter_mut().enumerate().filter(|(i, _)| i % 1024 < 32).for_each(|(_, b)| *b = 0);
    // a short stream, a whole one, and a sample
    for d in [&noise[..30000], &noise[..300_000], &noise] {
        assert_eq!(types_at(d, Level::Zstd(3)).0, LEAF_ZSTD, "{}", d.len());
        assert_eq!(types_at(d, Level::MID).0, LEAF_RAW, "{}", d.len());
    }
    // Text, which level 4 shrinks more than enough for its time.
    let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/index.bs")).unwrap();
    let (zstd3, mid) = (types_at(&text, Level::Zstd(3)).1, types_at(&text, Level::MID).1);
    assert!(mid < zstd3 - zstd3 / 32, "{mid} vs {zstd3}");
    // short, at level 3
    let short = Formula::raw("a", text[..30000].to_vec());
    assert_eq!(encode(&short, None, Level::MID).unwrap(), encode(&short, None, Level::Zstd(3)).unwrap());
    // and so are module and formula blocks
    assert_eq!(encode(&sample(), None, Level::MID).unwrap(), encode(&sample(), None, Level::Zstd(3)).unwrap());
}

/// Levels by name and number; the zstd levels picked from grow with the level, and only
/// from 8 on are dense streams (text, PE code) at the dense level (one no longer than a sample
/// where that is smaller).
#[test]
fn levels() {
    for (s, l) in
        [("low", Level::LOW), ("mid", Level::Of(5)), ("high", Level::Of(8)), ("1", Level::Of(1)), ("0", Level::STORED)]
    {
        assert_eq!(s.parse(), Ok(l));
    }
    assert_eq!("zstd19".parse(), Ok(Level::Zstd(19)));
    for s in ["10", "-1", "zstd", "max"] {
        assert!(s.parse::<Level>().is_err(), "{s}");
    }
    for n in 2..=Level::MAX {
        let (a, b) = (Pick::of(n - 1), Pick::of(n));
        assert!(b.higher.len() >= a.higher.len() && b.rate <= a.rate && b.top() >= a.top(), "{n}");
    }
    assert_eq!(Pick::of(1).top(), 1);

    let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/index.bs")).unwrap();
    let mut long = Vec::new();
    // (lines shuffled, so that it is not one text repeated)
    let mut rng = aqr_common::rng::Rng::new(1);
    let lines: Vec<&[u8]> = text.split_inclusive(|&b| b == b'\n').collect();
    while long.len() <= super::SAMPLE.get() * 2 {
        long.extend_from_slice(lines[rng.below(lines.len() as u64) as usize]);
    }
    let s = |d: &[u8], kind, class: &'static str| Stream {
        class: class.into(),
        kind,
        content: false,
        apart: false,
        data: d.to_vec().into(),
    };
    for (level, dense) in [(Level::MID, false), (Level::Of(7), false), (Level::HIGH, true), (Level::Of(9), true)] {
        let enc = Encoder::new(level);
        let d = How::Dense(DENSE_LEVEL.get());
        assert_eq!(enc.how(&s(&long, Kind::Text, "raw")) == d, dense, "{level:?}");
        assert_eq!(enc.how(&s(&long, Kind::Unknown, "x86/OP")) == d, dense, "{level:?}");
        let t = How::Dense(TOGETHER_LEVEL.get());
        assert_eq!(enc.how(&s(&long, Kind::Unknown, TOGETHER)) == t, dense, "{level:?}");
        assert_eq!(enc.how(&s(&long, Kind::Unknown, "raw")), How::Usual);
        assert_eq!(enc.how(&s(&long, Kind::Compressed, "raw")), How::Light);
        let at = |d: &[u8], how| match enc.plan(key(&[d]), &[d], how).unwrap() {
            Plan::Level(l, _) => l,
            Plan::Done(..) => 0,
            Plan::Probe => PROBE_LEVEL.get(),
        };
        assert_eq!(at(&long, d) == DENSE_LEVEL.get(), dense, "{level:?}");
        assert_eq!(at(&long, t) == TOGETHER_LEVEL.get(), dense, "{level:?}");
        // (at 9, picked from as any other)
        assert_eq!(at(&long, How::Usual) == DENSE_LEVEL.get(), level == Level::Of(9), "{level:?}");
        // no longer than a sample: compressed whole, at the dense level where smaller so
        let Plan::Done(Some(c), _) = enc.plan(key(&[&text]), &[&text], d).unwrap() else {
            panic!("{level:?}: not compressed whole")
        };
        let at_dense = compressed(&mut enc.cctx(), &[&text], DENSE_LEVEL.get(), 1).unwrap().unwrap();
        assert!(at_dense.len() < c.len() || dense, "{level:?}: {} vs {}", at_dense.len(), c.len());
        assert_eq!(c == at_dense, dense, "{level:?}");
    }
    // archived dense, and smaller so
    let f = |kind| {
        let mut f = Formula::raw("a", long.clone());
        f.streams[0].kind = kind;
        f
    };
    let (usual, dense) =
        (encode(&f(Kind::Unknown), None, Level::HIGH).unwrap(), encode(&f(Kind::Text), None, Level::HIGH).unwrap());
    assert_eq!(decode(&dense, &NoModules).unwrap(), long);
    assert!(dense.len() < usual.len(), "{} vs {}", dense.len(), usual.len());
}

/// A stream larger than Zstandard's jobs is archived the same on any number of threads.
#[test]
fn threads() {
    let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/index.bs")).unwrap();
    let mut d = Vec::new();
    let mut rng = aqr_common::rng::Rng::new(1);
    while d.len() < 40 << 20 {
        let at = rng.below(text.len() as u64 - 4096) as usize;
        d.extend_from_slice(&text[at..at + 4096]);
    }
    let f = Formula::raw("a", d.clone());
    for level in [Level::MID, Level::Zstd(3)] {
        let one = Encoder::new(level).encode(&f, None).unwrap();
        assert_eq!(Encoder::with_threads(level, 4).encode(&f, None).unwrap(), one);
        assert_eq!(decode(&one, &NoModules).unwrap(), d);
    }
}

/// A long stream is matched from afar: the same noise twice, 20 MiB apart, takes about
/// as much as once.
#[test]
fn long_distance() {
    let noise = aqr_common::rng::noise(20 << 20);
    let d = noise.repeat(2);
    let c = encode(&Formula::raw("a", d.clone()), None, Level::Zstd(3)).unwrap();
    assert!(c.len() < noise.len() + (1 << 20), "{}", c.len());
    assert!(decode(&c, &NoModules).unwrap() == d);
}

/// A compressed block decodes into as much memory as it declares (and a byte to tell a
/// longer one), not what growing as it comes would take.
#[test]
fn decoded_at_size() {
    let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/index.bs")).unwrap();
    let d = text.repeat((3 << 20) / text.len() + 1);
    let c = compressed(&mut CCtx::create(), &[&d], 3, 1).unwrap().unwrap();
    let out = decoded(&c, true, true).unwrap();
    assert_eq!(out, d);
    assert!(out.capacity() <= d.len() + 1, "{} for {}", out.capacity(), d.len());
}

/// A context compressed with before compresses as a new one does, whatever the levels,
/// threads and data it had (more than a job among them).
#[test]
fn cctx_reused() {
    let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/index.bs")).unwrap();
    let big = text.repeat(ZSTD_JOB.get() / text.len() + 2);
    let runs: [(&[u8], i32, usize); 7] = [
        (&big, 9, 4),
        (&text[..1000], 1, 1),
        (&text, 19, 1),
        (&big, 3, 1),
        (&text[5000..], 6, 4),
        (&[], 3, 2),
        (&text, 3, 1),
    ];
    let mut c = CCtx::create();
    for (d, level, threads) in runs {
        let fresh = compressed(&mut CCtx::create(), &[d], level, threads).unwrap();
        assert_eq!(compressed(&mut c, &[d], level, threads).unwrap(), fresh, "level {level}, {threads} threads");
    }
}

/// Data of any size compresses the same where it is told of ([`Swept`]), given little room
/// as well as plenty: on the calling thread (up to 512 KiB, where Zstandard does not take
/// workers) the output depends on the room it has.
#[test]
fn swept_alike() {
    let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/index.bs")).unwrap();
    let mut rng = aqr_common::rng::Rng::new(1);
    let mut d = Vec::new();
    // (text and noise, in runs of either)
    while d.len() < 3 << 20 {
        let at = rng.below(text.len() as u64 - 65536) as usize;
        let run = 1 << (8 + rng.below(9));
        match rng.below(2) {
            0 => d.extend_from_slice(&text[at..at + run]),
            _ => d.extend((0..run).map(|_| rng.below(256) as u8)),
        }
    }
    let file = vec![0u8; 1000];
    let _watch = aqr_common::progress::watch(&file, |_| {});
    for n in [50_000, 410_649, 600_000, 3 << 20] {
        for (level, threads) in [(1, 1), (9, 1), (3, 4)] {
            for size in [usize::MAX, n / 8] {
                let one = compressed_to(&mut CCtx::create(), &[&d[..n]], level, threads, size).unwrap();
                let swept = Swept::over(aqr_common::progress::Sweep::new(&file, n as u64));
                let two = compressed_to(&mut CCtx::create(), &[&d[..n]], level, threads, size).unwrap();
                drop(swept);
                assert!(one == two, "{n} bytes at level {level}, {threads} threads, room for {size}");
            }
        }
    }
}

/// A stream compresses the same where it is told of ([`Swept`]), a window of output at a
/// time: all of it told, in parts.
#[test]
fn swept() {
    let mut rng = aqr_common::rng::Rng::new(1);
    let d: Vec<u8> =
        (0..2 * ZSTD_JOB.get() + 12345).map(|i| if i % 3 == 0 { rng.below(256) as u8 } else { b'a' }).collect();
    let job = ZSTD_JOB.get();
    for pieces in [vec![&d[..]], vec![&d[..job], &d[job..]], vec![&d[..5], &d[5..job + 1], &d[job + 1..]]] {
        let one = compressed(&mut CCtx::create(), &pieces, 3, 4).unwrap();
        let file = vec![0u8; 1000];
        let told = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let t = told.clone();
        let _watch = aqr_common::progress::watch(&file, move |r| t.lock().unwrap().push(r));
        let swept = Swept::over(aqr_common::progress::Sweep::new(&file, d.len() as u64));
        assert_eq!(compressed(&mut CCtx::create(), &pieces, 3, 4).unwrap(), one);
        drop(swept);
        let told = told.lock().unwrap();
        assert!(told.len() > 2 && told.windows(2).all(|w| w[0].end == w[1].start), "{told:?}");
        assert_eq!((told[0].start, told.last().unwrap().end), (0, 1000));
    }
}

/// A stream in pieces compresses as it does in one, the pieces ending anywhere (at the end
/// of a job among them), and is sized as it is archived.
#[test]
fn pieces() {
    let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/index.bs")).unwrap();
    let mut d = Vec::new();
    let mut rng = aqr_common::rng::Rng::new(1);
    while d.len() < 2 * ZSTD_JOB.get() {
        let at = rng.below(text.len() as u64 - 4096) as usize;
        d.extend_from_slice(&text[at..at + 4096]);
    }
    d.truncate(2 * ZSTD_JOB.get());
    let job = ZSTD_JOB.get();
    let one = compressed(&mut CCtx::create(), &[&d], 1, 4).unwrap();
    // as Zstandard compresses it in one go (long data from afar)
    assert!(d.len() > LONG.get().unwrap());
    let mut z = zstd::bulk::Compressor::new(1).unwrap();
    for p in [
        CParameter::ContentSizeFlag(false),
        CParameter::ChecksumFlag(false),
        CParameter::DictIdFlag(false),
        CParameter::NbWorkers(4),
        CParameter::JobSize(ZSTD_JOB.get() as u32),
        CParameter::OverlapSizeLog(ZSTD_OVERLAP.get()),
        CParameter::WindowLog(25),
        CParameter::EnableLongDistanceMatching(true),
    ] {
        z.set_parameter(p).unwrap();
    }
    assert!(one.as_ref().unwrap().ends_with(&[&z.compress(&d).unwrap()[6..], &checksum(&d).to_le_bytes()].concat()));
    for cuts in [vec![job], vec![1, job - 1, job + 1, d.len()], (1..40).map(|i| i * d.len() / 40).collect()] {
        let (mut pieces, mut at) = (Vec::new(), 0);
        for c in cuts {
            pieces.push(&d[at..c]);
            at = c;
        }
        pieces.push(&d[at..]);
        assert_eq!(compressed(&mut CCtx::create(), &pieces, 1, 4).unwrap(), one);
    }

    let d = &d[..3 << 20];
    let mut f = Formula::default();
    f.push("a", b"head".to_vec());
    f.push("b", Vec::new());
    f.root = Node::concat([Node::slice(1, 0, d.len() as u64), Node::slice(0, 0, 4)]);
    let whole = Formula {
        streams: vec![
            f.streams[0].clone(),
            Stream { class: "b".into(), kind: Kind::Unknown, content: false, apart: false, data: d.to_vec().into() },
        ],
        ..f.clone()
    };
    let pieces = [&d[..100], &d[100..2 << 20], &d[2 << 20..]];
    let raw = [b"head".as_slice(), d].concat();
    for level in [Level::MID, Level::Zstd(3)] {
        let enc = Encoder::new(level);
        assert_eq!(enc.len_with(&f, Some((1, &pieces))).unwrap(), Encoder::new(level).len(&whole).unwrap());
        assert_eq!(enc.smaller_with(&f, 1, &pieces, &raw).unwrap(), Encoder::new(level).smaller(&whole, &raw).unwrap());
        // the stream compressed then is the one archived (its blocks, and the head)
        enc.forget(&raw);
        assert_eq!(enc.encode(&whole, None).unwrap(), Encoder::new(level).encode(&whole, None).unwrap());
        assert_eq!(enc.streams.borrow().len(), 1 + blocks_in(d.len()) as usize);
    }
}

/// Raw data that a formula was found smaller than, then forgotten, is not compressed again
/// to be compared with another formula of it (a scan's around the file), and is compared
/// as a new encoder would.
#[test]
fn smaller_after_forget() {
    let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/index.bs")).unwrap();
    let mut rng = aqr_common::rng::Rng::new(2);
    // within a sample and beyond one, alike throughout (so that the sample's estimate holds)
    for n in [200 << 10, 3 << 20] {
        let mut d = Vec::new();
        while d.len() < n {
            let at = rng.below(text.len() as u64 - 64) as usize;
            d.extend_from_slice(&text[at..at + 64]);
            d.extend_from_slice(&rng.below(1 << 32).to_le_bytes()[..4]);
        }
        // a formula of most of it, then one a little larger
        let part = d[..d.len() * 9 / 10].to_vec();
        let mut f = Formula::default();
        f.push("a", part.clone());
        f.root = Node::slice(0, 0, part.len() as u64);
        let g = Formula { root: Node::concat([f.root.clone(), Node::slice(0, 0, 0)]), ..f.clone() };
        for level in [Level::MID, Level::Zstd(3)] {
            let enc = Encoder::new(level);
            assert!(enc.smaller(&f, &d).unwrap());
            enc.forget(&d);
            let before = COMPRESSED.with(|c| c.get());
            let smaller = enc.smaller(&g, &d).unwrap();
            assert_eq!(COMPRESSED.with(|c| c.get()), before, "{n} bytes at {level:?}");
            assert_eq!(smaller, Encoder::new(level).smaller(&g, &d).unwrap());
            enc.clear();
            enc.smaller(&g, &d).unwrap();
            assert!(COMPRESSED.with(|c| c.get()) > before, "cleared");
        }
    }
}

/// [`Level::MID`] compares a formula with raw data compressed at the base level, however
/// higher the data would be archived at, and keeps only its length: what is compared is
/// mostly not archived.
#[test]
fn smaller_at_base() {
    let text = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/index.bs")).unwrap();
    // alike throughout, so that the sample's estimate holds
    let mut rng = aqr_common::rng::Rng::new(3);
    let mut d = Vec::new();
    while d.len() < 3 << 20 {
        let at = rng.below(text.len() as u64 - 64) as usize;
        d.extend_from_slice(&text[at..at + 64]);
        d.extend_from_slice(&rng.below(1 << 32).to_le_bytes()[..4]);
    }
    let part = d[..d.len() * 9 / 10].to_vec();
    let mut f = Formula::default();
    f.push("a", part.clone());
    f.root = Node::slice(0, 0, part.len() as u64);
    let enc = Encoder::new(Level::MID);
    let k = key(&[&d]);
    assert!(matches!(enc.plan(k, &[&d], How::Usual).unwrap(), Plan::Level(l, _) if l > BASE_LEVEL.get()));
    let raw = Encoder::new(Level::Zstd(BASE_LEVEL.get())).raw_len(&d).unwrap();
    assert_eq!(enc.smaller(&f, &d).unwrap(), enc.len(&f).unwrap() < raw);
    assert!(!enc.streams.borrow().contains_key(&k));
    assert_eq!(enc.raw_lens.borrow().get(&k), Some(&raw));
}

/// Above the base by the time to compress (from level 6 on, by the sample alone); below
/// where clearly smaller, as a sample may be off by a little.
#[test]
fn picks() {
    let mid_pick = |n, base, higher: &[usize], lower: &[usize]| pick(&Pick::of(5), n, base, higher, lower);
    // nothing better
    assert_eq!(mid_pick(1_000_000, 300_000, &[300_000; 3], &[310_000]), 3);
    // level 4, 1% smaller for time worth 0.02% of the data
    assert_eq!(mid_pick(1_000_000, 300_000, &[297_000, 297_000, 297_000], &[310_000]), 4);
    // level 6, 1% smaller than 4 for 0.41%: not enough
    assert_eq!(mid_pick(1_000_000, 300_000, &[297_000, 294_000, 294_000], &[310_000]), 4);
    // level 9, 12% smaller than 4 for 0.70%
    assert_eq!(mid_pick(1_000_000, 300_000, &[297_000, 294_000, 260_000], &[310_000]), 9);
    // level 9, 3% smaller for 0.71% (most of the solid blocks of 10gb save more)
    assert_eq!(mid_pick(1_000_000, 300_000, &[300_000, 300_000, 291_000], &[310_000]), 9);
    // level 1, by 0.7% (as of a sample of 19tech_4.psd, 8.7% larger whole) and by 10%
    assert_eq!(mid_pick(1_000_000, 300_000, &[300_000; 3], &[298_000]), 3);
    assert_eq!(mid_pick(1_000_000, 300_000, &[300_000; 3], &[270_000]), 1);
    // from 6 on, the smallest
    let high = |higher: &[usize], lower: &[usize]| pick(&Pick::of(6), 1_000_000, 300_000, higher, lower);
    assert_eq!(high(&[297_000, 294_000, 294_000], &[310_000]), 6);
    assert_eq!(high(&[300_000, 300_000, 299_900], &[310_000]), 9);
    assert_eq!(high(&[300_000; 3], &[310_000]), 3);
    // low, none higher
    assert_eq!(pick(&Pick::of(2), 1_000_000, 300_000, &[], &[310_000]), 3);
}

#[test]
fn stand_in_names() {
    assert_eq!(stand_in_name(&stand_in(REV)), Some("rev@1"));
    // after another custom section
    let other = [&WASM_HEADER[..], &[0, 3, 0, 9, 9], &stand_in(REV)[WASM_HEADER.len()..]].concat();
    assert_eq!(stand_in_name(&other), Some("rev@1"));
    assert_eq!(stand_in_name(&WASM_HEADER), None);
    assert_eq!(stand_in_name(&stand_in(REV)[..20]), None);
}

/// A dictionary or module block of a 1-byte size is a shorthand block: one in full of fewer
/// bytes is written with a 2-byte size.
#[test]
fn shorthand_types() {
    let small = always_compressed(&mut CCtx::create(), &[&stand_in(REV)], 3, 1).unwrap();
    assert!(small.len() < 0x100);
    for ty in [MODULE, DICTIONARY_RAW] {
        let mut out = Vec::new();
        block(&mut out, ty, &small);
        assert_eq!(out[0] as u64, ty + 1);
    }
    let shorthand = |ty: u8| archive(|out| out.extend([&[ty, 32][..], &[7; 32]].concat()));
    // a module shorthand block is parsed (resolved only when applied, which this one is not)
    assert_eq!(decode(&shorthand(0x60), &natives()), Ok(Vec::new()));
    assert_eq!(decode(&shorthand(0x50), &natives()), resource("registered dictionaries are not supported yet"));
}

/// ModuleBytes and ResourceBytes instructions push the data of the blocks as stored, of the
/// size they tell.
#[test]
fn stored_bytes() {
    let module = always_compressed(&mut CCtx::create(), &[&stand_in(REV)], 3, 1).unwrap();
    let resource = always_compressed(&mut CCtx::create(), &[&[b'r'; 300]], 3, 1).unwrap();
    let of = |insns: &[u8]| {
        let d = [&[0, 1, 0, 1, 0][..], insns].concat();
        archive(|out| {
            block(out, MODULE, &module);
            block(out, RESOURCE, &resource);
            formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[&d], 3, 1).unwrap());
        })
    };
    let (m, r) = (module.len() as u8, resource.len() as u8);
    let c = of(&[RESOURCE_BYTES, 0, r, MODULE_BYTES, 0, m, CONCAT, 2]);
    assert_eq!(decode(&c, &natives()).unwrap(), [&resource[..], &module].concat());
    let c = of(&[RESOURCE_BYTES, 0, r, MODULE_BYTES, 0, m + 1, CONCAT, 2]);
    assert_eq!(decode(&c, &natives()), invalid(format!("a block of {m} bytes, not {}", m + 1)));
    let c = of(&[RESOURCE_BYTES, 0, r - 1, MODULE_BYTES, 0, m, CONCAT, 2]);
    assert_eq!(decode(&c, &natives()), invalid(format!("a block of {r} bytes, not {}", r - 1)));
    // each listed block used
    assert_eq!(decode(&of(&[MODULE_BYTES, 0, m]), &natives()), invalid("a block listed but not used"));
    assert_eq!(decode(&of(&[RESOURCE_BYTES, 1, r]), &natives()), invalid("index 1 out of range"));
}

#[test]
fn instructions_by_hand() {
    // literals swapped, and a slice of the second non-leaf block
    let d = [&[1, 0][..], &[0, 0], &[LITERAL, 2], b"ab", &[LITERAL, 2], b"cd", &[SWAP, 1, SLICE, 0, 1, 2, CONCAT, 3]]
        .concat();
    assert_eq!(
        decode(&formula(&d, &[b"xyz", b"0123"]), &natives()),
        invalid("a non-leaf block no formula block references")
    );
    let d = [&[1, 1][..], &[0, 0], &[LITERAL, 2], b"ab", &[LITERAL, 2], b"cd", &[SWAP, 1, SLICE, 0, 1, 2, CONCAT, 3]]
        .concat();
    assert_eq!(
        decode(&formula(&d, &[b"xyz", b"0123"]), &natives()),
        invalid("a non-leaf block no formula block references")
    );
    let d = [
        &[2, 0, 0][..],
        &[0, 0],
        &[LITERAL, 2],
        b"ab",
        &[LITERAL, 2],
        b"cd",
        &[SWAP, 1, SLICE, 1, 1, 2, SLICE, 0, 0, 1, CONCAT, 4],
    ]
    .concat();
    assert_eq!(decode(&formula(&d, &[b"xyz", b"0123"]), &natives()).unwrap(), b"cdab12x");
}

#[test]
fn pack() {
    let mut f = sample();
    let c = f.push("c", b"0123".to_vec());
    let Node::Concat(ns) = &mut f.root else { unreachable!() };
    let rev = ns.remove(1);
    let packed = Node::pack([Node::slice(0, 1, 2), Node::slice(2, 0, 0), rev, c]);
    f.root = Node::apply(REV, vec![1, 2, 3], vec![Node::pack([Node::slice(1, 0, 1)]), packed.clone()], 71);
    let want = [&packed::pack(&[b"x"])[..], &packed::pack(&[&b"el"[..], b"", b"zyxolleh", b"0123"])]
        .concat()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    assert_eq!((packed.len(), want.len()), (40 + 14, 71));
    assert_eq!(
        f.eval(&|_: &aqr_formula::Apply, i: &[&[u8]]| Ok(i.concat().into_iter().rev().collect())).unwrap(),
        want
    );
    let c = encode(&f, Some(checksum(&want)), Level::Zstd(3)).unwrap();
    assert_eq!(decode(&c, &natives()).unwrap(), want);
}

/// Split instructions by hand: of a literal and of a slice, and their sizes.
#[test]
fn split() {
    let split = |sizes: &[u8]| {
        let d = [
            &[1, 0, 0, 0, LITERAL, 4][..],
            b"abcd",
            &[SPLIT, 2, 1, 3, SWAP, 1, SLICE, 0, 0, 4, SPLIT, sizes.len() as u8],
            sizes,
            &[SWAP, 1, CONCAT, sizes.len() as u8 + 2],
        ]
        .concat();
        decode(&formula(&d, &[b"0123"]), &natives())
    };
    assert_eq!(split(&[2, 0, 2]).unwrap(), b"bcda0123");
    assert_eq!(split(&[3, 1]).unwrap(), b"bcda3012");
    assert_eq!(split(&[3, 0]), invalid("a split into sizes not adding up"));
    assert_eq!(split(&[3, 2]), invalid("a split into sizes not adding up"));
    assert_eq!(split(&[]), invalid("a split into no items"));
}

/// A formula block declares how many outputs it has, as many as its instructions leave and
/// at least one; an archive without a metadata block is its outputs one after another.
#[test]
fn outputs() {
    // "abcd" split into "a" and "bcd"
    let split = [0, 0, 0, LITERAL, 4, b'a', b'b', b'c', b'd', SPLIT, 2, 1, 3];
    let of = |n: u64| {
        archive(|out| formula_block(out, n, &always_compressed(&mut CCtx::create(), &[&split], 3, 1).unwrap()))
    };
    assert_eq!(decode(&of(2), &NoModules).unwrap(), b"abcd");
    for n in [0, 1, 3] {
        assert!(matches!(decode(&of(n), &NoModules), Err(Failure::Invalid(_))), "{n} outputs");
    }
}

/// A non-leaf block that formula blocks share is there for each of them, as long as any is
/// to slice it.
#[test]
fn shared_blocks() {
    let mut apply = vec![APPLY, 0, 0, 3, 1, 2, 3];
    varint::put(&mut apply, MEMORY_LIMIT);
    apply.extend_from_slice(&[0, 2, 1, 4]);
    let first = [&[1, 0, 0, 1, 0, SLICE, 0, 0, 2, SLICE, 0, 1, 2][..], &apply, &[SLICE, 0, 2, 1, CONCAT, 2]].concat();
    let second = [1, 0, 0, 0, SLICE, 0, 0, 3];
    let c = archive(|out| {
        block(out, MODULE, &always_compressed(&mut CCtx::create(), &[&stand_in(REV)], 3, 1).unwrap());
        for d in [&first[..], &second] {
            formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[d], 3, 1).unwrap());
        }
        content_block(out, false, &[b"xyz"], None);
    });
    assert_eq!(decode(&c, &natives()).unwrap(), b"zyyxzxyz");
}

/// A non-leaf block that formula blocks share is there for each of them as their outputs
/// are hashed too ([`Archive::hashed`]), where one cuts its slice by a Split: the slice cut
/// is let go of as its pieces, not once more as itself.
#[test]
fn shared_blocks_hashed_past_a_split() {
    struct Untold;
    impl crate::chash::Records for Untold {
        fn outputs(&self, _: u128) -> Option<Vec<crate::chash::Ch>> {
            None
        }
    }
    let first = [1, 0, 0, 0, SLICE, 0, 0, 3, SPLIT, 2, 1, 2, CONCAT, 2];
    let second = [1, 0, 0, 0, SLICE, 0, 0, 3];
    let c = archive(|out| {
        for d in [&first[..], &second, &second] {
            formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[d], 3, 1).unwrap());
        }
        content_block(out, false, &[b"xyz"], None);
    });
    assert_eq!(decode(&c, &NoModules).unwrap(), b"xyzxyzxyz");
    let hashed = open(&c, &NoModules).unwrap().hashed(&Untold, &|_| None, 1).unwrap();
    let hashes: Vec<_> = hashed.expect("hashes of slices alone").into_iter().map(|h| h.0).collect();
    assert_eq!(hashes, [crate::chash::Ch::of(b"xyz"); 3]);
    // (the block stored as it is hashed in place, its checksum checked all the same)
    let mut bad = c.clone();
    let at = bad.windows(3).position(|w| w == b"xyz").unwrap();
    bad[at] = b'w';
    assert!(open(&bad, &NoModules).unwrap().hashed(&Untold, &|_| None, 1).is_err());
}

/// What the verification learns: applications' outputs by their keys.
#[derive(Default)]
struct Learned(Mutex<HashMap<u128, Vec<crate::chash::Ch>>>);

impl crate::chash::Records for Learned {
    fn outputs(&self, key: u128) -> Option<Vec<crate::chash::Ch>> {
        self.0.lock().unwrap().get(&key).cloned()
    }

    fn learn(&self, key: u128, outputs: &[crate::chash::Ch]) {
        self.0.lock().unwrap().insert(key, outputs.to_vec());
    }
}

/// A formula of applications, a Split of one, packed items, concatenations and a patch, and the
/// bytes it makes.
fn mixed() -> (Formula, Vec<u8>) {
    let noise = aqr_common::rng::noise(10_000);
    let mut target = noise[..6000].to_vec();
    target[100..200].fill(1);
    let mut f = Formula::default();
    f.push("a", noise.clone());
    f.push("b", b"0123456789abcdef".repeat(20));
    let patch = f.push_as("d", Kind::Compressed, aqr_formula::patch::make(&noise[..6000], &target, 3).unwrap());
    let rev = |s: usize, start: u64, len: u64| Node::apply(REV, vec![1, 2, 3], vec![Node::slice(s, start, len)], len);
    let [r0, r1] = <[Node; 2]>::try_from(f.split_node(rev(0, 500, 10), vec![4, 6])).unwrap();
    f.root = Node::concat([
        rev(0, 0, 300),
        Node::pack([Node::slice(1, 0, 7), rev(1, 3, 100)]),
        r1,
        Node::patch(Node::slice(0, 0, 6000), patch, 6000),
        r0,
        rev(0, 4000, 700),
    ]);
    let want = f.eval(&|_: &aqr_formula::Apply, i: &[&[u8]]| Ok(i.concat().into_iter().rev().collect())).unwrap();
    (f, want)
}

/// The hash of an output made of applications, a Split of one, packed items, concatenations and a
/// patch, worked out as evaluated (the values made ahead on threads, as they are on one), is that
/// of the bytes decoded; what it learns of the applications tells the same.
#[test]
fn hashed_evaluating_ahead() {
    use crate::chash::Ch;
    let (f, want) = mixed();
    let c = encode(&f, Some(checksum(&want)), Level::Zstd(3)).unwrap();
    assert!(decode(&c, &natives()).unwrap() == want);
    let identify = |w: &[u8]| Some(format!("{:?}", Ch::of(w)));
    let hash = |records: &Learned, runner: &dyn crate::Runner, threads| {
        let hashed = open(&c, runner).unwrap().hashed(records, &identify, threads).unwrap();
        let hashed = hashed.expect("hashes told");
        assert_eq!(hashed.len(), 1);
        hashed[0].0
    };
    let serial = Learned::default();
    assert_eq!(hash(&serial, &natives(), 1), Ch::of(&want));
    assert!(!serial.0.lock().unwrap().is_empty());
    let ahead = Learned::default();
    assert_eq!(hash(&ahead, &natives(), 4), Ch::of(&want));
    assert_eq!(*serial.0.lock().unwrap(), *ahead.0.lock().unwrap());
    // (told by the records now)
    assert_eq!(hash(&ahead, &natives(), 4), Ch::of(&want));
    assert_eq!(hash(&ahead, &natives(), 1), Ch::of(&want));
}

/// An archive of no tree is listed as one file ([`Archive::listing`]): its leaves' outputs
/// concatenated (equal ones as a count, no more than a few kinds told), or the one output of a
/// formula of all kinds of instructions.
#[test]
fn listed_without_a_tree() {
    let (f, want) = mixed();
    let c = encode(&f, Some(checksum(&want)), Level::Zstd(3)).unwrap();
    let names = |w: &[u8]| stand_in_name(w).map(String::from);
    let l = open(&c, &natives()).unwrap().listing(&names).unwrap();
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].size, want.len() as u64);
    assert!(l[0].formula.starts_with("cat("), "{}", l[0].formula);
    assert!(l[0].formula.contains("rev"), "{}", l[0].formula);
    assert!(l[0].nodes > 8 && l[0].data > 0 && l[0].overhead > 0, "{l:?}");

    // leaves of several kinds, the first three equal, then more than are told
    let slice = [1, 0, 0, 0, SLICE, 0, 0, 3];
    let leaves: [&[u8]; 7] = [
        &slice,
        &slice,
        &slice,
        &[0, 0, 0, LITERAL, 2, b'a', b'b'],
        &[1, 0, 0, 0, SLICE, 0, 0, 1, SLICE, 0, 1, 1, CONCAT, 2],
        &[1, 0, 0, 0, SLICE, 0, 0, 1, SLICE, 0, 1, 1, PACK, 2],
        &[1, 0, 0, 0, SLICE, 0, 0, 3, SPLIT, 2, 1, 2, CONCAT, 2],
    ];
    let c = archive(|out| {
        for d in leaves {
            formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[d], 3, 1).unwrap());
        }
        content_block(out, false, &[b"xyzabcdef"], None);
    });
    let got = decode(&c, &NoModules).unwrap();
    let l = open(&c, &NoModules).unwrap().listing(&names).unwrap();
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].size, got.len() as u64);
    let shown = &l[0].formula;
    assert!(shown.starts_with("cat(slice x3, lit, cat(slice x2), pack(slice x2)"), "{shown}");
    assert!(shown.ends_with(", ...)"), "{shown}");
}

/// The evaluation tells how far it is: nothing at first, then each block decoded and each
/// leaf evaluated, up to all the data blocks' bytes and all the outputs'.
#[test]
fn progress() {
    let mut apply = vec![APPLY, 0, 0, 3, 1, 2, 3];
    varint::put(&mut apply, MEMORY_LIMIT);
    apply.extend_from_slice(&[0, 2, 1, 4]);
    let first = [&[1, 0, 0, 1, 0, SLICE, 0, 0, 2, SLICE, 0, 1, 2][..], &apply, &[SLICE, 0, 2, 1, CONCAT, 2]].concat();
    let second = [1, 0, 0, 0, SLICE, 0, 0, 3];
    let c = archive(|out| {
        block(out, MODULE, &always_compressed(&mut CCtx::create(), &[&stand_in(REV)], 3, 1).unwrap());
        for _ in 0..3 {
            for d in [&first[..], &second] {
                formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[d], 3, 1).unwrap());
            }
            content_block(out, false, &[b"xyz"], None);
        }
        content_block(out, true, &[b"uv"], None);
    });
    let natives = natives();
    let mut a = open(&c, &natives).unwrap();
    let told = Arc::new(Mutex::new(Vec::new()));
    let t = told.clone();
    a.on_progress(Some(Arc::new(move |r| t.lock().unwrap().push(r))));
    assert_eq!(a.value(2).unwrap(), Value::File([&b"zyyxzxyz".repeat(3)[..], b"uv"].concat()));
    let told = told.lock().unwrap();
    // (the blocks' data with its checksum, and the outputs)
    let totals = (3 * (3 + 8) + 2 + 8, 3 * 8 + 2);
    // each leaf started and ended once, and told of between
    let (mut started, mut ended) = (vec![0; 7], vec![0; 7]);
    for s in told.iter() {
        match *s {
            Step::Started(i) => started[i] += 1,
            Step::Ended(i) => {
                assert_eq!(started[i], 1, "{i} ended before it started");
                ended[i] += 1;
            }
            _ => {}
        }
    }
    assert_eq!((started, ended), (vec![1; 7], vec![1; 7]));
    let told: Vec<Reached> =
        told.iter().filter_map(|s| if let Step::Reached(r) = s { Some(*r) } else { None }).collect();
    // (at first, each non-leaf block, each formula leaf, the stored leaf decoded and evaluated)
    assert_eq!(told.len(), 1 + 3 + 6 + 2);
    assert_eq!(told[0], Reached { decompressed: (0, totals.0), evaluated: (0, totals.1) });
    let most = told.iter().fold((0, 0), |m, r| (m.0.max(r.decompressed.0), m.1.max(r.evaluated.0)));
    assert_eq!(most, totals);
    assert!(told.iter().all(|r| (r.decompressed.1, r.evaluated.1) == totals));
}

/// Leaves evaluated within a budget, even of one at a time, give what they give without.
#[test]
fn budget() {
    let mut apply = vec![APPLY, 0, 0, 3, 1, 2, 3];
    varint::put(&mut apply, MEMORY_LIMIT);
    apply.extend_from_slice(&[0, 2, 1, 4]);
    let first = [&[1, 0, 0, 1, 0, SLICE, 0, 0, 2, SLICE, 0, 1, 2][..], &apply, &[SLICE, 0, 2, 1, CONCAT, 2]].concat();
    let second = [1, 0, 0, 0, SLICE, 0, 0, 3];
    let c = archive(|out| {
        block(out, MODULE, &always_compressed(&mut CCtx::create(), &[&stand_in(REV)], 3, 1).unwrap());
        for _ in 0..20 {
            for d in [&first[..], &second] {
                formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[d], 3, 1).unwrap());
            }
            content_block(out, false, &[b"xyz"], None);
        }
    });
    let natives = natives();
    let each = |budget: Option<u64>| {
        let budget = budget.map(Budget::new);
        let mut a = open(&c, &natives).unwrap();
        if let Some(b) = &budget {
            a.set_budget(b);
        }
        let got = Mutex::new(Vec::new());
        a.leaves(4, |i, o| {
            got.lock().unwrap().push((i, o));
            Ok(())
        })
        .unwrap();
        let mut got = got.into_inner().unwrap();
        got.sort();
        got
    };
    let all = each(None);
    assert_eq!(all.len(), 40);
    for b in [1, 6, 1000] {
        assert_eq!(each(Some(b)), all, "{b}");
    }
}

/// With the budget held elsewhere (another archive's leaves, say), the leaves go one at a
/// time, but they go.
#[test]
fn budget_held() {
    let leaf = [1, 0, 0, 0, SLICE, 0, 0, 3];
    let c = archive(|out| {
        for _ in 0..20 {
            formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[&leaf], 3, 1).unwrap());
        }
        content_block(out, false, &[b"xyz"], None);
    });
    let natives = natives();
    let budget = Budget::new(1000);
    let held = budget.take(1000);
    let mut a = open(&c, &natives).unwrap();
    a.set_budget(&budget);
    let (on, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (o, m) = (on.clone(), most.clone());
    a.on_progress(Some(Arc::new(move |s| match s {
        Step::Started(_) => _ = m.fetch_max(o.fetch_add(1, Relaxed) + 1, Relaxed),
        Step::Ended(_) => _ = o.fetch_sub(1, Relaxed),
        _ => {}
    })));
    let n = AtomicUsize::new(0);
    a.leaves(4, |_, o| {
        assert_eq!(o, vec![b"xyz".to_vec()]);
        n.fetch_add(1, Relaxed);
        Ok(())
    })
    .unwrap();
    assert_eq!((n.into_inner(), most.load(Relaxed)), (20, 1));
    drop(held);
    assert_eq!(budget.held(), 0);
}

/// `rev`, whose application of parameters 9 waits for the last leaf to start.
struct WaitsForLast(Arc<(Mutex<bool>, std::sync::Condvar)>);

impl crate::Modules for WaitsForLast {
    fn apply(&self, a: &aqr_formula::Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        if a.params == [9] {
            let (started, changed) = &*self.0;
            let s = started.lock().unwrap();
            let (s, _) = changed.wait_timeout_while(s, std::time::Duration::from_secs(5), |s| !*s).unwrap();
            if !*s {
                return Err(Error::new("the last leaf not let in"));
            }
        }
        Ok(inputs.concat().into_iter().rev().collect())
    }
}

/// Within a budget, the leaves after one waiting for room that fit go before it, and the
/// memory limit of a module of tasks is weighed as the instance's: in each archive, the first leaf waits for the last to start.
#[test]
fn budget_passed() {
    // `rev` of `params`, declaring `limit`; and a leaf of no application
    let leaf = |params: u8, limit: u64, classes: u8| {
        let mut d = vec![1, 0, 0, 1, 0, SLICE, 0, 0, 2, SLICE, 0, 1, 2, APPLY, 0, 0, 1, params];
        varint::put(&mut d, limit);
        d.push(classes);
        (0..classes).for_each(|_| varint::put(&mut d, MEMORY_LIMIT));
        d.extend_from_slice(&[2, 1, 4, SLICE, 0, 2, 1, CONCAT, 2]);
        d
    };
    let plain = vec![1, 0, 0, 0, SLICE, 0, 0, 3];
    let run = |module: &[u8], leaves: &[Vec<u8>]| {
        let c = archive(|out| {
            block(out, MODULE, &always_compressed(&mut CCtx::create(), &[module], 3, 1).unwrap());
            for d in leaves {
                formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[d], 3, 1).unwrap());
            }
            content_block(out, false, &[b"xyz"], None);
        });
        let started = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let waits = WaitsForLast(started.clone());
        let natives = super::natives(&[REV], &waits);
        let budget = Budget::new(2000);
        let mut a = open(&c, &natives).unwrap();
        a.set_budget(&budget);
        let last = leaves.len() - 1;
        a.on_progress(Some(Arc::new(move |s| {
            if matches!(s, Step::Started(i) if i == last) {
                *started.0.lock().unwrap() = true;
                started.1.notify_all();
            }
        })));
        a.leaves(4, |_, _| Ok(()))
    };
    // the second and the third wait for the first to end, and more leaves than `eval:passing` go
    // before them
    let mut leaves = vec![leaf(9, 1500, 0), leaf(1, 1800, 0), leaf(1, 1800, 0)];
    leaves.extend(std::iter::repeat_n(plain.clone(), 2 * PASSING.get()));
    assert_eq!(run(&stand_in(REV), &leaves), Ok(()));
    // a module of a class of 8 task slots: its instance of 1500 bytes (the tasks' have limits of
    // their own)
    let mut slotted = stand_in(REV);
    let name = b"aqr_task_slots";
    slotted.extend_from_slice(&[0, name.len() as u8 + 2, name.len() as u8]);
    slotted.extend_from_slice(name);
    slotted.push(8);
    assert_eq!(run(&slotted, &[leaf(9, 1500, 1), plain]), Ok(()));
}

#[test]
fn decode_errors() {
    let c = encode(&sample(), Some(checksum(SAMPLE)), Level::Zstd(3)).unwrap();
    // A changed byte is an invalid archive, or the same file (e.g. with a larger zstd window).
    let (mut same, mut failed) = (0, 0);
    for i in 0..c.len() {
        let mut bad = c.clone();
        bad[i] ^= 0x41;
        match decode(&bad, &natives()) {
            Ok(d) if d == SAMPLE => same += 1,
            Err(Failure::Invalid(_)) => failed += 1,
            r => panic!("byte {i}: {r:?}"),
        }
    }
    assert!(same < 8 && failed + same == c.len(), "{same} the same");
    for n in 1..c.len() {
        assert!(matches!(decode(&c[..n], &natives()), Err(Failure::Invalid(_))), "{n} bytes");
    }
    assert_eq!(decode(&c, &NoModules), resource("no module runs here"));
    assert_eq!(decode(&c, &super::natives(&[CUT], &RevCut)), resource("no native implementation of rev@1"));
    // the output checksum
    let bad = encode(&sample(), Some(checksum(SAMPLE) ^ 1), Level::Zstd(3)).unwrap();
    assert_eq!(decode(&bad, &natives()), invalid("output checksum mismatch"));
}

/// Lengths a formula adds up past 2^64 (slices of 2^63 bytes concatenated or packed, a slice
/// starting near the end of the numbers split): invalid, not a sum wrapped.
#[test]
fn lengths_past_the_numbers() {
    let big = 1u64 << 63;
    for insn in [CONCAT, PACK] {
        let mut d = vec![1, 0, 0, 0];
        for _ in 0..2 {
            d.extend_from_slice(&[SLICE, 0, 0]);
            varint::put(&mut d, big);
        }
        d.extend_from_slice(&[insn, 2]);
        let r = decode(&formula(&d, &[b"abc"]), &natives());
        assert!(matches!(r, Err(Failure::Invalid(_))), "{insn}: {r:?}");
    }
    let mut d = vec![1, 0, 0, 0, SLICE, 0];
    varint::put(&mut d, u64::MAX - 1);
    d.extend_from_slice(&[3, SPLIT, 2, 1, 2, CONCAT, 2]);
    let r = decode(&formula(&d, &[b"abc"]), &natives());
    assert!(matches!(r, Err(Failure::Invalid(_))), "split: {r:?}");
}

/// Applications by hand: the numbers of inputs and the declared sizes.
#[test]
fn applications() {
    let apply = |inputs: u8, sizes: &[u8]| {
        let mut d = vec![1, 0, 0, 1, 0, SLICE, 0, 0, 3, SLICE, 0, 1, 2, APPLY, 0, 0, 3, 1, 2, 3];
        varint::put(&mut d, MEMORY_LIMIT);
        d.extend_from_slice(&[0, inputs, sizes.len() as u8]);
        d.extend_from_slice(sizes);
        decode(&formula(&d, &[b"abc"]), &natives())
    };
    assert_eq!(apply(2, &[5]).unwrap(), b"cbcba");
    assert_eq!(apply(3, &[5]), invalid("stack underflow"));
    assert_eq!(apply(2, &[]), invalid("an application without outputs"));
    assert_eq!(apply(2, &[4]), invalid("module block 0: outputs of other sizes than declared"));
}

/// The earlier Apply instruction (opcode 11, no task limits, a memory limit of all the
/// application's memories) is valid only for the modules of releases ([`released`]), and
/// their memory limit is not enforced.
#[test]
fn earlier_apply_of_released_modules() {
    let old = |module: &[u8], limit: u64| {
        let mut d = vec![1, 0, 0, 1, 0, SLICE, 0, 0, 3, SLICE, 0, 1, 2, APPLY_OLD, 0, 0, 3, 1, 2, 3];
        varint::put(&mut d, limit);
        d.extend_from_slice(&[2, 1, 5]);
        let c = archive(|out| {
            block(out, MODULE, &always_compressed(&mut CCtx::create(), &[module], 3, 1).unwrap());
            formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[&d], 3, 1).unwrap());
            content_block(out, false, &[b"abc"], compressed(&mut CCtx::create(), &[b"abc"], 3, 1).unwrap().as_deref());
        });
        decode(&c, &natives())
    };
    // a module of the stand-in's kind, told apart by a section of its own
    let mut released_module = stand_in(REV);
    released_module.extend_from_slice(&[0, 3, 1, b'r', 0]);
    let other = stand_in(REV);
    assert_eq!(old(&other, 1), invalid("module block 0: the reserved Apply instruction of a module not released"));
    let sum = sha2::Digest::finalize(sha2::Digest::chain_update(sha2::Sha512_256::default(), &released_module));
    TEST_RELEASED.lock().unwrap().push(sum.iter().map(|b| format!("{b:02x}")).collect());
    // (the limit of 1 byte is not enforced)
    assert_eq!(old(&released_module, 1).unwrap(), b"cbcba");
    assert_eq!(old(&other, 1), invalid("module block 0: the reserved Apply instruction of a module not released"));
    // the real list: hexadecimal hashes, sorted, none repeated
    let list = crate::released::RELEASED;
    assert!(list.len() > 100);
    assert!(list.iter().all(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))));
    assert!(list.windows(2).all(|w| w[0] < w[1]));
}

/// An application of a module of classes of tasks takes a task limit for each: the bound
/// of a formula not yet declared counts them.
#[test]
fn bound_counts_task_limits() {
    let mut f = Formula::raw("a", vec![0; 100]);
    for limits in [vec![], vec![1 << 28, 1 << 28]] {
        f.root = Node::Apply(Box::new(aqr_formula::Apply {
            module: REV,
            resources: vec![],
            params: vec![],
            inputs: vec![Node::slice(0, 0, 100)],
            len: 100,
            memory_limit: aqr_formula::NO_LIMIT,
            task_limits: limits,
        }));
        let ordinals = ordinals_of(&f);
        let mut out = Vec::new();
        Emit {
            ordinals: &ordinals,
            literals: &[],
            modules: &[REV],
            classes: &[2],
            resources: &[],
            out: &mut out,
            stack: vec![],
        }
        .node(&f.root);
        let bound = description_bound(&f.root);
        assert!(bound >= out.len(), "{bound} < {}", out.len());
    }
}

#[test]
fn bound() {
    let desc = |f: &Formula| {
        let ordinals = ordinals_of(f);
        let resources: Vec<&Resource> = f.applies().into_iter().flat_map(|a| &a.resources).collect();
        let mut out = Vec::new();
        Emit {
            ordinals: &ordinals,
            literals: &[],
            modules: &[REV],
            classes: &[],
            resources: &resources,
            out: &mut out,
            stack: vec![],
        }
        .node(&f.root);
        out.len()
    };
    let mut f = sample();
    assert!(description_bound(&f.root) >= desc(&f));
    let Node::Concat(ns) = &mut f.root else { unreachable!() };
    let Node::Apply(a) = &mut ns[1] else { unreachable!() };
    a.resources = vec![Resource::new(vec![1]), Resource::new(vec![2])];
    assert!(description_bound(&f.root) >= desc(&f));
    // Of formulas put together: the parts add up.
    let mut g = Formula::raw("big", vec![0; 1 << 20]);
    let parts: Vec<_> = (0..300).map(|_| (description_bound(&sample().root), g.embed(sample()))).collect();
    let sum: usize = parts.iter().map(|p| p.0).sum();
    g.root = Node::Concat(std::iter::once(g.root.clone()).chain(parts.into_iter().map(|p| p.1)).collect());
    let b = description_bound(&g.root);
    assert_eq!(b, sum + description_bound(&Node::slice(0, 0, 1 << 20)) + 3);
    assert!(b >= desc(&g), "{b} < {}", desc(&g));
}

fn real_module(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../aqr-modules/wasm/{name}.wasm", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// A module block holds a module in the split form where that is smaller, and either form
/// gives the module back.
#[test]
fn module_block_forms() {
    for name in ["wasm-recons", "bcj-x86", "flac-recons"] {
        let wasm = real_module(name);
        let data = module_block_data(&wasm).unwrap();
        let decoded = stored_decoded(&data).unwrap();
        assert_eq!(decoded[0], aqr_common::wasm::SPLIT_FORM, "{name}");
        let stored = always_compressed(&mut CCtx::create(), &[&wasm], MODULE_LEVEL, 1).unwrap();
        assert!(data.len() < stored.len(), "{name}: {} against {}", data.len(), stored.len());
        assert_eq!(module_of_block_data(&data).unwrap(), wasm, "{name}");
        assert_eq!(module_of_block_data(&stored).unwrap(), wasm, "{name}");
    }
    // not splittable: stored
    let stand_in = stand_in(REV);
    assert_eq!(module_of_block_data(&module_block_data(&stand_in).unwrap()).unwrap(), stand_in);
}

/// Split forms that are not valid are not modules.
#[test]
fn invalid_split_forms() {
    let wasm = real_module("bcj-x86");
    let good = aqr_common::wasm::to_split_form(&wasm).unwrap();
    assert_eq!(module_of_decoded(good.clone()).unwrap(), wasm);
    let bad = |d: Vec<u8>| assert!(module_of_decoded(d).is_err());
    bad(Vec::new());
    bad(vec![1, 2, 3]);
    bad(good[..good.len() - 1].to_vec());
    bad([&good[..], &[0]].concat());
    // a count other than 13
    bad([&[12][..], &good[1..]].concat());
    // lengths past the payload
    bad(vec![0x0d; 1]);
    let mut long = good.clone();
    long[1] = 0xff;
    long.insert(2, 0x7f);
    bad(long);
}

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);

/// An archive of two segments: directories, files of extras and in a link group, a link, a
/// journal; then a tombstone, a file and a directory replacing one.
fn segmented_archive() -> Vec<u8> {
    use crate::{ContentAt, Entry, EntryNode, Extra, Leaf, SegmentWriter, Time, Tree};
    let enc = Encoder::new(Level::STORED);
    let mut w = SegmentWriter::new();
    let mut out = SegmentWriter::header().to_vec();
    let segments: [Vec<Entry>; 2] = [
        vec![
            Entry {
                parent: None,
                name: b"d".to_vec(),
                node: EntryNode::Dir,
                extra: Extra { mode: Some(0o755), ..Extra::default() },
            },
            Entry {
                parent: Some(0),
                name: b"a".to_vec(),
                node: EntryNode::File(0),
                extra: Extra { mtime: Some(Time::new(10, 5)), link_group: Some(7), ..Extra::default() },
            },
            Entry {
                parent: Some(0),
                name: b"b".to_vec(),
                node: EntryNode::File(1),
                extra: Extra { link_group: Some(7), uid: Some(1000), ..Extra::default() },
            },
            Entry {
                parent: None,
                name: b"l".to_vec(),
                node: EntryNode::Link(b"d/a".to_vec()),
                extra: Extra::default(),
            },
        ],
        vec![
            Entry { parent: None, name: b"d".to_vec(), node: EntryNode::Dir, extra: Extra::default() },
            Entry::tombstone(Some(0), b"b".to_vec()),
            Entry {
                parent: Some(0),
                name: b"c".to_vec(),
                node: EntryNode::File(0),
                extra: Extra { mtime: Some(Time::new(20, 0)), ..Extra::default() },
            },
        ],
    ];
    for (k, entries) in segments.iter().enumerate() {
        let files: Vec<Vec<u8>> = (0..entries.iter().filter(|e| matches!(e.node, EntryNode::File(_))).count())
            .map(|i| format!("file {k} {i} ").repeat(30).into_bytes())
            .collect();
        let mut f = Formula::default();
        let leaves: Vec<Leaf> =
            files.iter().map(|b| Leaf { outputs: vec![f.push("t", b.clone())], sums: vec![None] }).collect();
        let b = w.batch(&enc, &f, &leaves).unwrap();
        w.commit(&b);
        out.extend_from_slice(b.blocks());
        let t = Tree { entries: entries.clone(), ..Tree::default() };
        let contents: Vec<ContentAt> = (files.iter().enumerate())
            .map(|(j, d)| ContentAt { leaf: b.leaf_at[j], output: 0, size: d.len() as u64 })
            .collect();
        w.set_journal(vec![(1000 * (k as u64 + 1), format!("point {k}"))]).unwrap();
        if k + 1 < segments.len() {
            out.extend_from_slice(&w.close(&enc, &t, &contents).unwrap());
        } else {
            out.extend_from_slice(&w.finish(&enc, &t, &contents).unwrap().0);
        }
    }
    out
}

/// What reading `c` as the archive's openers do ends in: an error or what it holds, never a
/// panic, a hang or an allocation of what its bytes do not hold.
fn read_all(c: &[u8]) {
    let natives = natives();
    let path = vec![vec![b"d".to_vec(), b"c".to_vec()]];
    let paths: [&[Vec<Vec<u8>>]; 2] = [&path, &[]];
    if let Ok(a) = open(c, &natives) {
        let _ = a.value(1);
    }
    let _ = open_upto(c, &natives, c.len());
    let _ = open_salvaging(c, &natives, false);
    if let Ok(a) = open_indexed(Payload::Plain(c), &natives, 1) {
        let _ = a.value(1);
    }
    for p in paths {
        if let Ok(a) = open_indexed_at(Payload::Plain(c), &natives, 1, p) {
            let _ = a.tree().map(|t| t.entries.len());
            let _ = a.contents_len();
        }
    }
}

/// read/1: an archive of several segments, with links, extras, a journal and a tombstone,
/// damaged in every way: every opener ends in an error or an archive.
#[test]
fn segmented_archive_damaged() {
    let c = segmented_archive();
    let n = natives();
    let a = open(&c, &n).unwrap();
    assert_eq!(a.journal().len(), 2);
    let Value::Tree(t, contents) = a.value(1).unwrap() else { panic!("a tree") };
    assert!(t.entries.iter().any(|e| e.name == b"c") && !t.entries.iter().any(|e| e.name == b"b"));
    assert_eq!(contents.len(), 2);
    let b = open_indexed_at(Payload::Plain(&c), &n, 1, &[vec![b"d".to_vec(), b"c".to_vec()]]).unwrap();
    assert!(b.tree().unwrap().entries.iter().any(|e| e.name == b"c"));
    aqr_testkit::damaged(&c, 4, c.len(), std::time::Duration::from_secs(20), read_all);
}

/// An archive of one segment of a directory alone.
fn dir_archive() -> Vec<u8> {
    use crate::{Entry, EntryNode, Extra, SegmentWriter, Tree};
    let enc = Encoder::new(Level::STORED);
    let mut out = SegmentWriter::header().to_vec();
    let t = Tree {
        entries: vec![Entry { parent: None, name: b"d".to_vec(), node: EntryNode::Dir, extra: Extra::default() }],
        ..Tree::default()
    };
    out.extend_from_slice(&SegmentWriter::new().finish(&enc, &t, &[]).unwrap().0);
    out
}

/// `c`, a one-segment archive, with its metadata block's data changed by `f` (stored again,
/// its checksum made) and a footer made again.
fn remeta(c: &[u8], f: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let fields = u16::from_le_bytes([c[c.len() - 2], c[c.len() - 1]]) as usize;
    let d = &c[HEADER_MAGIC.len()..c.len() - 2 - fields - 8];
    let framed = crate::parse::frame(d).unwrap();
    let &(start, ty, data, _) = framed.iter().rev().find(|b| crate::locate::metadata(b.1)).unwrap();
    let mut out = [&HEADER_MAGIC[..], &d[..start]].concat();
    let mut m = decoded(data, ty & 0x04 != 0, true).unwrap();
    f(&mut m);
    let at = out.len();
    block_of(&mut out, 0x10, &[&m, &checksum_of(&[&m]).to_le_bytes()]);
    let end = out.len();
    footer_with(&mut out, end - HEADER_MAGIC.len(), (end + FOOTER_MAGIC.len() + 2 - at) as u64);
    out
}

/// Every opener's verdict on `c`, which `must` not accept: invalid, within a time.
fn all_openers_refuse(c: &[u8], what: &str) {
    let n = natives();
    let p = Payload::Plain(c);
    let results: Vec<(&str, Decoded<()>)> = aqr_testkit::within(std::time::Duration::from_secs(10), || {
        aqr_testkit::case(what);
        vec![
            ("open", open(c, &n).map(drop)),
            ("open_salvaging", open_salvaging(c, &n, false).map(drop)),
            ("open_indexed", open_indexed(p, &n, 1).map(drop)),
            ("open_indexed_at", open_indexed_at(p, &n, 1, &[vec![b"d".to_vec()]]).map(drop)),
        ]
    })
    .unwrap();
    for (name, r) in results {
        // (salvaging may open what is damaged: that it does not panic is what is checked)
        if name != "open_salvaging" {
            assert!(matches!(r, Err(Failure::Invalid(_))), "{what}: {name}: {r:?}");
        }
    }
}

/// read/2: footers and headers failing by hand, of the messages `footer_fields` and the index
/// have, `prev` pointers making loops, a header alone.
#[test]
fn footers_and_headers_fail() {
    let c = dir_archive();
    let n = natives();
    assert!(open(&c, &n).is_ok());
    let msg = |c: &[u8]| match open(c, &n) {
        Err(Failure::Invalid(m)) => m,
        r => panic!("{:?}", r.map(drop)),
    };
    let fields = u16::from_le_bytes([c[c.len() - 2], c[c.len() - 1]]) as usize;
    let end = c.len() - 2 - fields - 8;
    // a header alone, and its footer's two sizes
    assert_eq!(msg(&HEADER_MAGIC), "truncated footer");
    assert_eq!(msg(&[]), "bad header magic");
    assert_eq!(msg(&c[..3]), "bad header magic");
    let mut bad = c.clone();
    bad[0] ^= 1;
    assert_eq!(msg(&bad), "bad header magic");
    let mut bad = c.clone();
    bad[end] ^= 1;
    assert_eq!(msg(&bad), "bad footer");
    // the leading size of the footer's fields not the trailing one
    let mut bad = c.clone();
    bad[end + 6] ^= 1;
    assert_eq!(msg(&bad), "bad footer");
    // the trailing size past the archive, and a footer's fields reaching into the header
    for len in [c.len() as u16, 0xffff, (c.len() - 8) as u16, 1] {
        let mut bad = c.clone();
        bad[end + 6..end + 8].copy_from_slice(&len.to_le_bytes());
        let l = bad.len();
        bad[l - 2..].copy_from_slice(&len.to_le_bytes());
        assert!(matches!(open(&bad, &n), Err(Failure::Invalid(_))), "{len}");
        all_openers_refuse(&bad, &format!("fields of {len}"));
    }
    // a header and a footer, nothing between: no metadata block, one without blocks
    let mut empty = HEADER_MAGIC.to_vec();
    footer_with(&mut empty, 0, 0);
    assert_eq!(open(&empty, &n).map(|a| a.leaves_len()).ok(), Some(0));
    // the size hint: not the blocks' size; the fields with bytes after them
    for hint in [0, 1, (end - HEADER_MAGIC.len() + 1) as u64, u64::MAX] {
        let mut bad = c[..end].to_vec();
        footer_with(&mut bad, hint as usize, (c.len() - 2 - fields - 8 - end).max(1) as u64);
        // (the metadata offset is wrong too: either is invalid)
        all_openers_refuse(&bad, &format!("hint {hint}"));
    }
    let mut more = c[..end].to_vec();
    let mut f = Vec::new();
    varint::put(&mut f, (end - HEADER_MAGIC.len()) as u64);
    varint::put(&mut f, (c.len() - 2 - fields - end) as u64);
    f.push(0);
    more.extend_from_slice(&FOOTER_MAGIC);
    more.extend_from_slice(&(f.len() as u16).to_le_bytes());
    more.extend_from_slice(&f);
    more.extend_from_slice(&(f.len() as u16).to_le_bytes());
    assert_eq!(msg(&more), "bad footer");
    // the metadata offset: 0 (none, so the archive is one file of no blocks), past the start, at
    // the header's end, one byte off
    let meta_back = (c.len() - 2 - fields - end) as u64;
    for back in [meta_back + 1, meta_back - 1, meta_back + 2, 1 << 40, u64::MAX] {
        let mut bad = c[..end].to_vec();
        footer_with(&mut bad, end - HEADER_MAGIC.len(), back);
        all_openers_refuse(&bad, &format!("metadata at {back} back"));
    }
    // a metadata block of a previous pointer of 0 (itself: a loop), one into the middle of the
    // header, one of the block's own place, one past it
    for prev in [0u64, 2, 3, 1 << 40, u64::MAX] {
        let bad = remeta(&c, |m| {
            let mut at = 0;
            leb(m, &mut at).unwrap();
            let mut v = Vec::new();
            varint::put(&mut v, prev);
            m.splice(..at, v);
        });
        all_openers_refuse(&bad, &format!("prev {prev}"));
    }
    // (the original through the same path stays valid)
    assert!(open(&remeta(&c, |_| {}), &n).is_ok());
}

/// read/4: formula data failing by hand, in each way `program::read` checks: what is decoded of
/// an archive of one formula block of `outputs` outputs, a module block of `rev`, a shorthand
/// module block and one non-leaf block `xyz`.
#[test]
fn formula_data_fails() {
    let n = natives();
    let hash = [7u8; 32];
    let run_with = |d: &[u8], outputs: u64, stream: bool| {
        let c = archive(|out| {
            block(out, MODULE, &always_compressed(&mut CCtx::create(), &[&stand_in(REV)], 3, 1).unwrap());
            shorthand_block(out, MODULE, &hash);
            formula_block(out, outputs, &always_compressed(&mut CCtx::create(), &[d], 3, 1).unwrap());
            if stream {
                content_block(
                    out,
                    false,
                    &[b"xyz"],
                    compressed(&mut CCtx::create(), &[b"xyz"], 3, 1).unwrap().as_deref(),
                );
            }
        });
        decode_value(&c, &n).map(|_| ())
    };
    // (the stream is there where the program refers to one)
    let run = |d: &[u8], outputs: u64| run_with(d, outputs, d.first() == Some(&1));
    let bad = |d: &[u8], outputs: u64, want: &str| match run(d, outputs) {
        Err(Failure::Invalid(m)) => assert!(m.contains(want), "{d:?}: {m} for {want}"),
        r => panic!("{d:?}: {r:?} for {want}"),
    };
    // (no blocks, resources or modules listed: [0, 0, 0])
    let lit = [LITERAL, 3, b'a', b'b', b'c'];
    let prog = |tail: &[u8]| [&[0u8, 0, 0][..], tail].concat();
    assert_eq!(run(&prog(&lit), 1), Ok(()));
    bad(&prog(&[0x30]), 1, "reserved opcode 0x30");
    bad(&prog(&[0x04]), 1, "reserved opcode 0x04");
    bad(&prog(&[0xff]), 1, "reserved opcode 0xff");
    // checksums: truncated, of an empty stack, wrong
    bad(&prog(&[&lit[..], &[CHECKSUM, 1, 2, 3, 4, 5, 6, 7]].concat()), 1, "truncated checksum");
    bad(&prog(&[&lit[..], &[CHECKSUM]].concat()), 1, "truncated checksum");
    bad(&prog(&[CHECKSUM, 0, 0, 0, 0, 0, 0, 0, 0]), 1, "a checksum of an empty stack");
    bad(&prog(&lit), 0, "a formula block of no outputs");
    bad(&prog(&[&lit[..], &[CHECKSUM, 0, 0, 0, 0, 0, 0, 0, 0]].concat()), 1, "checksum");
    // outputs counted
    bad(&prog(&lit), 2, "1 outputs, not 2");
    bad(&prog(&[&lit[..], &lit[..]].concat()), 1, "2 outputs, not 1");
    bad(&prog(&[]), 1, "0 outputs, not 1");
    // a stack too shallow
    bad(&prog(&[SWAP, 0]), 1, "out of range");
    bad(&prog(&[&lit[..], &[SWAP, 1]].concat()), 1, "index 1 out of range");
    bad(&prog(&[&lit[..], &[CONCAT, 2]].concat()), 1, "out of range");
    bad(&prog(&[&lit[..], &[PACK, 2]].concat()), 1, "out of range");
    bad(&prog(&[&lit[..], &[PATCH, 3]].concat()), 1, "stack underflow");
    bad(&prog(&[SPLIT, 1, 3]), 1, "stack underflow");
    bad(&prog(&[&lit[..], &[SPLIT, 0]].concat()), 1, "a split into no items");
    // a swap with itself is a no-op, a concatenation of nothing is empty
    assert_eq!(run(&prog(&[&lit[..], &[SWAP, 0]].concat()), 1), Ok(()));
    let c = |tail: &[u8]| {
        let d = prog(tail);
        let c = formula(&d, &[]);
        decode(&c, &n)
    };
    assert_eq!(c(&[CONCAT, 0]).unwrap(), b"");
    assert_eq!(c(&[PACK, 0]).unwrap(), [0; 8]);
    assert_eq!(c(&[&lit[..], &[CONCAT, 0, CONCAT, 2]].concat()).unwrap(), b"abc");
    // references and lists: past the blocks, resources and modules there are; listed and not used
    bad(&[1, 0, 0, 0, SLICE, 1, 0, 1], 1, "out of range");
    bad(&[1, 5, 0, 0, SLICE, 0, 0, 1], 1, "a referenced block that does not exist");
    bad(&[1, 0, 0, 0], 1, "a block listed but not used");
    bad(&[0, 1, 0, 0, SLICE, 0, 0, 1], 1, "out of range");
    bad(&[0, 0, 1, 0, SLICE, 0, 0, 1], 1, "out of range");
    bad(&[0, 0, 1, 7, MODULE_BYTES, 0, 1], 1, "out of range");
    bad(&[0, 1, 0, 0, RESOURCE_BYTES, 0, 1], 1, "out of range");
    bad(&[0, 0, 0, 0, MODULE_BYTES, 0, 1], 1, "out of range");
    // counts past the bytes there are
    bad(&[0xff, 0xff, 0x7f], 1, "count");
    bad(&[0, 0xff, 0x7f], 1, "count");
    bad(&[0, 0, 0xff, 0xff, 0xff, 0xff, 0x0f], 1, "count");
    bad(&prog(&[LITERAL, 9, 1, 2]), 1, "count 9 out of range");
    bad(&prog(&[APPLY, 0]), 1, "out of range");
    bad(&[0, 0, 1, 0, APPLY, 0, 0xff, 0x7f], 1, "count");
    // a module block as stored, and a shorthand one that has no bytes
    let stored = always_compressed(&mut CCtx::create(), &[&stand_in(REV)], 3, 1).unwrap();
    let mut d = vec![0, 0, 1, 0, MODULE_BYTES, 0];
    varint::put(&mut d, stored.len() as u64);
    assert_eq!(run(&d, 1), Ok(()));
    let mut d = vec![0, 0, 1, 1, MODULE_BYTES, 0];
    varint::put(&mut d, 32);
    bad(&d, 1, "a ModuleBytes instruction of a shorthand module block");
    // (the size of the block as stored told wrong)
    let mut d = vec![0, 0, 1, 0, MODULE_BYTES, 0];
    varint::put(&mut d, stored.len() as u64 + 1);
    bad(&d, 1, "bytes, not");
}

/// read/5: Checksum instructions over intermediate values, several to a value, and over
/// literals, slices and the pieces of a split: a wrong one among right ones fails the formula
/// whichever way it is evaluated.
#[test]
fn intermediate_checksums() {
    let n = natives();
    let sum = |d: &[u8]| {
        let mut v = vec![CHECKSUM];
        v.extend_from_slice(&checksum(d).to_le_bytes());
        v
    };
    let wrong = |d: &[u8]| sum(&[d, b"!"].concat());
    let lit = |d: &[u8]| {
        let mut v = vec![LITERAL, d.len() as u8];
        v.extend_from_slice(d);
        v
    };
    // (of the stream "xyz" where a program slices it: its first instruction)
    let prog = |parts: &[&[u8]]| {
        let head: &[u8] = if parts[0][0] == SLICE { &[1, 0, 0, 0] } else { &[0, 0, 0] };
        [head, &parts.concat()].concat()
    };
    // the stream "xyz" sliced as it is, and the piece of it checksummed
    let slice = [SLICE, 0, 0, 3];
    let run = |d: &[u8]| {
        let c = formula(d, if d[0] == 1 { &[b"xyz"] } else { &[] });
        let r = decode(&c, &n);
        // (evaluated by leaves too, and written out)
        let a = open(&c, &n).unwrap();
        let by_leaves = a.leaves(1, |_, _| Ok(()));
        let mut got = Vec::new();
        let written = a.write_file(1, &mut |b: &[u8]| {
            got.extend_from_slice(b);
            Ok(())
        });
        assert_eq!(by_leaves.is_ok(), r.is_ok(), "{d:?}");
        assert_eq!(written.is_ok(), r.is_ok(), "{d:?}");
        if let Ok(r) = &r {
            assert_eq!(&got, r);
        }
        r
    };
    let (abc, def, xyz) = (&b"abc"[..], &b"def"[..], &b"xyz"[..]);
    // each of the intermediate values right, and the output
    let good = prog(&[&lit(abc), &sum(abc), &lit(def), &sum(def), &[CONCAT, 2], &sum(b"abcdef")]);
    assert_eq!(run(&good).unwrap(), b"abcdef");
    // a wrong one among them, wherever it is
    for (a, b, c) in [(false, true, true), (true, false, true), (true, true, false)] {
        let pick = |ok: bool, d: &[u8]| if ok { sum(d) } else { wrong(d) };
        let p = prog(&[&lit(abc), &pick(a, abc), &lit(def), &pick(b, def), &[CONCAT, 2], &pick(c, b"abcdef")]);
        assert_eq!(run(&p), invalid("output checksum mismatch"), "{a} {b} {c}");
    }
    // two to a value, the second wrong; and both wrong in the same way
    assert_eq!(run(&prog(&[&lit(abc), &sum(abc), &wrong(abc)])), invalid("output checksum mismatch"));
    assert_eq!(run(&prog(&[&lit(abc), &wrong(abc), &sum(abc)])), invalid("output checksum mismatch"));
    assert_eq!(run(&prog(&[&lit(abc), &sum(abc), &sum(abc)])).unwrap(), abc);
    // a slice, and the pieces of a split, each by its own: the one swapped to the top
    let split =
        prog(&[&lit(b"abcdef"), &[SPLIT, 2, 2, 4], &sum(b"cdef"), &[SWAP, 1], &sum(b"ab"), &[SWAP, 1, CONCAT, 2]]);
    assert_eq!(run(&split).unwrap(), b"abcdef");
    let wrong_piece =
        prog(&[&lit(b"abcdef"), &[SPLIT, 2, 2, 4], &sum(b"cdef"), &[SWAP, 1], &sum(b"cd"), &[SWAP, 1, CONCAT, 2]]);
    assert_eq!(run(&wrong_piece), invalid("output checksum mismatch"));
    // (a checksum of the top after a swap is of the item swapped up, not of the one below)
    let swapped = prog(&[&lit(b"abcdef"), &[SPLIT, 2, 2, 4], &[SWAP, 1], &sum(b"cdef"), &[SWAP, 1, CONCAT, 2]]);
    assert_eq!(run(&swapped), invalid("output checksum mismatch"));
    let sliced = prog(&[&slice, &sum(xyz), &lit(abc), &sum(abc), &[CONCAT, 2]]);
    assert_eq!(run(&sliced).unwrap(), b"xyzabc");
    assert_eq!(run(&prog(&[&slice, &wrong(xyz), &lit(abc), &[CONCAT, 2]])), invalid("output checksum mismatch"));
    // the checksum of a Split input, checked before its pieces are taken
    let input = prog(&[&lit(b"abcdef"), &wrong(b"abcdef"), &[SPLIT, 2, 3, 3, CONCAT, 2]]);
    assert_eq!(run(&input), invalid("output checksum mismatch"));
}

/// read/6: sizes a formula declares past what its bytes can hold (a Patch's length, an
/// application's output sizes, a Split's, a Slice's): refused as invalid or as a resource the
/// decoder lacks, with no allocation of them (the testkit's allocator refuses 1 GiB).
#[test]
fn declared_sizes_past_the_bytes() {
    let n = natives();
    let noise = aqr_common::rng::noise(1000);
    let patch = aqr_formula::patch::make(&noise, &noise[..900], 3).unwrap();
    let big = [1u64 << 36, 1 << 38, 1 << 40, 1 << 62, u64::MAX];
    let verdict = |c: &[u8], what: &str| {
        aqr_testkit::within(std::time::Duration::from_secs(20), || {
            aqr_testkit::case(what);
            let r = decode_value(c, &n).map(|_| ());
            // no request for what the formula only declares
            assert!(aqr_testkit::most() < 1 << 30, "{what}: a request of {} bytes", aqr_testkit::most());
            r
        })
        .unwrap()
    };
    for len in big {
        // a Patch of the source "noise" by a patch of 900 bytes, its length declared as `len`
        let mut d = vec![1, 0, 0, 0, SLICE, 0, 0];
        varint::put(&mut d, 1000);
        d.extend_from_slice(&[LITERAL]);
        varint::put(&mut d, patch.len() as u64);
        d.extend_from_slice(&patch);
        d.push(PATCH);
        varint::put(&mut d, len);
        let c = formula(&d, &[&noise]);
        let r = verdict(&c, &format!("a patch of {len} bytes"));
        assert!(matches!(r, Err(Failure::Invalid(_) | Failure::Resource(_))), "patch {len}: {r:?}");
        // an application's outputs of those sizes, and one of them among others
        for sizes in [vec![len], vec![len, 1], vec![1, len], vec![len, len]] {
            let mut d = vec![1, 0, 0, 1, 0, SLICE, 0, 0, 3, SLICE, 0, 1, 2, APPLY, 0, 0, 3, 1, 2, 3];
            varint::put(&mut d, MEMORY_LIMIT);
            d.extend_from_slice(&[0, 2]);
            varint::put(&mut d, sizes.len() as u64);
            sizes.iter().for_each(|&s| varint::put(&mut d, s));
            let c = formula(&d, &[b"abc"]);
            let r = verdict(&c, &format!("an application of outputs {sizes:?}"));
            assert!(matches!(r, Err(Failure::Invalid(_) | Failure::Resource(_))), "apply {sizes:?}: {r:?}");
        }
        // a Split into those sizes, and a Slice of them
        let mut d = vec![1, 0, 0, 0, SLICE, 0, 0, 3, SPLIT, 2];
        varint::put(&mut d, len);
        varint::put(&mut d, 3u64.wrapping_sub(len));
        d.extend_from_slice(&[CONCAT, 2]);
        let r = verdict(&formula(&d, &[b"abc"]), &format!("a split of {len}"));
        assert!(matches!(r, Err(Failure::Invalid(_) | Failure::Resource(_))), "split {len}: {r:?}");
        let mut d = vec![1, 0, 0, 0, SLICE, 0, 0];
        varint::put(&mut d, len);
        let r = verdict(&formula(&d, &[b"abc"]), &format!("a slice of {len}"));
        assert!(matches!(r, Err(Failure::Invalid(_) | Failure::Resource(_))), "slice {len}: {r:?}");
        let mut d = vec![1, 0, 0, 0, SLICE, 0];
        varint::put(&mut d, len);
        d.push(1);
        let r = verdict(&formula(&d, &[b"abc"]), &format!("a slice from {len}"));
        assert!(matches!(r, Err(Failure::Invalid(_) | Failure::Resource(_))), "slice from {len}: {r:?}");
    }
}

/// A metadata block's data as `tree::read` takes it, made by hand: `prev`, the offsets of
/// modules, resources and dictionaries, then entries (directories, files, specials in this order;
/// each its parent's number and name; a file its dictionary, offset, output and size; a special
/// its kind and what follows it) and the optional metadata.
#[derive(Clone, Default)]
struct Meta {
    prev: u64,
    modules: Vec<u64>,
    resources: Vec<u64>,
    dictionaries: Vec<u64>,
    dirs: Vec<(u64, &'static str)>,
    files: Vec<(u64, &'static str, u64, u64, u64, u64)>,
    specials: Vec<(u64, &'static str, u64, Vec<u8>)>,
    opt: Vec<u8>,
}

impl Meta {
    fn bytes(&self) -> Vec<u8> {
        let width = |n: u64| match n {
            0 => 0,
            1..0x100 => 1,
            0x100..0x1_0000 => 2,
            0x1_0000..0x1_0000_0000 => 4,
            _ => 8,
        };
        let put = |out: &mut Vec<u8>, bound: u64, v: u64| out.extend_from_slice(&v.to_le_bytes()[..width(bound)]);
        let mut entries = Vec::new();
        let mut starts = Vec::new();
        for (_, name) in &self.dirs {
            starts.push(entries.len() as u64);
            entries.extend_from_slice(name.as_bytes());
            entries.push(0);
        }
        for (_, name, .., size) in &self.files {
            starts.push(entries.len() as u64);
            entries.extend_from_slice(name.as_bytes());
            entries.push(0);
            varint::put(&mut entries, *size);
        }
        for (_, name, kind, rest) in &self.specials {
            starts.push(entries.len() as u64);
            entries.extend_from_slice(name.as_bytes());
            entries.push(0);
            varint::put(&mut entries, *kind);
            entries.extend_from_slice(rest);
        }
        let mut m = Vec::new();
        varint::put(&mut m, self.prev);
        for c in [
            self.modules.len(),
            self.resources.len(),
            self.dictionaries.len(),
            self.dirs.len(),
            self.files.len(),
            self.specials.len(),
            entries.len(),
            self.opt.len(),
        ] {
            varint::put(&mut m, c as u64);
        }
        for list in [&self.modules, &self.resources, &self.dictionaries] {
            list.iter().for_each(|&o| put(&mut m, self.prev, o));
        }
        let parents = (self.dirs.iter().map(|d| d.0))
            .chain(self.files.iter().map(|f| f.0))
            .chain(self.specials.iter().map(|s| s.0));
        parents.skip(1).for_each(|p| put(&mut m, self.dirs.len() as u64, p));
        for &(_, _, dictionary, offset, output, _) in &self.files {
            put(&mut m, self.dictionaries.len() as u64, dictionary);
            put(&mut m, self.prev, offset + (output > 0) as u64);
            if output > 0 {
                varint::put(&mut m, output - 1);
            }
        }
        starts.iter().skip(1).for_each(|&s| put(&mut m, entries.len() as u64, s));
        m.extend_from_slice(&entries);
        m.extend_from_slice(&self.opt);
        m
    }
}

/// A CBOR array (or map, tag) of `kind` nested `depth` times around a 0.
fn nested(kind: &str, depth: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for _ in 0..depth {
        match kind {
            "array" => v.push(0x81),
            "indefinite" => v.push(0x9f),
            "map" => v.extend_from_slice(&[0xa1, 0x00]),
            "tag" => v.push(0xc1),
            _ => unreachable!(),
        }
    }
    v.push(0x00);
    if kind == "indefinite" {
        v.extend(std::iter::repeat_n(0xff, depth));
    }
    v
}

/// read/7: the metadata block's own structure, malformed by hand: what `tree::read` and
/// `tree_of` refuse, and a CBOR nested past the depth they read.
#[test]
fn malformed_metadata() {
    use crate::tree;
    let read = |m: &Meta| tree::read(&m.bytes()).map(|m| m.entries.len());
    let bad = |m: &Meta, want: &str| match read(m) {
        Err(Failure::Invalid(e)) => assert!(e.contains(want), "{e} for {want}"),
        r => panic!("{r:?} for {want}"),
    };
    let base = Meta {
        prev: 100,
        dirs: vec![(0, "d"), (1, "e")],
        files: vec![(2, "f", 0, 10, 0, 5), (0, "g", 0, 20, 2, 7)],
        specials: vec![(0, "l", 1, b"d/f\0".to_vec()), (1, "t", 0, vec![])],
        ..Meta::default()
    };
    assert_eq!(read(&base), Ok(6));
    let r = tree::read(&base.bytes()).unwrap();
    assert!(matches!(r.entries[3].2, tree::Read::File { offset: 20, output: 2, size: 7, .. }));
    // parents: after its entry, itself, past the directories
    let mut m = base.clone();
    m.dirs[1].0 = 2;
    bad(&m, "a parent after its entry");
    m.dirs[1].0 = 3;
    bad(&m, "a field out of range");
    let mut m = base.clone();
    m.files[0].0 = 3;
    bad(&m, "a field out of range");
    // fields over their bounds: offsets past `prev`, a dictionary of none
    bad(&Meta { modules: vec![101], ..base.clone() }, "a field out of range");
    let mut m = base.clone();
    (m.dictionaries, m.files[0].2) = (vec![5], 2);
    bad(&m, "a field out of range");
    let mut m = base.clone();
    m.files[0].3 = 101;
    bad(&m, "a field out of range");
    // names
    for name in ["", ".", "..", "a/b", "/"] {
        let mut m = base.clone();
        m.dirs[0].1 = Box::leak(name.to_string().into_boxed_str());
        bad(&m, "an invalid entry name");
    }
    // links: no target, a target of no bytes, one unended
    let mut m = base.clone();
    m.specials[0].3 = b"\0".to_vec();
    bad(&m, "an invalid link target");
    m.specials[0].3 = b"abc".to_vec();
    bad(&m, "an unended link target");
    m.specials[0].3 = vec![];
    bad(&m, "an unended link target");
    // bytes after an entry, after the entries, after the optional metadata, truncations
    let mut m = base.clone();
    m.specials[1].3 = vec![9];
    bad(&m, "bytes after an entry");
    let good = base.bytes();
    let mut longer = good.clone();
    longer.push(0);
    assert!(matches!(tree::read(&longer), Err(Failure::Invalid(e)) if e.contains("bytes after")), "trailing");
    for cut in 0..good.len() {
        assert!(matches!(tree::read(&good[..cut]), Err(Failure::Invalid(_))), "cut at {cut}");
    }
    // entry offsets: one moved
    let mut moved = good.clone();
    let at = good.len() - base.opt.len() - (6 + 2) - 1;
    moved[at] ^= 1;
    assert!(tree::read(&moved).is_err());
    // counts past the data
    let mut m = good.clone();
    m[1] = 0xff;
    m.insert(2, 0x7f);
    assert!(matches!(tree::read(&m), Err(Failure::Invalid(e)) if e.contains("count")));

    // CBOR: nested within the limit, and past it (a resource failure), whatever nests
    let with_opt = |opt: Vec<u8>| Meta { opt, ..Meta { prev: 5, dirs: vec![(0, "d")], ..Meta::default() } };
    let unknown = |v: Vec<u8>| [&[0xa1, 0x01][..], &v].concat();
    let in_extras = |v: Vec<u8>| [&[0xa1, 0x21, 0x82, 0xf6, 0xa1, 0x18, 0x63][..], &v].concat();
    for kind in ["array", "indefinite", "map", "tag"] {
        for depth in [0, 10, 50] {
            for opt in [unknown(nested(kind, depth)), in_extras(nested(kind, depth))] {
                let m = with_opt(opt);
                let r = tree::read(&m.bytes());
                assert!(r.is_ok(), "{kind} {depth}: {:?}", r.map(|_| ()));
            }
        }
        for depth in [70, 1000, 100_000] {
            for opt in [unknown(nested(kind, depth)), in_extras(nested(kind, depth))] {
                let m = with_opt(opt);
                let r = tree::read(&m.bytes());
                assert!(matches!(r, Err(Failure::Resource(_))), "{kind} {depth}: {:?}", r.map(|_| ()));
                // (and the extras of no entry wanted, gone over by their heads alone)
                let r = tree::read_with(&m.bytes(), &mut |e| Some(vec![false; e.len() + 1]));
                assert!(matches!(r, Err(Failure::Resource(_))), "{kind} {depth} skipped: {:?}", r.map(|_| ()));
            }
        }
    }
}

/// An archive of one segment of two files, `a` (100 bytes) and `b` (200), each of a leaf block.
fn two_files_archive() -> Vec<u8> {
    use crate::{ContentAt, Entry, EntryNode, Extra, Leaf, SegmentWriter, Tree};
    let enc = Encoder::new(Level::STORED);
    let mut w = SegmentWriter::new();
    let mut out = SegmentWriter::header().to_vec();
    let mut f = Formula::default();
    let leaves: Vec<Leaf> =
        [100, 200].iter().map(|&n| Leaf { outputs: vec![f.push("t", vec![n as u8; n])], sums: vec![None] }).collect();
    let b = w.batch(&enc, &f, &leaves).unwrap();
    w.commit(&b);
    out.extend_from_slice(b.blocks());
    let entries = ["a", "b"]
        .iter()
        .enumerate()
        .map(|(i, name)| Entry {
            parent: None,
            name: name.as_bytes().to_vec(),
            node: EntryNode::File(i),
            extra: Extra::default(),
        })
        .collect();
    let t = Tree { entries, ..Tree::default() };
    let contents: Vec<ContentAt> =
        [100, 200].iter().enumerate().map(|(i, &n)| ContentAt { leaf: b.leaf_at[i], output: 0, size: n }).collect();
    out.extend_from_slice(&w.finish(&enc, &t, &contents).unwrap().0);
    out
}

/// read/7: what `tree_of` refuses of a metadata block that reads, in an archive of its own:
/// two entries of a name, a tombstone of an extra, files of one output of two sizes, of an
/// output its leaf block lacks, and not at a leaf block.
#[test]
fn metadata_not_matching_the_blocks() {
    let n = natives();
    let c = dir_archive();
    let refused = |c: &[u8], want: &str| {
        for r in [
            open(c, &n).map(drop),
            // (the indexed opener reads the leaf blocks as they are taken: its failure may be of
            // reading them)
            open_indexed(Payload::Plain(c), &n, 1).and_then(|a| a.value(1)).map(drop),
            open_salvaging(c, &n, false).map(|_| ()).and(Err(Failure::Invalid("(salvaged)".into()))),
        ] {
            match r {
                Err(Failure::Invalid(e)) if e == "(salvaged)" => {}
                Err(Failure::Invalid(e)) => assert!(e.contains(want), "{e} for {want}"),
                r => panic!("{r:?} for {want}"),
            }
        }
    };
    let dirs = |m: Meta| remeta(&c, |d| *d = Meta { prev: 1, ..m }.bytes());
    refused(&dirs(Meta { dirs: vec![(0, "d"), (0, "d")], ..Meta::default() }), "two entries of a name");
    refused(&dirs(Meta { dirs: vec![(0, "d"), (1, "d"), (1, "d")], ..Meta::default() }), "two entries of a name");
    // (the same name in two directories is not)
    assert!(open(&dirs(Meta { dirs: vec![(0, "d"), (0, "e"), (1, "d"), (2, "d")], ..Meta::default() }), &n).is_ok());
    let tomb = |extra: &[u8]| {
        dirs(Meta {
            dirs: vec![(0, "d")],
            specials: vec![(0, "t", 0, vec![])],
            opt: [&[0xa1, 0x21, 0x83, 0xf6, 0xf6][..], extra].concat(),
            ..Meta::default()
        })
    };
    assert!(open(&tomb(&[0xf6]), &n).is_ok());
    refused(&tomb(&[0xa0]), "a tombstone of an extra");
    // the first segment has to have an entry
    refused(&dirs(Meta::default()), "the first segment of no entries");

    let c = two_files_archive();
    let m = crate::tree::read(&decoded_metadata(&c)).unwrap();
    let crate::tree::Read::File { offset: oa, size: sa, .. } = m.entries[0].2 else { panic!("a file") };
    let crate::tree::Read::File { offset: ob, size: sb, .. } = m.entries[1].2 else { panic!("a file") };
    let files = |files: Vec<(u64, &'static str, u64, u64, u64, u64)>| {
        remeta(&c, |d| *d = Meta { prev: m.prev, files, ..Meta::default() }.bytes())
    };
    assert!(open(&files(vec![(0, "a", 0, oa, 0, sa), (0, "b", 0, ob, 0, sb)]), &n).is_ok());
    // two files of one output, of the one size: one content
    let same = files(vec![(0, "a", 0, oa, 0, sa), (0, "b", 0, oa, 0, sa)]);
    let a = open(&same, &n).unwrap();
    assert_eq!(a.contents_len(), 1);
    refused(&files(vec![(0, "a", 0, oa, 0, sa), (0, "b", 0, oa, 0, sb)]), "of other sizes");
    refused(&files(vec![(0, "a", 0, oa, 0, sa), (0, "b", 0, oa, 1, sa)]), "an output its leaf block does not have");
    refused(&files(vec![(0, "a", 0, oa, 0, sa), (0, "b", 0, oa - 2, 0, sb)]), "not at a leaf block");
    refused(&files(vec![(0, "a", 0, 0, 0, sa), (0, "b", 0, ob, 0, sb)]), "not at a leaf block");
    // a dictionary the metadata tells that no block is of, and a file of it
    let listed = Meta { prev: m.prev, dictionaries: vec![1], files: vec![(0, "a", 1, oa, 0, sa)], ..Meta::default() };
    refused(&remeta(&c, |d| *d = listed.bytes()), "dictionary");
}

/// The data of the last metadata block of archive `c`.
fn decoded_metadata(c: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    remeta(c, |m| data = m.clone());
    data
}

/// read/8: a spacer block of `ty` (0 to 3: a size field of 1, 2, 4 or 8 bytes) of `n` bytes of
/// `fill`, padded to an even length.
fn spacer_of(out: &mut Vec<u8>, ty: u8, n: usize, fill: u8) {
    out.push(ty);
    out.extend_from_slice(&(n as u64).to_le_bytes()[..1 << ty]);
    out.extend(std::iter::repeat_n(fill, n));
    if (1 + (1 << ty) + n) % 2 == 1 {
        out.push(fill);
    }
}

/// read/8: spacer blocks of every size field and size, of any bytes, anywhere among the blocks
/// (the first, between, after the last); cut short or past the archive they are invalid.
#[test]
fn spacer_blocks_of_every_size_field() {
    let n = natives();
    let plain = |out: &mut Vec<u8>| {
        formula_block(out, 1, &always_compressed(&mut CCtx::create(), &[&[0, 0, 0, LITERAL, 1, b'x']], 3, 1).unwrap());
    };
    let want = archive(plain);
    assert_eq!(decode(&want, &n).unwrap(), b"x");
    for ty in 0..=3u8 {
        for len in [0, 1, 2, 3, 254, 255, 256, 257, 4000] {
            for fill in [0, 0xa5] {
                // (a size field of 1 byte holds 255 at most)
                if ty == 0 && len > 255 {
                    continue;
                }
                let c = archive(|out| {
                    spacer_of(out, ty, len, fill);
                    plain(out);
                    spacer_of(out, ty, len, fill);
                });
                assert_eq!(decode(&c, &n).as_deref(), Ok(&b"x"[..]), "type {ty}, {len} bytes of {fill:#x}");
                let c = archive(|out| {
                    plain(out);
                    spacer_of(out, ty, len, fill);
                });
                assert_eq!(decode(&c, &n).as_deref(), Ok(&b"x"[..]), "after: type {ty}, {len} bytes");
            }
        }
    }
    // past the archive: a size field naming more than there is, up to the numbers' end
    for ty in 0..=3u8 {
        for size in [5u64, 1 << 20, 1 << 40, u64::MAX] {
            if size >= 1 << (8 << ty).min(63) && ty < 3 {
                continue;
            }
            let c = archive(|out| {
                out.push(ty);
                out.extend_from_slice(&size.to_le_bytes()[..1 << ty]);
                out.extend_from_slice(&[0; 3]);
                plain(out);
            });
            let r = decode(&c, &n);
            assert!(matches!(r, Err(Failure::Invalid(_))), "type {ty}, size {size}: {r:?}");
        }
    }
    // a spacer block that ends the blocks as it is (even), or short of its padding byte (odd)
    for (tail, ok) in [(&[0u8, 0][..], true), (&[0, 2, 0, 0], true), (&[0, 1, 0], false), (&[0, 2, 0], false)] {
        let c = archive(|out| {
            plain(out);
            out.extend_from_slice(tail);
        });
        let r = decode(&c, &n);
        assert!(
            if ok { r.as_deref() == Ok(&b"x"[..]) } else { matches!(r, Err(Failure::Invalid(_))) },
            "{tail:?}: {r:?}"
        );
    }
}

/// read/8: dictionary references to no dictionary block (before the block, or at all), and the
/// import blocks 58 to 5F, which no reader here takes.
#[test]
fn dictionaries_and_imports_refused() {
    let n = natives();
    let junk = [3u8; 12];
    // a dictionary index in a leaf, a non-leaf and a formula block: the index first
    for ty in [LEAF_ZSTD_DICT, NON_LEAF_ZSTD_DICT, FORMULA + 4] {
        for index in [0u64, 1, 2, 1 << 40, u64::MAX] {
            let mut data = Vec::new();
            if ty == FORMULA + 4 {
                varint::put(&mut data, index);
                data.push(1);
            } else {
                varint::put(&mut data, index);
            }
            data.extend_from_slice(&junk);
            // (a block of no dictionary before it, and of one dictionary before it: index 1 and up)
            let none = archive(|out| block(out, ty, &data));
            let r = decode_value(&none, &n).map(|_| ());
            assert!(matches!(r, Err(Failure::Invalid(_))), "type {ty:#x}, index {index}: {r:?}");
            let raw =
                always_compressed(&mut CCtx::create(), &[&b"a raw dictionary of content".repeat(10)], 3, 1).unwrap();
            let some = archive(|out| {
                block(out, DICTIONARY_RAW + 1, &raw);
                block(out, ty, &data);
            });
            let r = decode_value(&some, &n).map(|_| ());
            assert!(matches!(r, Err(Failure::Invalid(_))), "type {ty:#x}, index {index} of one: {r:?}");
            let r = open(&some, &n).map(|a| a.leaves_len());
            if index >= 1 {
                assert!(matches!(r, Err(Failure::Invalid(_))), "type {ty:#x}, index {index} of one, opened: {r:?}");
            }
        }
        // a dictionary block coming after the block that names it
        let mut data = vec![0];
        data.extend_from_slice(&junk);
        let raw = always_compressed(&mut CCtx::create(), &[&b"content".repeat(10)], 3, 1).unwrap();
        let late = archive(|out| {
            block(out, ty, &data);
            block(out, DICTIONARY_RAW + 1, &raw);
        });
        assert!(matches!(decode_value(&late, &n), Err(Failure::Invalid(_))), "type {ty:#x} late");
    }
    // imports, of any size and content, wherever
    // (the type with a size field of 1 byte, widened by the size of the data)
    for ty in [0x58, 0x5c] {
        for data in [&[][..], &[0u8; 5], &[0xffu8; 300]] {
            let c = archive(|out| {
                block(out, ty, data);
                formula_block(
                    out,
                    1,
                    &always_compressed(&mut CCtx::create(), &[&[0, 0, 0, LITERAL, 0]], 3, 1).unwrap(),
                );
            });
            let r = decode_value(&c, &n).map(|_| ());
            assert!(
                matches!(&r, Err(Failure::Unsupported(e)) if e.contains("undefined critical type")),
                "{ty:#x}: {r:?}"
            );
            for r in [
                open(&c, &n).map(drop),
                open_indexed(Payload::Plain(&c), &n, 1).map(drop),
                open_salvaging(&c, &n, false).map(drop),
            ] {
                assert!(matches!(r, Err(Failure::Unsupported(_) | Failure::Invalid(_)) | Ok(())), "{ty:#x}: {r:?}");
            }
            // (the inspector tells blocks as they are, imports among them, never panicking)
            let _ = crate::inspect::inspect(&c, false, &|_| None, None);
        }
    }
}

/// read/8: a content block after the last metadata block (the inspector counts the imports 58
/// to 5F, the registered dictionary 50 and the others of 20 to 77 among them) is invalid.
#[test]
fn content_after_the_last_metadata_block() {
    let n = natives();
    let c = dir_archive();
    let fields = u16::from_le_bytes([c[c.len() - 2], c[c.len() - 1]]) as usize;
    let end = c.len() - 2 - fields - 8;
    let back = (c.len() - 2 - fields - end) as u64;
    for ty in [0x20, 0x40, 0x50, 0x54, 0x58, 0x5c, 0x60, 0x64, 0x70] {
        let mut v = c[..end].to_vec();
        block(&mut v, ty, &[0u8; 40]);
        let added = (v.len() - end) as u64;
        let hint = v.len() - HEADER_MAGIC.len();
        footer_with(&mut v, hint, back + added);
        let r = open(&v, &n).map(drop);
        assert!(matches!(r, Err(Failure::Invalid(_) | Failure::Unsupported(_))), "{ty:#x}: {r:?}");
        let i = crate::inspect::inspect(&v, false, &|_| None, None).map(|_| ());
        assert!(
            matches!(&i, Err(Failure::Invalid(e)) if e.contains("content blocks after the last metadata block")),
            "{ty:#x}: {i:?}"
        );
    }
}

/// read/8: a formula block's output count of 0 or past what the archive holds, and numbers not
/// in their shortest form (valid but for block types).
#[test]
fn output_counts_and_long_numbers() {
    let n = natives();
    let data = [0u8, 0, 0, LITERAL, 1, b'x'];
    let run = |outputs: u64, d: &[u8]| {
        let c =
            archive(|out| formula_block(out, outputs, &always_compressed(&mut CCtx::create(), &[d], 3, 1).unwrap()));
        aqr_testkit::within(std::time::Duration::from_secs(10), || {
            aqr_testkit::case(&format!("{outputs} outputs"));
            let r = decode_value(&c, &n);
            assert!(aqr_testkit::most() < 1 << 30);
            r
        })
        .unwrap()
    };
    assert_eq!(run(1, &data), Ok(Value::File(b"x".to_vec())));
    for outputs in [0, 2, 1 << 32, 1 << 40, u64::MAX] {
        assert!(matches!(run(outputs, &data), Err(Failure::Invalid(_))), "{outputs}");
    }
    // the counts and indexes of a formula in a longer form than needed
    let long = [0x80u8, 0x00, 0x80, 0x80, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];
    assert_eq!(
        run(1, &[0x80, 0x00, 0x80, 0x00, 0x80, 0x00, LITERAL, 0x81, 0x00, b'x']),
        Ok(Value::File(b"x".to_vec()))
    );
    // (a number past 64 bits is not)
    let mut over = vec![0x80u8; 10];
    over.push(0x00);
    over.extend_from_slice(&[0, 0, LITERAL, 1, b'x']);
    assert!(matches!(run(1, &over), Err(Failure::Invalid(_))));
    assert!(matches!(run(1, &[&long[..], &[0, 0]].concat()), Err(Failure::Invalid(_))));
    // the footer's fields in a longer form (the size hint, the metadata offset) are read
    let c = formula(&data, &[]);
    let fields = u16::from_le_bytes([c[c.len() - 2], c[c.len() - 1]]) as usize;
    let end = c.len() - 2 - fields - 8;
    let mut f = Vec::new();
    f.extend_from_slice(&[(end - HEADER_MAGIC.len()) as u8 | 0x80, 0x00, 0x80, 0x00]);
    let mut longer = c[..end].to_vec();
    longer.extend_from_slice(&FOOTER_MAGIC);
    longer.extend_from_slice(&(f.len() as u16).to_le_bytes());
    longer.extend_from_slice(&f);
    longer.extend_from_slice(&(f.len() as u16).to_le_bytes());
    assert_eq!(decode(&longer, &n), decode(&c, &n));
    // (a block's type is not: the type in a longer form is invalid)
    let mut typed = c.clone();
    let at = HEADER_MAGIC.len();
    typed.splice(at..at + 1, [typed[at] | 0x80, 0x00]);
    assert!(matches!(decode(&typed, &n), Err(Failure::Invalid(_))));
}

/// read/8: `journal` and `segment-status` of the optional metadata, at their ranges: times of
/// an unsigned 64-bit integer (any, but not repeated or negative), labels of text, counts of
/// the unsigned.
#[test]
fn journal_and_status_ranges() {
    use crate::tree::{self, Status};
    let read = |opt: &[u8]| {
        let m = Meta { prev: 5, dirs: vec![(0, "d")], opt: opt.to_vec(), ..Meta::default() };
        tree::read(&m.bytes()).map(|m| (m.journal, m.status))
    };
    let journal = |pairs: &[u8]| [&[0xa1, 0x23][..], pairs].concat();
    let status = |v: &[u8]| [&[0xa1, 0x22][..], v].concat();
    let bad = |opt: &[u8], want: &str| match read(opt) {
        Err(Failure::Invalid(e)) => assert!(e.contains(want), "{e} for {want}"),
        r => panic!("{r:?} for {want}"),
    };
    // times: 0, a middle one, the largest u64, in the order of the times whatever the order given
    let ok = read(&journal(&[
        0xa3, 0x00, 0x61, b'a', 0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x61, b'c', 0x18, 0x40, 0x61,
        b'b',
    ]));
    let (points, _) = ok.unwrap();
    assert_eq!(points, [(0, "a".to_string()), (0x40, "b".to_string()), (u64::MAX, "c".to_string())]);
    // an empty journal; a time negative, one past u64 as a bignum, a label not text, repeated times
    assert_eq!(read(&journal(&[0xa0])).unwrap().0, []);
    bad(&journal(&[0xa1, 0x20, 0x61, b'a']), "a journal time out of range");
    bad(&journal(&[0xa1, 0x3b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x61, b'a']), "out of range");
    bad(&journal(&[0xa1, 0xc2, 0x49, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0x61, b'a']), "not an integer");
    bad(&journal(&[0xa1, 0x00, 0x01]), "");
    // (a byte string is an `aqr-text` of an unknown encoding)
    assert_eq!(read(&journal(&[0xa1, 0x00, 0x41, 0x00])).unwrap().0, [(0, "\0".to_string())]);
    bad(&journal(&[0xa2, 0x00, 0x61, b'a', 0x00, 0x61, b'b']), "duplicate");
    bad(&journal(&[0xa1, 0x61, b'x', 0x61, b'a']), "not an integer");
    bad(&journal(&[0x01]), "journal not a map");
    bad(&journal(&[0x80]), "journal not a map");
    // a text that is not UTF-8 is a byte string's: of a text string it is invalid CBOR
    assert!(matches!(read(&journal(&[0xa1, 0x00, 0x62, 0xff, 0xfe])), Err(Failure::Invalid(_))));
    // a label longer than the data
    bad(&journal(&[0xa1, 0x00, 0x7b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]), "");
    // the journal twice
    bad(&[&[0xa2, 0x23, 0xa0, 0x23, 0xa0][..]].concat(), "duplicate");
    // the status: true, none, counts of every size, a count negative or past u64, not true
    assert_eq!(read(&status(&[0xf5])).unwrap().1, Status::Complete);
    assert_eq!(read(&status(&[0xa0])).unwrap().1, Status::Incomplete { files: None, bytes: None });
    assert_eq!(
        read(&status(&[0xa2, 0x00, 0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01, 0x00])).unwrap().1,
        Status::Incomplete { files: Some(u64::MAX), bytes: Some(0) }
    );
    bad(&status(&[0xf4]), "neither true nor a map");
    bad(&status(&[0xf6]), "neither true nor a map");
    bad(&status(&[0x01]), "neither true nor a map");
    bad(&status(&[0xa1, 0x00, 0x20]), "a count out of range");
    bad(&status(&[0xa1, 0x01, 0x3b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]), "a count out of range");
    bad(&status(&[0xa1, 0x00, 0xf5]), "a count not an integer");
    bad(&status(&[0xa1, 0x00, 0x61, b'1']), "a count not an integer");
    bad(&status(&[0xa2, 0x00, 0x01, 0x00, 0x02]), "duplicate");
    // other keys of the status are let be, repeated ones are not
    assert!(read(&status(&[0xa1, 0x05, 0x01])).is_ok());
    bad(&status(&[0xa2, 0x05, 0x01, 0x05, 0x02]), "duplicate");
    // both
    let both = [&[0xa2, 0x22, 0xf5][..], &[0x23, 0xa1, 0x01, 0x61, b'x']].concat();
    let r = read(&both).unwrap();
    assert_eq!((r.0, r.1), (vec![(1, "x".to_string())], Status::Complete));
}

/// read/8: the metadata block of a segment tells where its module blocks are: not where they
/// are, past its own segment, or not in order, it is invalid.
#[test]
fn module_offsets_of_segments() {
    use crate::{ContentAt, Entry, EntryNode, Extra, Leaf, SegmentWriter, Tree};
    let n = natives();
    let enc = Encoder::new(Level::STORED);
    let mut w = SegmentWriter::new();
    let mut out = SegmentWriter::header().to_vec();
    let mut metas = Vec::new();
    for k in 0..2usize {
        let mut f = Formula::default();
        let a = f.push("a", format!("file {k}").repeat(10).into_bytes());
        let len = 60;
        let leaves = vec![Leaf { outputs: vec![Node::apply(REV, vec![1, 2, 3], vec![a], len)], sums: vec![None] }];
        let b = w.batch(&enc, &f, &leaves).unwrap();
        w.commit(&b);
        out.extend_from_slice(b.blocks());
        let name = format!("f{k}").into_bytes();
        let t = Tree {
            entries: vec![Entry { parent: None, name, node: EntryNode::File(0), extra: Extra::default() }],
            ..Tree::default()
        };
        let contents = [ContentAt { leaf: b.leaf_at[0], output: 0, size: len }];
        let m = if k == 0 { w.close(&enc, &t, &contents).unwrap() } else { w.finish(&enc, &t, &contents).unwrap().0 };
        metas.push(out.len());
        out.extend_from_slice(&m);
    }
    assert_eq!(decode_value(&out, &n).map(|_| ()), Ok(()));
    // where each metadata block's list of module offsets starts (its fields: prev, the counts)
    let fields_of = |c: &[u8], at: usize| -> (usize, usize) {
        let framed_at = at;
        let (mut p, mut q) = (0, 0);
        // (the block: its type, a size field, then the data; the type 0x10 + the size field)
        let ty = c[framed_at] as usize;
        assert_eq!(ty & 0xf0, 0x10, "a metadata block");
        let start = framed_at + 1 + (1 << (ty % 4));
        let d = &c[start..];
        let prev = leb(d, &mut p).unwrap();
        let modules = leb(d, &mut p).unwrap();
        assert_eq!(modules, 1);
        for _ in 0..7 {
            leb(d, &mut p).unwrap();
        }
        q += start + p;
        (q, if prev < 0x100 { 1 } else { 2 })
    };
    for (k, &at) in metas.iter().enumerate() {
        let (field, width) = fields_of(&out, at);
        assert_eq!(width, 1, "the sample is small");
        let good = out[field];
        let start = at + 1 + (1 << (out[at] % 4));
        let prev = {
            let mut p = 0;
            leb(&out[start..], &mut p).unwrap()
        };
        // each new value of the offset, the block's checksum made again (the data's length is
        // the same: the field's width)
        let size = {
            let ty = out[at] as usize;
            let w = 1 << (ty % 4);
            let mut b = [0u8; 8];
            b[..w].copy_from_slice(&out[at + 1..at + 1 + w]);
            u64::from_le_bytes(b) as usize
        };
        for value in [0, 1, good.wrapping_sub(2), good + 2, good + 1, prev as u8, (prev as u8).wrapping_add(1), 0xff] {
            if value == good {
                continue;
            }
            let mut c = out.clone();
            c[field] = value;
            let sum = checksum_of(&[&c[start..start + size - 8]]).to_le_bytes();
            c[start + size - 8..start + size].copy_from_slice(&sum);
            let verdicts = [
                decode_value(&c, &n).map(drop),
                open_indexed(Payload::Plain(&c), &n, 1).and_then(|a| a.value(1)).map(drop),
            ];
            for r in verdicts {
                assert!(matches!(r, Err(Failure::Invalid(_))), "segment {k}, offset {value} for {good}: {r:?}");
            }
        }
    }
}

/// A split's outputs spread over the leaves of a batch: refused (each output is taken in the
/// leaf it is made in, as there is no instruction dropping the others); taken by one leaf, in any
/// order and of any size (empty too), decoded as the split cuts it. fuzz/roundtrip found leaf
/// blocks written of such leaves, leaving more outputs than they declare.
#[test]
fn split_outputs_in_one_leaf() {
    use crate::{ContentAt, Entry, EntryNode, Extra, Leaf, SegmentWriter, Tree};
    let data: Vec<u8> = (0..100u8).collect();
    for (sizes, order) in
        [(vec![0, 0, 0, 0], [1, 2, 0, 3]), (vec![10, 20, 30, 40], [1, 2, 0, 3]), (vec![10, 20, 30, 40], [0, 1, 2, 3])]
    {
        let mut f = Formula::default();
        let total: u64 = sizes.iter().sum();
        let s = f.push("t", data[..total as usize].to_vec());
        let node = if total == 0 { Node::Concat(Vec::new()) } else { s };
        let outputs = f.split_node(node, sizes.clone());
        let at = |i: usize| sizes[..i].iter().sum::<u64>() as usize;
        let want: Vec<Vec<u8>> = order.iter().map(|&i| data[at(i)..at(i) + sizes[i] as usize].to_vec()).collect();
        let enc = Encoder::new(Level::STORED);
        let w = SegmentWriter::new();
        let spread = vec![
            Leaf { outputs: order[..3].iter().map(|&i| outputs[i].clone()).collect(), sums: vec![None; 3] },
            Leaf { outputs: vec![outputs[order[3]].clone()], sums: vec![None] },
        ];
        assert!(w.batch(&enc, &f, &spread).is_err(), "{sizes:?} {order:?}");
        // (with a leaf of a file of its own after it)
        let other = f.push("u", b"other".to_vec());
        let leaves = vec![
            Leaf { outputs: order.iter().map(|&i| outputs[i].clone()).collect(), sums: vec![None; 4] },
            Leaf { outputs: vec![other], sums: vec![None] },
        ];
        let mut w = SegmentWriter::new();
        let mut out = SegmentWriter::header().to_vec();
        let mut b = w.batch(&enc, &f, &leaves).unwrap();
        w.commit(&b);
        let (d, r) = b.take_blocks();
        out.extend_from_slice(&d[r]);
        let mut contents = Vec::new();
        for (&leaf, l) in b.leaf_at.iter().zip(&leaves) {
            for (output, n) in l.outputs.iter().enumerate() {
                contents.push(ContentAt { leaf, output, size: n.len() });
            }
        }
        let mut t = Tree::default();
        for i in 0..contents.len() {
            t.entries.push(Entry {
                parent: None,
                name: format!("f{i}").into_bytes(),
                node: EntryNode::File(i),
                extra: Extra::default(),
            });
        }
        out.extend_from_slice(&w.finish(&enc, &t, &contents).unwrap().0);
        match decode_value(&out, &natives()) {
            Ok(Value::Tree(_, files)) => {
                assert_eq!(files, [want, vec![b"other".to_vec()]].concat(), "{sizes:?} {order:?}")
            }
            r => panic!("{sizes:?} {order:?}: {:?}", r.map(|_| ())),
        }
    }
}

/// A footer telling of a metadata block where there is none, its offset back past the archive's
/// start: invalid, as the index's reading has it (fuzz/decode found the whole reading taking it,
/// the place computed and the last segment's metadata block both none).
#[test]
fn footer_of_metadata_before_the_archive() {
    let c = [
        0xf8, 0x04, 0x03, 0x6a, 0xd6, 0xbe, 0xfd, 0xaa, 0xae, 0xde, 0x85, 0x12, 0x03, 0x00, 0x00, 0xd6, 0x00, 0x03,
        0x00,
    ];
    assert!(locate(&c).is_some(), "an archive by its header and footer");
    let r = open(&c, &natives()).and_then(|a| a.value(1).map(drop));
    assert!(matches!(r, Err(Failure::Invalid(ref m)) if m.contains("metadata")), "{r:?}");
    assert!(crate::inspect::inspect(&c, false, &|_| None, Some(64)).is_err());
}

/// An archive opened for some paths alone is not evaluated whole (`value`): its files are taken
/// alone (`contents_of`), as the whole decoding has them. Read of every segment's metadata block
/// or not, it says so: fuzz/decode found it told invalid otherwise of its blocks read whole ("leaf
/// blocks not those the metadata blocks tell"), of its tree of those paths alone.
#[test]
fn opened_of_paths_evaluated_whole() {
    let c = segmented_archive();
    let n = natives();
    let whole = match decode_value(&c, &n).unwrap() {
        Value::Tree(t, files) => (t, files),
        Value::File(_) => panic!("a tree"),
    };
    // (of the last segment alone, and of the first: every segment's metadata block read)
    for name in [&b"c"[..], b"a"] {
        let paths = vec![vec![b"d".to_vec(), name.to_vec()]];
        let a = open_indexed_at(Payload::Plain(&c), &n, 1, &paths).unwrap();
        let r = a.value(1).map(drop);
        assert!(matches!(r, Err(Failure::Invalid(ref m)) if m.contains("some of its paths")), "{r:?}");
        let t = a.tree().unwrap();
        let i = t.entries.iter().position(|e| e.name == name).unwrap();
        let EntryNode::File(k) = t.entries[i].node else { panic!("a file") };
        let mut wanted = vec![false; a.contents_len()];
        wanted[k] = true;
        let got = std::sync::Mutex::new(None);
        a.contents_of(1, Some(&wanted), |_, d| {
            *got.lock().unwrap() = Some(d);
            Ok(())
        })
        .unwrap();
        let w = whole.0.entries.iter().position(|e| e.name == name).unwrap();
        let EntryNode::File(wk) = whole.0.entries[w].node else { panic!("a file") };
        assert_eq!(got.into_inner().unwrap().as_ref(), Some(&whole.1[wk]));
    }
}

/// Zstandard data declaring more than its blocks can make (each block of 4 bytes at least where
/// it makes any, and of 128 KiB at most: RFC 8878's Block_Maximum_Size) is invalid at once, not
/// reserved as declared: an RLE block of 4 bytes declaring 2^40 bytes, against the same of its
/// 128 KiB. (Of the reservations fuzzing showed, `docs/edge-tests-todo.md`.)
#[test]
fn zstd_sizes_beyond_blocks() {
    // (Last_Block, Block_Type 1 (RLE), Block_Size 128 KiB; the byte repeated)
    let n = 128u32 << 10;
    let head = (n << 3 | 1 << 1 | 1).to_le_bytes();
    let blocks = [head[0], head[1], head[2], b'a'];
    let data = |size: u64| {
        let mut d = Vec::new();
        aqr_common::varint::put(&mut d, size - blocks.len() as u64);
        // (a window of 128 KiB: Block_Maximum_Size is the smaller of it and 128 KiB)
        d.push(7 << 3);
        d.extend_from_slice(&blocks);
        d.extend_from_slice(&checksum(&vec![b'a'; n as usize]).to_le_bytes());
        d
    };
    aqr_testkit::case("zstd_sizes_beyond_blocks");
    let r = crate::parse::decoded(&data(1 << 40), true, true);
    assert!(matches!(r, Err(Failure::Invalid(_))), "{:?}", r.map(|d| d.len()));
    assert!(aqr_testkit::most() < 1 << 24, "a request of {} bytes", aqr_testkit::most());
    assert_eq!(crate::parse::decoded(&data(n as u64), true, true).unwrap(), vec![b'a'; n as usize]);
}
