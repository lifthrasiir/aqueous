//! Names of an archive taken as one by a file system taking names case aside (macOS's APFS by
//! default) through `aqr -d`: the metadata of what was made applied to that alone, not to what a
//! link made over it by a name of another case leads to (spec, "Extraction").
#![cfg(target_os = "macos")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use aqr_compress::{Input, Options, compress_tree};
use aqr_formula::aqr::{Entry, EntryNode, Extra, Tree};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-names-{name}-{}", std::process::id()));
    if d.exists() {
        fs::remove_dir_all(&d).unwrap();
    }
    fs::create_dir_all(&d).unwrap();
    d
}

fn entry(name: &str, node: EntryNode, mode: Option<u32>) -> Entry {
    Entry { parent: None, name: name.into(), node, extra: Extra { mode, ..Extra::default() } }
}

/// Whether `d` takes names case aside.
fn case_insensitive(d: &std::path::Path) -> bool {
    fs::write(d.join("probe"), b"").unwrap();
    let r = d.join("PROBE").exists();
    fs::remove_file(d.join("probe")).unwrap();
    r
}

fn extract(d: &std::path::Path, tree: &Tree, force: bool) -> (PathBuf, String) {
    let contents = [b"content".to_vec()];
    let inputs: Vec<Input> = contents.iter().map(|c| Input { name: b"c", data: c }).collect();
    let archive = d.join("a.aqr");
    fs::write(&archive, compress_tree(tree, &inputs, &Options::default()).archive).unwrap();
    let out = d.join("out");
    let mut c = Command::new(env!("CARGO_BIN_EXE_aqr"));
    c.args(["-d".as_ref(), "-o".as_ref(), out.as_os_str(), archive.as_os_str()]);
    if force {
        c.arg("-f");
    }
    let o = c.output().unwrap();
    (out, String::from_utf8_lossy(&o.stderr).into_owned())
}

/// With `-f`: file `b` written over by link `B` (`a/../victim`: `a` is `A` here, a link to `.`,
/// so out of the directory), and the mode of `b` then applied to what `B` leads to.
#[test]
fn mode_through_other_case() {
    let d = scratch("case-mode");
    if !case_insensitive(&d) {
        eprintln!("{} takes names by case: not tested", d.display());
        return;
    }
    let tree = Tree {
        root: Extra::default(),
        entries: vec![
            {
                // (of a fork: its metadata applied at the end)
                let mut b = entry("b", EntryNode::File(0), Some(0o777));
                b.extra.forks = vec![(b"user.x".to_vec(), 0)];
                b
            },
            entry("A", EntryNode::Link(".".into()), None),
            entry("B", EntryNode::Link("a/../victim".into()), None),
        ],
    };
    fs::write(d.join("victim"), b"outside").unwrap();
    fs::set_permissions(d.join("victim"), fs::Permissions::from_mode(0o600)).unwrap();
    let (_, err) = extract(&d, &tree, true);
    let mode = fs::metadata(d.join("victim")).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode, 0o600, "{err}");
    fs::remove_dir_all(&d).unwrap();
}
