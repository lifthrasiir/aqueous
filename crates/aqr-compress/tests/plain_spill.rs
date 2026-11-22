//! Below level 2, an archive of several files takes nothing of the spill
//! ([`aqr_compress::Options::spill`]): each batch is read, compressed and written as it is made.
//! (A test binary of its own: it counts the files the process has open.)

use std::io::{self, Write};
use std::path::Path;

use aqr_archive::{Entry, EntryNode, Extra, Level, Tree, Value};
use aqr_compress::{Input, Options};

/// The files of spill segments open: the unlinked files the process has open (Unix); the files
/// in `dir`, as delete on close keeps them there until closed (Windows).
#[cfg(unix)]
fn open_files(_: &Path) -> usize {
    use std::os::unix::fs::MetadataExt;
    let fds = std::fs::read_dir("/dev/fd").unwrap();
    let paths: Vec<_> = fds.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    paths.iter().filter_map(|p| std::fs::metadata(p).ok()).filter(|m| m.is_file() && m.nlink() == 0).count()
}

#[cfg(not(unix))]
fn open_files(dir: &Path) -> usize {
    std::fs::read_dir(dir).unwrap().count()
}

/// The archive written, and the most spill files open as any of it was.
struct Watched<'a> {
    archive: Vec<u8>,
    dir: &'a Path,
    most: usize,
}

impl Write for Watched<'_> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.most = self.most.max(open_files(self.dir));
        self.archive.extend_from_slice(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Lines of text, `n` bytes or so, of `seed`: compressible, unlike any other's.
fn text(n: usize, seed: u64) -> Vec<u8> {
    let mut t = Vec::with_capacity(n + 100);
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    while t.len() < n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        writeln!(t, "{seed} line {}: {} of {}", x % 1000, x >> 40, (x >> 20) % 77).unwrap();
    }
    t
}

/// Files of 24 MiB in all, in batches of 16 MiB: each batch's blocks longer than a stream of a
/// length not told keeps on the heap (`store:untold`), the spill set. At levels 1 and 0, none of
/// the spill taken, and the archive the same as without a spill.
#[test]
fn plain_takes_no_spill() {
    let dir = std::env::temp_dir().join(format!("aqr-compress-plain-spill-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let files: Vec<(String, Vec<u8>)> = (0..6).map(|i| (format!("f{i}.txt"), text(4 << 20, i))).collect();
    let inputs: Vec<Input> = files.iter().map(|(n, d)| Input { name: n.as_bytes(), data: d }).collect();
    let mut tree = Tree { root: Extra::default(), entries: Vec::new() };
    for (c, (name, _)) in files.iter().enumerate() {
        let e =
            Entry { parent: None, name: name.as_bytes().to_vec(), node: EntryNode::File(c), extra: Extra::default() };
        tree.entries.push(e);
    }
    for level in [Level::Of(1), Level::STORED] {
        let opt = Options { level, batch: 16 << 20, ..Options::default() };
        let plain = aqr_compress::compress_tree(&tree, &inputs, &opt).archive;
        let spilling = Options { spill: Some(dir.join(".out")), ..opt };
        let before = open_files(&dir);
        let mut out = Watched { archive: Vec::new(), dir: &dir, most: before };
        aqr_compress::compress_tree_to(&tree, &inputs[..], &spilling, &mut out, &|_| {}).unwrap();
        assert_eq!(out.most, before, "spill files open as the archive was written, at {level:?}");
        assert!(out.archive == plain, "not the same archive with a spill, at {level:?}");
        let Ok(Value::Tree(_, got)) = aqr_decompress::decompress_value(&out.archive, 1) else { panic!("not decoded") };
        assert!(got.iter().zip(&files).all(|(g, f)| g[..] == f.1[..]), "not given back, at {level:?}");
    }
    std::fs::remove_dir(&dir).unwrap();
}
