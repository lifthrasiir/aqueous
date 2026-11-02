//! Trees written as ZIP archives, read back by aqr-unpack (and by Python's `zipfile` where it is
//! at hand): the entries, contents and metadata kept.

use std::collections::BTreeMap;
use std::sync::Mutex;

use aqr_pack::zip::{Options, write};
use aqr_pack::{Bytes, Contents, Entry, EntryNode, Extra, Progress, Time, Tree};
use aqr_unpack::{Format, Opened, Sink};

struct Files(Vec<Vec<u8>>);

impl Contents for Files {
    fn size(&self, c: usize) -> u64 {
        self.0[c].len() as u64
    }

    fn read(&self, c: usize) -> std::io::Result<Bytes> {
        Ok(Bytes::from(self.0[c].clone()))
    }
}

#[derive(Default)]
struct Kept(Mutex<BTreeMap<usize, Vec<u8>>>);

impl Sink for Kept {
    fn entries(&self, _: &Tree, _: &[usize]) -> Result<(), String> {
        Ok(())
    }

    fn content(&self, c: usize, d: Vec<u8>) -> Result<(), String> {
        self.0.lock().unwrap().insert(c, d);
        Ok(())
    }

    fn note(&self, n: &str) {
        panic!("{n}");
    }
}

fn entry(parent: Option<usize>, name: &str, node: EntryNode, extra: Extra) -> Entry {
    Entry { parent, name: name.as_bytes().to_vec(), node, extra }
}

/// The tree's entries by path: what each is (its content's bytes for a file), and its
/// modification time and mode.
fn listed(t: &Tree, contents: &[Vec<u8>]) -> BTreeMap<String, (String, Option<Time>, Option<u32>)> {
    let path = |mut i: usize| {
        let mut names = vec![String::from_utf8_lossy(&t.entries[i].name).into_owned()];
        while let Some(p) = t.entries[i].parent {
            names.push(String::from_utf8_lossy(&t.entries[p].name).into_owned());
            i = p;
        }
        names.reverse();
        names.join("/")
    };
    (t.entries.iter().enumerate())
        .map(|(i, e)| {
            let what = match &e.node {
                EntryNode::Dir => "dir".into(),
                EntryNode::File(c) => {
                    format!("{:?}", &contents[*c][..contents[*c].len().min(16)]) + &contents[*c].len().to_string()
                }
                EntryNode::Link(to) => format!("-> {}", String::from_utf8_lossy(to)),
                n => format!("{n:?}"),
            };
            (path(i), (what, e.extra.mtime, e.extra.mode))
        })
        .collect()
}

#[test]
fn round_trip() {
    let t0 = Time { secs: 1_700_000_000, nanos: 123_456_700 };
    let x = |mode: Option<u32>| Extra { mtime: Some(t0), mode, ..Extra::default() };
    let text: Vec<u8> = (0..5000).flat_map(|i| format!("line {i}\n").into_bytes()).collect();
    let noise: Vec<u8> = (0..3000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    let files = Files(vec![text.clone(), noise.clone(), Vec::new(), b"tiny".to_vec()]);
    let tree = Tree {
        root: Extra::default(),
        entries: vec![
            entry(None, "dir", EntryNode::Dir, x(Some(0o755))),
            entry(Some(0), "text.txt", EntryNode::File(0), x(Some(0o644))),
            entry(Some(0), "한글 이름.bin", EntryNode::File(1), x(None)),
            entry(None, "empty", EntryNode::File(2), x(Some(0o600))),
            entry(Some(0), "link", EntryNode::Link(b"text.txt".to_vec()), x(None)),
            entry(None, "same", EntryNode::File(0), x(Some(0o644))),
            entry(None, "fifo", EntryNode::Fifo, x(None)),
            entry(Some(0), "tiny", EntryNode::File(3), Extra::default()),
        ],
    };
    let steps = Mutex::new(Vec::new());
    let mut out = Vec::new();
    let o = Options { level: 6, threads: 2, batch: 4000 };
    let w = write(&tree, &files, &o, &mut out, &|p| steps.lock().unwrap().push(p)).unwrap();
    assert_eq!(w.len, out.len() as u64);
    assert_eq!(w.read, (2 * text.len() + noise.len() + 4 + 8) as u64);
    assert_eq!(w.notes, ["fifo: a FIFO or device, not kept in ZIP"]);
    let steps = steps.into_inner().unwrap();
    let total = (2 * text.len() + noise.len() + 4) as u64;
    assert_eq!(steps.last(), Some(&Progress::Done { done: total, total }));
    // (text deflated, noise stored)
    assert!(out.len() < text.len() + noise.len(), "{}", out.len());

    let opened = Opened::open(Format::Zip, &out).unwrap();
    let kept = Kept::default();
    let done = opened.unpack(2, &kept).unwrap();
    let contents: Vec<Vec<u8>> = kept.0.into_inner().unwrap().into_values().collect();
    let mut want = listed(&tree, &files.0);
    want.remove("fifo");
    // (no time, as MS-DOS's of 1980; the time to 100 ns, NTFS's; a link's mode its own)
    want.get_mut("dir/tiny").unwrap().1 = done.tree.entries.iter().find(|e| e.name == b"tiny").unwrap().extra.mtime;
    want.get_mut("dir/link").unwrap().2 = Some(0o777);
    assert_eq!(listed(&done.tree, &contents), want);
}

/// Python's `zipfile` reads it too: names, modes and contents (where Python is at hand).
#[test]
fn read_by_python() {
    let files = Files(vec![b"hello\n".repeat(100), (0..10).collect()]);
    let x = Extra { mtime: Some(Time { secs: 1_600_000_000, nanos: 0 }), mode: Some(0o640), ..Extra::default() };
    let tree = Tree {
        root: Extra::default(),
        entries: vec![
            entry(None, "a", EntryNode::Dir, Extra::default()),
            entry(Some(0), "ü.txt", EntryNode::File(0), x.clone()),
            entry(None, "b", EntryNode::File(1), x),
        ],
    };
    let mut out = Vec::new();
    write(&tree, &files, &Options::default(), &mut out, &|_| {}).unwrap();
    let dir = std::env::temp_dir().join(format!("aqr-pack-zip-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.zip");
    std::fs::write(&path, &out).unwrap();
    let script = "import sys, zipfile\n\
                  z = zipfile.ZipFile(sys.argv[1])\n\
                  assert z.testzip() is None\n\
                  for i in z.infolist(): print(i.filename, oct(i.external_attr >> 16), len(z.read(i)), i.compress_type)\n";
    let r = std::process::Command::new("python").args(["-I", "-X", "utf8", "-c", script]).arg(&path).output();
    std::fs::remove_dir_all(&dir).unwrap();
    let Ok(r) = r else { return eprintln!("no Python: not checked") };
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let lines = String::from_utf8(r.stdout).unwrap().replace("\r\n", "\n");
    assert_eq!(lines, "a/ 0o0 0 0\na/ü.txt 0o100640 600 8\nb 0o100640 10 0\n");
}
