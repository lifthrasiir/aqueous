//! Names holding control characters (escape sequences, line feeds) shown escaped: in a listing
//! (`-t -q` a name a line, whatever it holds), in what `-I` tells and on standard error.

mod common;

use std::fs;
use std::process::Command;

use aqr_archive::{Entry, EntryNode, Extra, Tree};
use aqr_compress::{Input, Options, compress_tree};

#[test]
fn names_escaped() {
    let d = common::scratch("terminal", "names");
    let evil = "a\x1b]0;title\x07\nfake.txt";
    let e = |name: &str| Entry {
        parent: None,
        name: name.as_bytes().to_vec(),
        node: EntryNode::File(0),
        extra: Extra::default(),
    };
    let tree = Tree { entries: vec![e(evil), e("\u{202e}txt.exe")], ..Tree::default() };
    let data = b"content".to_vec();
    let archive = d.join("a.aqr");
    let inputs = [Input { name: b"c", data: &data }];
    fs::write(&archive, compress_tree(&tree, &inputs, &Options::default()).archive).unwrap();
    let run = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(&d).args(args).output().unwrap();
        (String::from_utf8(o.stdout).unwrap(), String::from_utf8(o.stderr).unwrap())
    };
    let clean = |s: &str| !s.contains(['\x1b', '\x07', '\u{202e}']);
    let (out, err) = run(&["-t", "-q", "a.aqr"]);
    assert_eq!(
        out,
        r"a\u{1b}]0;title\u{7}\u{a}fake.txt
\u{202e}txt.exe
",
        "{err}"
    );
    let (out, _) = run(&["-t", "a.aqr"]);
    assert!(clean(&out) && out.contains("fake.txt"), "{out}");
    let (out, _) = run(&["-I", "-v", "a.aqr"]);
    assert!(clean(&out), "{out}");
    // (written or not as the system takes the name, what is told escaped)
    let (_, err) = run(&["-d", "-v", "-o", "out", "a.aqr"]);
    assert!(clean(&err), "{err}");
    fs::remove_dir_all(&d).ok();
}
