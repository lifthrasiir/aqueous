//! Peak heap use of archiving several files in a batch: the files are held once, as they are
//! read, not copied again into the formula the batch is made of. (A test binary of its own:
//! the allocator counts every thread.)

use std::io::Write;

use aqr_common::mem::{Counting, peak};
use aqr_compress::{Input, Options, compress_tree_to};
use aqr_formula::aqr::{Entry, EntryNode, Extra, Tree};

#[global_allocator]
static A: Counting = Counting;

fn text(n: usize, seed: usize) -> Vec<u8> {
    let mut t = Vec::with_capacity(n + 100);
    for i in 0.. {
        if t.len() >= n {
            break;
        }
        writeln!(t, "line {i}: the quick brown fox jumps over {} lazy dogs", (i + seed) * 7 % 13).unwrap();
    }
    t
}

/// Bytes that do not compress.
fn noise(n: usize, mut x: u64) -> Vec<u8> {
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

#[test]
fn files_held_once() {
    let n = 24 << 20;
    let files = [("a.txt", text(n, 1)), ("b.txt", text(n, 2)), ("c.bin", noise(n, 3))];
    let mut tree = Tree::default();
    for (c, (name, _)) in files.iter().enumerate() {
        let e = Entry { parent: None, name: name.as_bytes().to_vec(), node: EntryNode::File(c), extra: Extra::default() };
        tree.entries.push(e);
    }
    let inputs: Vec<Input> = files.iter().map(|(name, d)| Input { name: name.as_bytes(), data: d }).collect();
    let opt = Options { threads: 4, ..Options::default() };
    let mut out = Vec::with_capacity(4 * n);
    let (r, used) = peak(|| compress_tree_to(&tree, &inputs[..], &opt, &mut out).unwrap());
    assert!(r.unverified.is_empty() && !r.files[0].modules.is_empty());
    let total = 3 * n;
    println!("{:.1} MB of files: peak {:.1} MB ({:.2}x)", total as f64 / 1e6, used as f64 / 1e6, used as f64 / total as f64);
    // the files read (once), and what is made of them: each text coded, and decoded again to
    // verify it (both at once), the blocks and the archive decoded (a copy of the files
    // besides: 6.6 times a file over them)
    assert!(used < total + 5 * n, "peak {used} bytes for {total} bytes of files");
}
