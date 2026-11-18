//! Formulas evaluated ([`Evaluate`]), as recompressors make them and as the archive encoder
//! changes them (compacted, classes merged, slices shared, `either`s resolved).

use std::sync::Arc;

use aqr_archive::{Evaluate, Modules};
use aqr_formula::*;

const REV: Module = Module::new("rev", 1);

/// `rev` reverses the concatenation of its inputs.
struct Rev;

impl Modules for Rev {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        assert_eq!(a.module, REV);
        Ok(inputs.concat().into_iter().rev().collect())
    }
}

#[test]
fn concat_flattens() {
    let n = Node::concat([
        Node::slice(0, 0, 2),
        Node::concat([Node::slice(0, 2, 3), Node::slice(1, 0, 0)]),
        Node::slice(0, 6, 1),
    ]);
    assert_eq!(n, Node::Concat(vec![Node::slice(0, 0, 5), Node::slice(0, 6, 1)]));
    assert_eq!(Node::concat([Node::slice(0, 0, 1), Node::slice(0, 1, 1)]), Node::slice(0, 0, 2));
}

#[test]
fn cut_by_sizes() {
    let n = Node::Concat(vec![Node::slice(0, 0, 5), Node::slice(1, 10, 3)]);
    assert_eq!(
        n.cut(&[2, 0, 4, 2]),
        [
            Node::slice(0, 0, 2),
            Node::Concat(Vec::new()),
            Node::Concat(vec![Node::slice(0, 2, 3), Node::slice(1, 10, 1)]),
            Node::slice(1, 11, 2),
        ]
    );
    assert_eq!(Node::slice(0, 4, 6).cut(&[6]), [Node::slice(0, 4, 6)]);
}

#[test]
fn eval_parts() {
    let mut f = Formula::default();
    let a = f.push("a", b"hello".to_vec());
    let b = f.push("b", b"xyz".to_vec());
    f.root = Node::Concat(vec![
        Node::apply(REV, vec![], vec![a.clone()], 5),
        Node::Concat(vec![Node::slice(1, 0, 2), Node::apply(REV, vec![], vec![b], 3)]),
        a,
    ]);
    let mut parts = Vec::new();
    f.eval_parts(&Rev, 1, &mut |p| {
        parts.push(p.to_vec());
        Ok(())
    })
    .unwrap();
    // (a part each, what the root concatenates and what that does in turn)
    assert_eq!(parts, [&b"olleh"[..], b"xy", b"zyx", b"hello"]);
    assert_eq!(parts.concat(), f.eval(&Rev).unwrap());
    // (a failure of the part's taker stops it)
    let mut n = 0;
    let r = f.eval_parts(&Rev, 1, &mut |_| {
        n += 1;
        Err(Error::new("enough"))
    });
    assert!(r.is_err() && n == 1);
}

#[test]
fn eval_embedded() {
    let child = Formula::raw("b", b"xyz".to_vec());
    let mut f = Formula::default();
    let a = f.push("a", b"hello".to_vec());
    let b = f.embed(child);
    assert_eq!(b, Node::slice(1, 0, 3));
    f.root = Node::concat([Node::slice(0, 0, 2), Node::apply(REV, vec![], vec![a, b], 8)]);
    assert_eq!(f.len(), 10);
    assert_eq!(f.eval(&Rev).unwrap(), b"hezyxolleh");
    assert_eq!(f.applies().len(), 1);
    let shown = f.to_string();
    assert_eq!(
        shown,
        "concat 10\n  slice #0 [0, +2) a\n  apply rev@1 8\n    slice #0 [0, +5) a\n    slice #1 [0, +3) b\n"
    );
    f.streams[1].kind = Kind::Format("zlib");
    assert!(f.to_string().ends_with("slice #1 [0, +3) b (format=zlib)\n"), "{f}");
    f.streams[1].content = true;
    assert!(f.to_string().ends_with("slice #1 [0, +3) b (format=zlib, content)\n"), "{f}");
}

/// The streams sliced in an input its module tells is data as it is are contents, those of
/// its other inputs not, nor those of an application within it but by its own module.
#[test]
fn contents_marked() {
    const DATA: Module = Module::new("data", 1).with_contents(&[0]);
    let mut f = Formula::default();
    let [a, b, c, d] = ["a", "b", "c", "d"].map(|s| f.push(s, s.as_bytes().to_vec()));
    let inner = Node::apply(REV, vec![], vec![c], 1);
    f.root = Node::concat([Node::apply(DATA, vec![], vec![Node::concat([a, inner]), b], 2), d]);
    f.mark_contents();
    assert_eq!(f.streams.iter().map(|s| s.content).collect::<Vec<_>>(), [true, false, false, false]);
    f.root = Node::apply(DATA, vec![], vec![Node::apply(DATA, vec![], vec![Node::slice(2, 0, 1)], 1)], 1);
    f.mark_contents();
    assert!(f.streams[2].content);
}

#[test]
fn eval_pack() {
    let mut f = Formula::raw("a", b"hello".to_vec());
    f.root = Node::pack([
        Node::slice(0, 1, 2),
        Node::apply(REV, vec![], vec![Node::slice(0, 0, 3)], 3),
        Node::slice(0, 0, 0),
    ]);
    assert_eq!(f.len(), 32 + 5);
    let out = f.eval(&Rev).unwrap();
    assert_eq!(packed::unpack(&out).unwrap(), [&b"el"[..], b"leh", b""]);
    assert_eq!(
        f.to_string(),
        "pack 37\n  slice #0 [1, +2) a\n  apply rev@1 3\n    slice #0 [0, +3) a\n  slice #0 [0, +0) a\n"
    );
}

/// Contents of a class are merged as the formula is archived ([`Formula::merged`]), as
/// nothing recompresses them any more, but not as recompressors merge classes.
#[test]
fn contents_merged_as_archived() {
    let mut f = Formula::default();
    let a = f.push_content("raw", Kind::Unknown, b"one".to_vec());
    let b = f.push_content("raw", Kind::Unknown, b"two".to_vec());
    f.root = Node::concat([a, b]);
    let (m, groups) = f.merged();
    assert_eq!(groups, [vec![(0, 0, 3), (1, 0, 3)]]);
    assert!(m.streams.len() == 1 && m.streams[0].content);
    let mut g = f.clone();
    g.merge_classes();
    assert_eq!(g.streams.len(), 2);
}

/// Alternatives, one within another and one of an application of several outputs: listed
/// innermost first, each resolved either way to the same output, what the others took
/// dropped; refused unresolved by the encoder and evaluation.
#[test]
fn eithers_resolved() {
    let mut f = Formula::default();
    let either = |kept, made, len| Node::Either(Box::new(Either { kept, made, len, sizes: (0, 0) }));
    let raw = f.push("raw", b"hello world".to_vec());
    let rev = f.push("rev", b"dlrow olleh".to_vec());
    let parts = Node::concat([f.push("a", b"dlrow".to_vec()), f.push("b", b" olleh".to_vec())]);
    let inner = either(rev, parts, 11);
    let outer = either(raw, Node::apply(REV, vec![], vec![inner], 11), 11);
    let ab = f.push("ab", b"ab".to_vec());
    let ba = f.push("ba", b"ba".to_vec());
    let Node::Apply(a) = Node::apply(REV, vec![], vec![ba], 2) else { unreachable!() };
    let outs = f.split(*a, vec![1, 1]);
    f.root = Node::concat([outer, either(ab, Node::concat(outs), 2)]);
    assert_eq!(f.eithers().iter().map(|e| (e.0, e.1.len)).collect::<Vec<_>>(), [(1, 11), (0, 11), (0, 2)]);
    assert!(f.eval(&Rev).is_err());
    assert!(aqr_archive::encode(&f, None, aqr_archive::Level::MID).is_err());
    // chosen as either: the streams only the others took dropped, the rest as they were
    for made in [false, true] {
        let g = f.chosen(made);
        assert_eq!(g.eval(&Rev).unwrap(), b"hello worldab");
        let classes: Vec<&str> = g.streams.iter().map(|s| &*s.class).collect();
        assert_eq!(classes, if made { &["a", "b", "ba"][..] } else { &["raw", "ab"][..] });
        assert_eq!(g.shared.len(), made as usize);
    }
    // a stream no slice took stays
    let mut h = f.clone();
    h.push("unused", b"z".to_vec());
    assert_eq!(h.chosen(false).streams.last().map(|s| &*s.class), Some("unused"));
    for k in 0..8 {
        let made = [k & 1 != 0, k & 2 != 0, k & 4 != 0];
        let mut g = f.clone();
        g.resolve(&made);
        assert!(g.eithers().is_empty());
        assert_eq!(g.eval(&Rev).unwrap(), b"hello worldab", "{made:?}");
        assert_eq!(g.shared.len(), made[2] as usize, "{made:?}");
        let classes: Vec<&str> = g.streams.iter().map(|s| &*s.class).collect();
        let want: Vec<&str> = match (made[1], made[0]) {
            (false, _) => vec!["raw"],
            (true, false) => vec!["rev"],
            (true, true) => vec!["a", "b"],
        };
        assert_eq!(classes, [want, vec![if made[2] { "ba" } else { "ab" }]].concat(), "{made:?}");
    }
}

#[test]
fn compact() {
    let mut f = Formula::default();
    f.push("a", b"0123456789".to_vec());
    f.push("unused", b"xyz".to_vec());
    f.push("b", b"abcdef".to_vec());
    f.root = Node::concat([
        Node::slice(0, 6, 2),
        Node::apply(REV, vec![], vec![Node::slice(0, 1, 3), Node::slice(0, 2, 3), Node::slice(0, 9, 0)], 4),
        Node::slice(2, 4, 3),
        Node::slice(5, 0, 0),
    ]);
    let mut c = f.clone();
    c.compact();
    // (failing alike: the streams it names are numbered anew)
    assert_eq!(c.eval(&Rev).ok(), f.eval(&Rev).ok());
    // shared bytes: only those kept copied, the same as of its own
    let mut s = f.clone();
    s.streams[0].data = Bytes::shared(Arc::new(b"0123456789".to_vec()));
    s.compact();
    assert!(!s.streams[0].data.is_shared());
    assert_eq!(s, c);
    // The slice past the end of "b" keeps it whole; the slice of no stream stays so.
    assert_eq!(
        c.streams.iter().map(|s| (&*s.class, &s.data[..])).collect::<Vec<_>>(),
        [("a", &b"123467"[..]), ("b", &b"abcdef"[..])]
    );
    assert_eq!(
        c.root,
        Node::concat([
            Node::slice(0, 4, 2),
            Node::apply(REV, vec![], vec![Node::slice(0, 0, 3), Node::slice(0, 1, 3), Node::slice(0, 6, 0)], 4),
            Node::slice(1, 4, 3),
            Node::slice(4, 0, 0),
        ])
    );
    f.root = Node::slice(2, 1, 2);
    f.compact();
    assert_eq!(f.eval(&Rev).unwrap(), b"bc");
    assert_eq!(f.streams.len(), 1);
    assert_eq!(f.streams[0].data, b"bc");
    // a stream sliced whole is not copied
    let mut f = Formula::raw("a", b"abc".to_vec());
    let whole = f.streams[0].data.as_ptr();
    f.compact();
    assert_eq!(f.streams[0].data.as_ptr(), whole);
}

const CUT: Module = Module::new("cut", 1);

/// `rev`, and `cut`, cutting the concatenation of its inputs into its outputs.
struct RevCut;

impl Modules for RevCut {
    fn apply(&self, a: &Apply, inputs: &[&[u8]]) -> Result<Vec<u8>> {
        Rev.apply(a, inputs)
    }

    fn apply_shared(&self, s: &Shared, inputs: &[&[u8]]) -> Result<Vec<Vec<u8>>> {
        assert_eq!(s.apply().unwrap().module, CUT);
        let (all, mut at) = (inputs.concat(), 0);
        let mut next = |n: u64| {
            at += n as usize;
            all[at - n as usize..at].to_vec()
        };
        Ok(s.sizes.iter().map(|&n| next(n)).collect())
    }
}

fn cut(inputs: Vec<Node>) -> Apply {
    Apply {
        module: CUT,
        resources: vec![],
        params: vec![],
        inputs,
        len: 0,
        memory_limit: crate::NO_LIMIT,
        task_limits: Vec::new(),
    }
}

/// Outputs of applications of several outputs, taken by the root and by later ones; shown,
/// embedded and compacted.
#[test]
fn eval_shared() {
    let mut f = Formula::raw("a", b"abcdefgh".to_vec());
    let a = f.root.clone();
    let [o0, o1] = <[Node; 2]>::try_from(f.share(cut(vec![a]), vec![3, 5])).unwrap();
    let [p0, p1] = <[Node; 2]>::try_from(f.share(cut(vec![o1]), vec![1, 4])).unwrap();
    f.root = Node::concat([p1, Node::slice(0, 7, 1), o0, Node::apply(REV, vec![], vec![p0], 1)]);
    assert_eq!(f.len(), 9);
    assert_eq!(f.eval(&RevCut).unwrap(), b"efghhabcd");
    assert_eq!(f.applies().len(), 3);
    assert_eq!(
        f.to_string(),
        "shared #0 cut@1 3 5\n  slice #0 [0, +8) a\nshared #1 cut@1 1 4\n  output #0.1 5\n\
         concat 9\n  output #1.1 4\n  slice #0 [7, +1) a\n  output #0.0 3\n  apply rev@1 1\n    output #1.0 1\n"
    );
    // embedded after a formula of its own application of several outputs
    let mut g = Formula::raw("b", b"xy".to_vec());
    let b = g.root.clone();
    let [x, y] = <[Node; 2]>::try_from(g.share(cut(vec![b]), vec![1, 1])).unwrap();
    let child = g.embed(f.clone());
    g.root = Node::concat([y, child, x]);
    assert_eq!(g.eval(&RevCut).unwrap(), b"yefghhabcdx");
    // compacted: the slices of the applications are kept
    let mut c = f.clone();
    c.streams[0].data.extend_from_slice(b"unused");
    c.compact();
    assert_eq!(c.eval(&RevCut).unwrap(), b"efghhabcd");
    assert_eq!(c.streams[0].data, b"abcdefgh");

    // split from the one output of a module
    let mut s = Formula::raw("a", b"abcdefgh".to_vec());
    let rev = Apply {
        module: REV,
        resources: vec![],
        params: vec![],
        inputs: vec![s.root.clone()],
        len: 0,
        memory_limit: crate::NO_LIMIT,
        task_limits: Vec::new(),
    };
    let [x, y] = <[Node; 2]>::try_from(s.split(rev, vec![5, 3])).unwrap();
    s.root = Node::concat([y, x]);
    assert_eq!(s.eval(&Rev).unwrap(), b"cbahgfed");
}

/// A patch of a slice against another one, shown.
#[test]
fn eval_patch() {
    let (source, target) = (b"the quick brown fox jumps".to_vec(), b"the quick red fox jumps!".to_vec());
    let mut f = Formula::raw("a", source.clone());
    let d = f.push_as("d", Kind::Compressed, patch::make(&source, &target, 3).unwrap());
    let n = target.len() as u64;
    f.root = Node::concat([Node::slice(0, 0, 4), Node::patch(Node::slice(0, 0, 25), d, n)]);
    assert_eq!(f.eval(&Rev).unwrap(), [&b"the "[..], &target].concat());
    assert!(f.to_string().starts_with("concat 28\n  slice #0 [0, +4) a\n  patch 24\n    slice #0 [0, +25) a\n"), "{f}");
    let Node::Concat(ns) = &mut f.root else { unreachable!() };
    let Node::Patch(p) = &mut ns[1] else { unreachable!() };
    p.len += 1;
    assert!(f.eval(&Rev).is_err());
}

/// Streams of a class and kind merged, but those compressed, the contents, those apart and
/// those a slice goes past; the output the same.
#[test]
fn merge_classes() {
    let mut f = Formula::default();
    let a0 = f.push("a", b"012".to_vec());
    let b = f.push("b", b"xyz".to_vec());
    let a1 = f.push("a", b"345".to_vec());
    let c = f.push_as("a", Kind::Compressed, b"ccc".to_vec());
    let t = f.push_content("a", Kind::Unknown, b"ttt".to_vec());
    f.push("a", b"678".to_vec());
    let a3 = f.push("a", b"9".to_vec());
    let p = f.push_apart("a", b"ppp".to_vec());
    f.root = Node::concat([
        a1,
        Node::apply(REV, vec![], vec![a0, b, Node::slice(5, 1, 2)], 8),
        c,
        t,
        a3,
        Node::slice(5, 0, 1),
        p,
    ]);
    let want = f.eval(&Rev).unwrap();
    let mut g = f.clone();
    g.merge_classes();
    assert_eq!(g.eval(&Rev), Ok(want));
    let classes: Vec<_> = g.streams.iter().map(|s| (&*s.class, &s.data[..])).collect();
    assert_eq!(
        classes,
        [("a", &b"0123456789"[..]), ("b", &b"xyz"[..]), ("a", &b"ccc"[..]), ("a", &b"ttt"[..]), ("a", &b"ppp"[..])]
    );
    // within 6 bytes a stream
    let mut g = f.clone();
    g.merge_classes_within(6);
    assert_eq!(g.eval(&Rev), f.eval(&Rev));
    let classes: Vec<_> = g.streams.iter().map(|s| (&*s.class, &s.data[..])).collect();
    assert_eq!(
        classes,
        [
            ("a", &b"012345"[..]),
            ("b", &b"xyz"[..]),
            ("a", &b"ccc"[..]),
            ("a", &b"ttt"[..]),
            ("a", &b"6789"[..]),
            ("a", &b"ppp"[..])
        ]
    );
    // a slice past the end of the last keeps it apart
    let mut past = f.clone();
    let Node::Concat(ns) = &mut past.root else { unreachable!() };
    ns.push(Node::slice(6, 0, 3));
    past.merge_classes();
    assert_eq!(past.streams.len(), 6);
    assert_eq!((&past.streams[0].data[..], &past.streams[4].data[..]), (&b"012345678"[..], &b"9"[..]));
}

/// Streams longer than half the limit cut into pieces, those of different streams most alike
/// put together, the rest of each stream one; the output the same.
#[test]
fn merge_pieces() {
    let p = PIECE_MIN.get();
    let noise = |n: usize, seed: u64| -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| (x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407), (x >> 56) as u8).1)
            .collect()
    };
    let a = noise(3 * p, 1);
    // a's first two pieces swapped, then others
    let b = [&a[p..2 * p], &a[..p], &noise(p / 2, 2)].concat();
    let mut f = Formula::default();
    let na = f.push("c", a.clone());
    let nb = f.push("c", b.clone());
    let s = f.push("c", b"small".to_vec());
    let o = f.push("d", b"other".to_vec());
    f.root = Node::concat([
        Node::apply(REV, vec![], vec![Node::slice(0, p as u64 - 10, p as u64 + 20)], p as u64 + 20),
        nb,
        s,
        o,
        Node::slice(1, 10, 2 * p as u64),
        na,
    ]);
    let want = f.eval(&Rev).unwrap();
    let mut g = f.clone();
    g.merge_classes_within(2 * PIECE_MIN.get() as u64);
    assert_eq!(g.eval(&Rev), Ok(want));
    let streams: Vec<_> = g.streams.iter().map(|s| (&*s.class, &s.data[..])).collect();
    let (a, b) = (&a[..], &b[..]);
    let (ab, ba) = ([&a[..p], &b[p..2 * p]].concat(), [&a[p..2 * p], &b[..p]].concat());
    assert_eq!(
        streams,
        [("c", &ab[..]), ("c", &ba[..]), ("c", &a[2 * p..]), ("c", &b[2 * p..]), ("c", b"small"), ("d", b"other")]
    );
    // not within a limit of less than twice the least piece
    let mut g = f.clone();
    g.merge_classes_within(2 * PIECE_MIN.get() as u64 - 1);
    assert_eq!(g.streams.len(), 4);
}

/// Slices cut into the shorter ones they start with, wherever those lie, but those shorter
/// than the least, and those past the end of their stream; the output the same.
#[test]
fn share_slices() {
    let long: Vec<u8> = (0..40u32).flat_map(|i| i.to_le_bytes()).collect();
    let mut f = Formula::default();
    let a = f.push("a", long[..100].to_vec());
    let b = f.push_content("b", Kind::Unknown, long.clone());
    let c = f.push("c", long[..100].to_vec());
    let d = f.push("d", long[..8].to_vec());
    f.push("e", long[..120].to_vec());
    let mut other = long[..100].to_vec();
    other[99] ^= 1;
    let o = f.push("o", other);
    // long twice within, after other bytes
    f.push("z", [&b"xyz"[..], &long[..], &long[..]].concat());
    // long's start, but less than half of what a keeps after the 20 bytes of c
    let mut q = long[..100].to_vec();
    q[50..].iter_mut().for_each(|x| *x ^= 0x55);
    let q = f.push("q", q);
    f.root = Node::concat([
        a,
        Node::apply(REV, vec![], vec![Node::slice(2, 0, 20), b], 180),
        c,
        d,
        o,
        Node::slice(4, 100, 20),
        Node::slice(6, 3, 120),
        Node::slice(6, 163, 160),
        Node::slice(6, 0, 4),
        q,
    ]);
    let want = f.eval(&Rev).unwrap();
    let mut g = f.clone();
    g.share_slices(16);
    assert_eq!(g.eval(&Rev), Ok(want));
    let classes: Vec<_> = g.streams.iter().map(|s| (&*s.class, s.data.len())).collect();
    // long[..20] of c, long[20..100] of a, long[100..120] of e, long[120..] of b (each
    // of those alone first); o its own but for its first 99 (of c, and the start of a's);
    // q but for the first 20
    assert_eq!(classes, [("a", 80), ("b", 40), ("c", 20), ("d", 8), ("e", 20), ("o", 1), ("z", 4), ("q", 80)]);
    // a slice past the end of one is left as it is, and keeps it whole
    let Node::Concat(ns) = &mut f.root else { unreachable!() };
    ns.push(Node::slice(2, 90, 20));
    f.share_slices(16);
    let classes: Vec<_> = f.streams.iter().map(|s| (&*s.class, s.data.len())).collect();
    assert_eq!(classes, [("a", 80), ("b", 40), ("c", 100), ("d", 8), ("e", 20), ("o", 1), ("z", 4), ("q", 80)]);
    assert!(matches!(&f.root, Node::Concat(ns) if ns.last() == Some(&Node::slice(2, 90, 20))));
}

/// A slice cut within a concatenation below the root is spliced into it, not nested; the
/// root's parts stay its parts, each cut where it is.
#[test]
fn share_slices_flat() {
    let long: Vec<u8> = (0..40u32).flat_map(|i| i.to_le_bytes()).collect();
    let mut f = Formula::default();
    let a = f.push("a", long[..32].to_vec());
    let b = f.push("b", long.clone());
    let c = f.push("c", b"x".repeat(20));
    f.root = Node::Concat(vec![Node::Concat(vec![c, b.clone()]), b.clone(), a]);
    let want = f.eval(&Rev).unwrap();
    f.share_slices(16);
    assert_eq!(f.eval(&Rev), Ok(want));
    let Node::Concat(parts) = &f.root else { panic!("{f}") };
    assert_eq!(parts.len(), 3, "{f}");
    let nested = |n: &Node| matches!(n, Node::Concat(ns) if ns.iter().any(|n| matches!(n, Node::Concat(_))));
    assert!(!parts.iter().any(nested), "{f}");
    assert!(matches!(&parts[0], Node::Concat(ns) if ns.len() == 3), "{f}");
    assert!(matches!(&parts[1], Node::Concat(ns) if ns.len() == 2), "{f}");
}

/// A slice whose bytes repeat a piece over and over (padding, zeros) is cut at its first
/// copy only: the rest of the run is kept of its own bytes, which compress to little, not
/// cut into a slice of each copy.
#[test]
fn share_slices_runs() {
    let mut f = Formula::default();
    let z = f.push("z", vec![0; 16]);
    let run = f.push("run", [&[0; 200][..], b"some tail of its own"].concat());
    f.root = Node::Concat(vec![z, run]);
    let want = f.eval(&Rev).unwrap();
    f.share_slices(16);
    assert_eq!(f.eval(&Rev), Ok(want));
    let Node::Concat(parts) = &f.root else { panic!("{f}") };
    // the zeros' first 16 bytes of z, the rest of run's own
    assert_eq!(parts[1], Node::concat([Node::slice(0, 0, 16), Node::slice(1, 0, 204)]), "{f}");
    assert_eq!(f.streams[1].data.len(), 204);
}

/// Bytes shared are cloned without a copy, and copied once changed.
#[test]
fn shared_bytes() {
    let mut b = Bytes::from(vec![1, 2, 3]);
    let at = b.as_ptr();
    b.share();
    let mut c = b.clone();
    assert!(b.is_shared() && c.as_ptr() == at && *c == [1, 2, 3]);
    c[0] = 4;
    assert!(!c.is_shared() && *b == [1, 2, 3] && *c == [4, 2, 3]);
}

/// Outputs not taken once each, of other sizes, or taken by an application before theirs.
#[test]
fn shared_errors() {
    let mut f = Formula::raw("a", b"abcdefgh".to_vec());
    let a = f.root.clone();
    let [o0, o1] = <[Node; 2]>::try_from(f.share(cut(vec![a]), vec![3, 5])).unwrap();
    for root in [
        Node::concat([o0.clone()]),
        Node::concat([o0.clone(), o1.clone(), o0.clone()]),
        Node::concat([o0.clone(), Node::Output { shared: 0, index: 1, len: 4 }]),
        Node::concat([o0.clone(), o1.clone(), Node::Output { shared: 0, index: 2, len: 0 }]),
    ] {
        f.root = root;
        assert!(f.check_outputs().is_err() && f.eval(&RevCut).is_err(), "{f}");
    }
    f.root = Node::concat([o0, o1]);
    assert_eq!(f.eval(&RevCut).unwrap(), b"abcdefgh");
    f.shared[0].apply_mut().unwrap().inputs = vec![Node::Output { shared: 0, index: 0, len: 3 }];
    assert!(f.check_outputs().is_err());
    // a module of one output only, by default
    let mut g = Formula::raw("a", b"ab".to_vec());
    let a = g.root.clone();
    g.root = Node::concat(g.share(Apply { module: REV, ..cut(vec![a]) }, vec![1, 1]));
    assert!(g.eval(&Rev).is_err());
}

/// Applications of several outputs reordered: their outputs are taken where they were.
#[test]
fn shared_reordered() {
    let mut f = Formula::raw("a", b"abcdefgh".to_vec());
    let a = f.root.clone();
    let first = f.share(cut(vec![a]), vec![3, 5]);
    let second = f.share(cut(first), vec![4, 4]);
    f.root = Node::concat(second.into_iter().rev());
    let want = f.eval(&RevCut).unwrap();
    f.reorder_shared(&[1, 0]);
    assert!(f.check_outputs().is_err());
    f.reorder_shared(&[1, 0]);
    f.check_outputs().unwrap();
    assert_eq!(f.eval(&RevCut).unwrap(), want);
    let mut g = f.clone();
    g.reorder_shared(&[0, 1]);
    assert_eq!(g, f);
}

#[test]
fn eval_errors() {
    let mut f = Formula::raw("a", b"abc".to_vec());
    f.root = Node::slice(0, 1, 3);
    assert!(f.eval(&Rev).is_err());
    f.root = Node::apply(REV, vec![], vec![Node::slice(0, 0, 3)], 2);
    assert!(f.eval(&Rev).is_err());
}
