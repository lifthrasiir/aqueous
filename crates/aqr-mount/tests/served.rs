//! The tree of an archive through `aqr --serve` (the `aqr` built beside the tests: `cargo rt -p aqr
//! -p aqr-mount`): directories looked in, in any case or not, the entries as made, every file
//! read whole in pieces of odd sizes and at once from several threads.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use aqr_mount::{Case, Error, Kind, ROOT, ServedTree, Tree, lookup_path};

fn aqr() -> PathBuf {
    let dir = std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().to_owned();
    let aqr = dir.join(format!("aqr{}", std::env::consts::EXE_SUFFIX));
    assert!(aqr.is_file(), "{}: not built (cargo rt -p aqr -p aqr-mount)", aqr.display());
    aqr
}

fn text(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed;
    let mut t = Vec::new();
    while t.len() < n {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        t.extend_from_slice(format!("{} ", (x >> 40) % 100_000).as_bytes());
    }
    t.truncate(n);
    t
}

/// An archive of `t/` (its contents at the root): its files by path.
fn archive(d: &Path) -> Vec<(String, Vec<u8>)> {
    let files = vec![
        ("a.txt".to_owned(), text(1, 5000)),
        ("sub/Big.txt".to_owned(), text(2, 3 << 20)),
        ("sub/deep/c.txt".to_owned(), text(3, 700)),
        ("zero".to_owned(), Vec::new()),
    ];
    for (p, b) in &files {
        let p = d.join("t").join(p);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b).unwrap();
    }
    fs::create_dir_all(d.join("t/empty")).unwrap();
    let o = Command::new(aqr()).current_dir(d).args(["-q", "-o", "t.aqr", "t"]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    files
}

#[test]
fn archive_tree() {
    let d = std::env::temp_dir().join(format!("aqr-mount-served-{}", std::process::id()));
    _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    let files = archive(&d);
    let tree = ServedTree::open(&aqr(), &d.join("t.aqr")).unwrap();
    assert_eq!(tree.label(), "t");
    assert_eq!(tree.attr(ROOT).unwrap().kind, Kind::Dir);

    let mut names: Vec<_> = tree.entries(ROOT).unwrap().into_iter().map(|e| e.name).collect();
    names.sort();
    assert_eq!(names, ["a.txt", "empty", "sub", "zero"]);
    let sub = tree.lookup(ROOT, "sub", Case::Sensitive).unwrap();
    let empty = tree.lookup(ROOT, "empty", Case::Sensitive).unwrap();
    assert!(tree.entries(empty).unwrap().is_empty());

    // (in any case where asked; as it is otherwise)
    assert!(matches!(lookup_path(&tree, "sub/big.txt", '/', Case::Sensitive), Err(Error::NotFound)));
    let big = lookup_path(&tree, "\\SUB\\big.TXT", '\\', Case::Insensitive).unwrap();
    assert_eq!(lookup_path(&tree, "sub/Big.txt", '/', Case::Sensitive).unwrap(), big);
    assert!(matches!(lookup_path(&tree, "a.txt/x", '/', Case::Sensitive), Err(Error::NotADirectory)));
    assert!(matches!(tree.read(sub, 0, &mut [0; 4]), Err(Error::IsADirectory)));

    std::thread::scope(|scope| {
        for (path, bytes) in &files {
            let tree = &tree;
            scope.spawn(move || {
                let node = lookup_path(tree, path, '/', Case::Sensitive).unwrap();
                let attr = tree.attr(node).unwrap();
                assert_eq!((attr.kind, attr.size), (Kind::File, bytes.len() as u64), "{path}");
                // (pieces over the protocol's chunk and of odd sizes: each filled but at the end)
                let mut got = Vec::new();
                let mut buf = vec![0; (1 << 20) + 4093];
                loop {
                    let n = tree.read(node, got.len() as u64, &mut buf).unwrap();
                    if n == 0 {
                        break;
                    }
                    assert!(n == buf.len() || got.len() + n == bytes.len(), "{path}: short read");
                    got.extend_from_slice(&buf[..n]);
                }
                assert!(got == *bytes, "{path}");
                assert_eq!(tree.read(node, u64::MAX, &mut buf).unwrap(), 0);
            });
        }
    });
    drop(tree);
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn not_an_archive() {
    let d = std::env::temp_dir().join(format!("aqr-mount-served-no-{}", std::process::id()));
    _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("x.aqr"), b"not an archive").unwrap();
    assert!(matches!(ServedTree::open(&aqr(), &d.join("x.aqr")), Err(Error::Io(_))));
    assert!(matches!(ServedTree::open(&aqr(), &d.join("missing.aqr")), Err(Error::Io(_))));
    fs::remove_dir_all(&d).unwrap();
}
