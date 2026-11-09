//! Trees written as ZIP archives, read back by aqr-unpack (and by Python's `zipfile` where it is
//! at hand): the entries, contents and metadata kept.

use std::collections::BTreeMap;
use std::sync::Mutex;

use aqr_pack::zip::write;
use aqr_pack::{Bytes, Contents, Entry, EntryNode, Extra, Options, Progress, Time, Tree};
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
    let t0 = Time::new(1_700_000_000, 123_456_700);
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
    let o = Options { level: Some(6), threads: 2, batch: 4000, passphrase: None };
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
    let x = Extra { mtime: Some(Time::new(1_600_000_000, 0)), mode: Some(0o640), ..Extra::default() };
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

/// Encrypted by WinZip's AES-256: read back by the passphrase alone (and by 7-Zip, where it is
/// at hand); directories and links are not encrypted.
#[test]
fn aes() {
    let text: Vec<u8> = (0..3000).flat_map(|i| format!("line {i}\n").into_bytes()).collect();
    let files = Files(vec![text.clone(), b"tiny".to_vec(), Vec::new()]);
    let x = Extra { mtime: Some(Time::new(1_600_000_000, 0)), mode: Some(0o644), ..Extra::default() };
    let tree = Tree {
        root: Extra::default(),
        entries: vec![
            entry(None, "d", EntryNode::Dir, Extra::default()),
            entry(Some(0), "text", EntryNode::File(0), x.clone()),
            entry(None, "tiny", EntryNode::File(1), x.clone()),
            entry(None, "empty", EntryNode::File(2), x.clone()),
            entry(None, "link", EntryNode::Link(b"tiny".to_vec()), Extra::default()),
        ],
    };
    let o = Options { threads: 2, passphrase: Some("비밀 pass".as_bytes().to_vec()), ..Options::default() };
    let mut out = Vec::new();
    write(&tree, &files, &o, &mut out, &|_| {}).unwrap();
    // (deflated, then encrypted: no text in the clear)
    assert!(out.len() < text.len() / 2);
    assert!(!out.windows(8).any(|w| w == b"line 100"));

    let mut opened = Opened::open(Format::Zip, &out).unwrap();
    assert!(opened.encrypted());
    assert!(!opened.unlock(b"wrong").unwrap());
    assert!(opened.unlock("비밀 pass".as_bytes()).unwrap());
    let kept = Kept::default();
    let done = opened.unpack(2, &kept).unwrap();
    let contents: Vec<Vec<u8>> = kept.0.into_inner().unwrap().into_values().collect();
    let mut want = listed(&tree, &files.0);
    let mtime = |n: &[u8]| done.tree.entries.iter().find(|e| e.name == n).unwrap().extra.mtime;
    want.get_mut("d").unwrap().1 = mtime(b"d");
    want.get_mut("link").unwrap().1 = mtime(b"link");
    want.get_mut("link").unwrap().2 = Some(0o777);
    assert_eq!(listed(&done.tree, &contents), want);

    let dir = std::env::temp_dir().join(format!("aqr-pack-zip-aes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.zip");
    std::fs::write(&path, &out).unwrap();
    let r = std::process::Command::new("7zz").args(["t", "-p비밀 pass"]).arg(&path).output();
    std::fs::remove_dir_all(&dir).unwrap();
    let Ok(r) = r else { return eprintln!("no 7-Zip: not checked") };
    let said = String::from_utf8_lossy(&r.stdout);
    assert!(r.status.success() && said.contains("Everything is Ok"), "{said}{}", String::from_utf8_lossy(&r.stderr));
}

/// The owner's ids in Info-ZIP's Unix field (0x7875), in the local and central headers both as
/// Info-ZIP's `zip` writes it (version 1, ids of 4 bytes; 8 where larger); none where unknown.
#[test]
fn owner() {
    let files = Files(vec![b"x".to_vec()]);
    let x = |uid, gid| Extra { mode: Some(0o644), uid, gid, ..Extra::default() };
    let tree = Tree {
        root: Extra::default(),
        entries: vec![
            entry(None, "a", EntryNode::File(0), x(Some(501), Some(20))),
            entry(None, "b", EntryNode::File(0), x(Some(1 << 40), Some(7))),
            entry(None, "c", EntryNode::File(0), x(None, None)),
        ],
    };
    let mut out = Vec::new();
    write(&tree, &files, &Options::default(), &mut out, &|_| {}).unwrap();
    let field = [&b"ux\x0b\x00\x01\x04"[..], &501u32.to_le_bytes(), b"\x04", &20u32.to_le_bytes()].concat();
    assert_eq!(out.windows(field.len()).filter(|w| *w == field).count(), 2);
    let done = Opened::open(Format::Zip, &out).unwrap().unpack(1, &Kept::default()).unwrap();
    let ids: Vec<_> = done.tree.entries.iter().map(|e| (e.extra.uid, e.extra.gid)).collect();
    assert_eq!(ids, [(Some(501), Some(20)), (Some(1 << 40), Some(7)), (None, None)]);
}

/// A local time of an unknown time zone (`Zone::Local`) goes into the extended timestamp and
/// NTFS's field as local time here (it was left to MS-DOS's time alone, of 2 seconds): read
/// back, the same local date and time, to 100 ns.
#[test]
fn local_times() {
    use aqr_formula::aqr::Zone;
    let local = Time { secs: 1_700_000_001, nanos: 250_000_000, zone: Zone::Local, unit: 1 };
    let here = aqr_foreign::time::instant(local).unwrap();
    let x = Extra { mtime: Some(local), ..Extra::default() };
    let tree = Tree { root: Extra::default(), entries: vec![entry(None, "a", EntryNode::File(0), x)] };
    let files = Files(vec![b"x".to_vec()]);
    let mut out = Vec::new();
    write(&tree, &files, &Options::default(), &mut out, &|_| {}).unwrap();
    let done = Opened::open(Format::Zip, &out).unwrap().unpack(1, &Kept::default()).unwrap();
    let t = done.tree.entries[0].extra.mtime;
    assert_eq!(t.map(|t| (t.secs, t.nanos)), Some((here.secs, here.nanos)));
}
