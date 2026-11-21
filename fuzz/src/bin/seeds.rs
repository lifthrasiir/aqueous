//! Writes the seed archives of the fuzz targets to the directories given (`corpus/decode`, say):
//! single files and trees, stored and compressed, of each instruction and of the toy modules.
//! Stored blocks keep their bytes in sight of the mutations; the checksums are skipped anyway.

use std::path::Path;

use aqr_archive::{ContentAt, Encoder, Entry, EntryNode, Extra, Leaf, Level, SegmentWriter, Tree, checksum, encode};
use aqr_formula::{Apply, Formula, Kind, NO_LIMIT, Node, patch};
use aqr_fuzz::{COPY, CUT, REV};

fn noise(n: usize, seed: u32) -> Vec<u8> {
    (0..n as u32).map(|i| (i.wrapping_add(seed).wrapping_mul(2654435761) >> 24) as u8).collect()
}

fn cut(inputs: Vec<Node>) -> Apply {
    Apply {
        module: CUT,
        resources: vec![],
        params: vec![],
        inputs,
        len: 0,
        memory_limit: NO_LIMIT,
        task_limits: Vec::new(),
    }
}

/// Formulas of a single file: what each exercises.
fn formulas() -> Vec<(&'static str, Formula)> {
    let mut out = Vec::new();
    let text = b"hello, aqueous; hello, aqueous; hello, aqueous".to_vec();

    let mut f = Formula::default();
    f.root = f.push("a", text.clone());
    out.push(("literal", f));

    let mut f = Formula::default();
    let a = f.push("a", noise(300, 1));
    let b = f.push("b", text.clone());
    f.root = Node::concat([Node::slice(1, 7, 5), a, Node::slice(0, 10, 20), b]);
    out.push(("concat", f));

    let mut f = Formula::default();
    let a = f.push("a", text.clone());
    f.root = Node::Pack(vec![a, Node::slice(0, 0, 3), Node::slice(0, 3, 0)]);
    out.push(("pack", f));

    let mut f = Formula::default();
    let a = f.push("a", text.clone());
    let b = f.push("b", noise(100, 2));
    f.root = Node::apply(REV, vec![1, 2, 3], vec![a, b], text.len() as u64 + 100);
    out.push(("rev", f));

    let mut f = Formula::default();
    let a = f.push("a", noise(5000, 3));
    f.root = Node::apply(COPY, vec![], vec![a, Node::slice(0, 100, 50)], 5050);
    out.push(("copy-part", f));

    let mut f = Formula::default();
    let a = f.push("a", b"abcdefgh".to_vec());
    let [o0, o1] = <[Node; 2]>::try_from(f.share(cut(vec![a]), vec![3, 5])).unwrap();
    let [p0, p1] = <[Node; 2]>::try_from(f.share(cut(vec![o1, Node::slice(0, 0, 2)]), vec![1, 6])).unwrap();
    f.root = Node::concat([p1, Node::slice(0, 7, 1), o0, Node::apply(REV, vec![], vec![p0], 1)]);
    out.push(("shared", f));

    let base = noise(3000, 4);
    let mut target = base[..2000].to_vec();
    target[100..150].fill(1);
    let mut f = Formula::default();
    let a = f.push("a", base.clone());
    let d = f.push_as("d", Kind::Compressed, patch::make(&base[..2000], &target, 3).unwrap());
    let [o0, o1] = <[Node; 2]>::try_from(f.share(cut(vec![a]), vec![2000, 1000])).unwrap();
    f.root = Node::concat([o1, Node::patch(o0, d, 2000)]);
    out.push(("patch", f));

    let mut f = Formula::default();
    let a = f.push("a", noise(64, 5));
    let parts = f.split_node(a, vec![10, 20, 34]);
    f.root = Node::concat(parts.into_iter().rev());
    out.push(("split", f));
    out
}

/// A tree of a directory, files (one of a module's output, two of one leaf), a link and a
/// fifo, in two batches.
fn tree(level: Level) -> Vec<u8> {
    let enc = Encoder::new(level);
    let mut f = Formula::default();
    let a = f.push("a", b"hello".to_vec());
    let b = f.push("b", noise(700, 6));
    let mut g = Formula::default();
    let c = g.push("c", noise(500, 7));
    let [o0, o1] = <[Node; 2]>::try_from(g.share(cut(vec![c]), vec![200, 300])).unwrap();
    let mut w = SegmentWriter::new();
    let mut out = SegmentWriter::header().to_vec();
    let mut contents = Vec::new();
    for (f, leaves) in [
        (
            &f,
            vec![
                Leaf { outputs: vec![Node::apply(REV, vec![], vec![a.clone(), b], 705)], sums: vec![None] },
                Leaf { outputs: vec![a], sums: vec![Some(checksum(b"hello"))] },
            ],
        ),
        (&g, vec![Leaf { outputs: vec![o1, o0], sums: vec![None, None] }]),
    ] {
        let sizes: Vec<Vec<u64>> = leaves.iter().map(|l| l.outputs.iter().map(Node::len).collect()).collect();
        let mut batch = w.batch(&enc, f, &leaves).unwrap();
        w.commit(&batch);
        let (d, r) = batch.take_blocks();
        out.extend_from_slice(&d[r]);
        for (&leaf, sizes) in batch.leaf_at.iter().zip(&sizes) {
            for (output, &size) in sizes.iter().enumerate() {
                contents.push(ContentAt { leaf, output, size });
            }
        }
    }
    let entry =
        |parent, name: &str, node| Entry { parent, name: name.as_bytes().to_vec(), node, extra: Extra::default() };
    let mut t = Tree::default();
    t.entries.push(entry(None, "d", EntryNode::Dir));
    for i in 0..contents.len() {
        t.entries.push(entry(Some(0), &format!("f{i}"), EntryNode::File(i)));
    }
    t.entries.push(entry(None, "l", EntryNode::Link(b"d/f0".to_vec())));
    t.entries.push(entry(None, "p", EntryNode::Fifo));
    out.extend_from_slice(&w.finish(&enc, &t, &contents).unwrap().0);
    out
}

fn main() {
    let dirs: Vec<String> = std::env::args().skip(1).collect();
    assert!(!dirs.is_empty(), "usage: seeds DIR...");
    let runner = aqr_fuzz::toys();
    let mut seeds =
        vec![("quine".to_string(), include_bytes!("../../../crates/aqr-archive/testdata/quine.aqr").to_vec())];
    for (name, f) in formulas() {
        for (l, level) in [("stored", Level::STORED), ("zstd", Level::Zstd(3))] {
            let c = encode(&f, None, level).unwrap();
            let want = aqr_archive::decode(&c, &runner).unwrap_or_else(|e| panic!("seed {name}: {e}"));
            seeds.push((format!("{name}-{l}"), c));
            if !matches!(f.root, Node::Slice { .. }) {
                seeds.push((format!("{name}-{l}-sum"), encode(&f, Some(checksum(&want)), level).unwrap()));
            }
        }
    }
    seeds.push(("tree-stored".into(), tree(Level::STORED)));
    seeds.push(("tree-zstd".into(), tree(Level::Zstd(3))));
    for (name, c) in &seeds {
        if let Err(e) = aqr_archive::decode_value(c, &runner) {
            panic!("seed {name} does not decode: {e}");
        }
        for d in &dirs {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(Path::new(d).join(format!("{name}.aqr")), c).unwrap();
        }
    }
    eprintln!("{} seeds", seeds.len());
}
