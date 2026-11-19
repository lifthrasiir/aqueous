//! The stub run with an archive appended to it (no bundle: an archive needing no module), with
//! bytes after the archive (an Authenticode signature, an installer's) or cut short.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aqr_archive::{Encoder, Entry, EntryNode, Extra, Leaf, Level, Segment, Tree};
use aqr_formula::Formula;

/// An archive of `a.txt` and `sub/b.bin`, their bytes stored.
fn archive() -> (Vec<u8>, [(&'static str, Vec<u8>); 2]) {
    let a = b"text of the file, kept as it is\n".repeat(40);
    let b: Vec<u8> = (0..3000u32).map(|i| (i * 31 % 251) as u8).collect();
    let mut f = Formula::default();
    let (na, nb) = (f.push("a", a.clone()), f.push("b", b.clone()));
    let leaves = [Leaf { outputs: vec![na], sums: vec![None] }, Leaf { outputs: vec![nb], sums: vec![None] }];
    let e = |parent, name: &str, node| Entry { parent, name: name.into(), node, extra: Extra::default() };
    let tree = Tree {
        entries: vec![e(None, "a.txt", EntryNode::File(0)), e(None, "sub", EntryNode::Dir), e(Some(1), "b.bin", EntryNode::File(1))],
        ..Tree::default()
    };
    let seg = Segment { f: &f, leaves: &leaves, tree: &tree, contents: &[(0, 0), (1, 0)] };
    let (c, _) = Encoder::new(Level::STORED).encode_segment(&seg).unwrap();
    (c, [("a.txt", a), ("sub/b.bin", b)])
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-sfx-shim-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The stub with `tail` appended, run in `d` to write under `d/out`.
fn run(d: &Path, tail: &[u8]) -> Output {
    let mut exe = std::fs::read(env!("CARGO_BIN_EXE_aqr-sfx-shim")).unwrap();
    exe.extend_from_slice(tail);
    let path = d.join(if cfg!(windows) { "x.exe" } else { "x" });
    std::fs::write(&path, exe).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    Command::new(&path).current_dir(d).arg("out").output().unwrap()
}

#[test]
fn archive_appended() {
    let (c, files) = archive();
    // as it is, and with bytes after it: a signature of a certificate's size, a few bytes of
    // padding, bytes that look like a footer's end
    for (name, after) in [("plain", vec![]), ("signed", vec![0x30; 4096]), ("padded", vec![0; 7]), ("odd", vec![0xff, 0x7f])]
    {
        let d = scratch(name);
        let o = run(&d, &[&c[..], &after].concat());
        assert!(o.status.success(), "{name}: {}", String::from_utf8_lossy(&o.stderr));
        for (f, want) in &files {
            assert!(std::fs::read(d.join("out").join(f)).unwrap() == *want, "{name}: {f}");
        }
        std::fs::remove_dir_all(&d).unwrap();
    }
}

#[test]
fn archive_cut_short() {
    let (c, _) = archive();
    for cut in [c.len() - 1, c.len() / 2, 7] {
        let d = scratch(&format!("cut{cut}"));
        let o = run(&d, &c[..cut]);
        assert!(!o.status.success(), "cut at {cut}");
        assert!(!d.join("out").join("a.txt").exists(), "cut at {cut}: a file written");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
