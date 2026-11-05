//! The Zstandard contexts of the encoders alive at once in archiving many files of a batch: those
//! of the files being made, not one for each file made (a context takes up to 4 MiB, outside the
//! Rust heap; 16,421 small files of a batch once took 10 GB). (A test binary of its own: the
//! count is the process's.)

use std::io::Write;

use aqr_compress::{Input, Options, compress_tree_to};
use aqr_formula::aqr::{Entry, EntryNode, Extra, Tree, contexts, reset_contexts_peak};

fn gzip(seed: usize) -> Vec<u8> {
    let mut t = Vec::new();
    for i in 0..400 {
        writeln!(t, "line {i} of file {seed}: the quick brown fox jumps over {} lazy dogs", (i * seed) % 17).unwrap();
    }
    let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(&t).unwrap();
    z.finish().unwrap()
}

#[test]
fn contexts_of_files_being_made() {
    const FILES: usize = 64;
    let files: Vec<(String, Vec<u8>)> = (0..FILES).map(|i| (format!("f{i}.gz"), gzip(i))).collect();
    let mut tree = Tree::default();
    for (c, (name, _)) in files.iter().enumerate() {
        let e =
            Entry { parent: None, name: name.clone().into_bytes(), node: EntryNode::File(c), extra: Extra::default() };
        tree.entries.push(e);
    }
    let inputs: Vec<Input> = files.iter().map(|(name, d)| Input { name: name.as_bytes(), data: d }).collect();
    let threads = 2;
    let opt = Options { threads, ..Options::default() };
    let mut out = Vec::new();
    reset_contexts_peak();
    compress_tree_to(&tree, &inputs[..], &opt, &mut out, &|_| {}).unwrap();
    let (alive, peak) = contexts();
    println!("{FILES} files on {threads} threads: {peak} contexts at most, {alive} alive after");
    assert!(peak <= 8 * threads, "{peak} contexts at once for {FILES} files on {threads} threads");
}
