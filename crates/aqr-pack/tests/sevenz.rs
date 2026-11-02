//! Trees written as 7z archives, read back by aqr-unpack (and by 7-Zip's `7zz` where it is at
//! hand): the entries, contents and metadata kept, in parts, stored, and encrypted.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use aqr_pack::sevenz::write;
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

/// The tree's entries by path: what each is (its content's head and length for a file), and its
/// times and mode.
type Listed = BTreeMap<String, (String, [Option<Time>; 3], Option<u32>)>;

fn listed(t: &Tree, contents: &[Vec<u8>]) -> Listed {
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
            (path(i), (what, [e.extra.mtime, e.extra.atime, e.extra.crtime], e.extra.mode))
        })
        .collect()
}

/// Pseudo-random bytes, text-like (repeats) for the first half, noise for the rest.
fn data(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761) | 1;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x
    };
    let words: Vec<Vec<u8>> =
        (0..64).map(|_| (0..3 + next() % 6).map(|_| b'a' + (next() % 26) as u8).collect()).collect();
    let mut v = Vec::with_capacity(n + 16);
    while v.len() < n / 2 {
        v.extend(&words[(next() % 64) as usize]);
        v.push(b' ');
    }
    while v.len() < n {
        v.push((next() >> 7) as u8);
    }
    v.truncate(n);
    v
}

fn t0() -> Time {
    Time { secs: 1_700_000_000, nanos: 123_456_700 }
}

fn sample(sizes: &[usize]) -> (Tree, Files) {
    let x = |mode: Option<u32>| Extra {
        mtime: Some(t0()),
        atime: Some(Time { secs: 1_700_000_100, nanos: 0 }),
        crtime: Some(Time { secs: 1_600_000_000, nanos: 500 }),
        mode,
        ..Extra::default()
    };
    let mut contents: Vec<Vec<u8>> = sizes.iter().enumerate().map(|(i, &n)| data(n, i as u32 + 1)).collect();
    let mut entries = vec![entry(None, "dir", EntryNode::Dir, x(Some(0o755)))];
    for i in 0..contents.len() {
        let parent = if i % 2 == 0 { Some(0) } else { None };
        entries.push(entry(
            parent,
            &format!("file{i} 한글.bin"),
            EntryNode::File(i),
            x(Some(if i % 3 == 0 { 0o644 } else { 0o600 })),
        ));
    }
    contents.push(Vec::new());
    entries.push(entry(None, "empty", EntryNode::File(contents.len() - 1), x(Some(0o600))));
    entries.push(entry(Some(0), "link", EntryNode::Link("file0 한글.bin".as_bytes().to_vec()), x(None)));
    entries.push(entry(None, "emptydir", EntryNode::Dir, Extra::default()));
    entries.push(entry(None, "fifo", EntryNode::Fifo, Extra::default()));
    (Tree { root: Extra::default(), entries }, Files(contents))
}

fn archive(tree: &Tree, files: &dyn Contents, o: &Options) -> (Vec<u8>, Vec<Progress>, aqr_pack::Written) {
    let steps = Mutex::new(Vec::new());
    let mut out = Cursor::new(Vec::new());
    let w = write(tree, files, o, &mut out, &|p| steps.lock().unwrap().push(p)).unwrap();
    (out.into_inner(), steps.into_inner().unwrap(), w)
}

/// `tree` and `files` written with `o` and read back (with `pass`), the entries and contents
/// the same but for the FIFO; the archive returned.
fn round_trip(tree: &Tree, files: &Files, o: &Options) -> Vec<u8> {
    let (out, steps, w) = archive(tree, files, o);
    assert_eq!(w.len, out.len() as u64);
    assert!(w.notes.iter().all(|n| n.starts_with("fifo")), "{:?}", w.notes);
    let total: u64 = files.0.iter().map(|f| f.len() as u64).sum::<u64>();
    // (a link's target is data too)
    let links: u64 =
        tree.entries.iter().map(|e| if let EntryNode::Link(t) = &e.node { t.len() as u64 } else { 0 }).sum();
    assert_eq!(steps.last(), Some(&Progress::Done { done: total + links, total: total + links }));
    let mut opened = Opened::open(Format::SevenZip, &out).unwrap();
    if let Some(p) = &o.passphrase {
        assert!(opened.encrypted());
        let wrong = Opened::open(Format::SevenZip, &out).unwrap().unlock(b"wrong").unwrap_or(false);
        assert!(!wrong, "a wrong passphrase");
        assert!(opened.unlock(p).unwrap());
    }
    let kept = Kept::default();
    let done = opened.unpack(o.threads, &kept).unwrap();
    assert_eq!(done.skipped, 0);
    let kept = kept.0.into_inner().unwrap();
    // (the contents are numbered as the archive lists them: compared by path)
    let mut got = vec![Vec::new(); done.tree.entries.len()];
    kept.into_iter().for_each(|(c, d)| got[c] = d);
    let mut want = listed(tree, &files.0);
    want.remove("fifo");
    // (a link has a mode of its own, if the tree gives none)
    want.get_mut("dir/link").into_iter().for_each(|l| l.2 = Some(0o777));
    assert_eq!(listed(&done.tree, &got), want);
    out
}

fn sevenzz() -> Option<&'static str> {
    ["/opt/homebrew/bin/7zz"].into_iter().find(|p| Path::new(p).exists())
}

/// Archive `bytes` tested and listed by 7zz (where it is at hand): its output of `l -slt`.
fn by_7zz(name: &str, bytes: &[u8], pass: Option<&str>) -> Option<String> {
    let z = sevenzz()?;
    let dir = std::env::temp_dir().join(format!("aqr-pack-7z-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.7z");
    std::fs::write(&path, bytes).unwrap();
    let p = format!("-p{}", pass.unwrap_or(""));
    let t = Command::new(z).args(["t", &p]).arg(&path).output().unwrap();
    assert!(t.status.success(), "7zz t: {}{}", String::from_utf8_lossy(&t.stdout), String::from_utf8_lossy(&t.stderr));
    let l = Command::new(z).args(["l", "-slt", &p]).arg(&path).output().unwrap();
    assert!(l.status.success());
    std::fs::remove_dir_all(&dir).ok();
    Some(String::from_utf8_lossy(&l.stdout).into_owned())
}

#[test]
fn basic() {
    let (tree, files) = sample(&[5000, 3000, 1, 200_000]);
    let o = Options { threads: 2, batch: 4000, ..Options::default() };
    let out = round_trip(&tree, &files, &o);
    if let Some(l) = by_7zz("basic", &out, None) {
        assert!(l.contains("Method = LZMA2"), "{l}");
        assert!(l.contains("Path = dir/link\n"), "{l}");
        // (the times in the local zone)
        assert!(l.contains(".1234567\n"), "{l}");
        assert!(l.contains(".0000005\n"), "{l}");
        assert!(l.contains("Attributes = D drwxr-xr-x"), "{l}");
        assert!(l.contains("Attributes =  -rw-r--r--"), "{l}");
        assert!(l.contains("Attributes =  lrwxrwxrwx"), "{l}");
        assert!(l.contains("Attributes = D\n"), "{l}");
        eprintln!("7zz: ok");
    }
}

/// Several LZMA2 parts: the data is larger than the parts' 1 MiB, on several threads.
#[test]
fn parts() {
    let (tree, files) = sample(&[3 << 20, 700_000, 2 << 20, 1 << 20]);
    for threads in [1, 4] {
        let o = Options { threads, batch: 3 << 20, ..Options::default() };
        let out = round_trip(&tree, &files, &o);
        by_7zz("parts", &out, None);
    }
}

#[test]
fn stored() {
    let (tree, files) = sample(&[5000, 70_000]);
    let o = Options { level: Some(0), threads: 3, ..Options::default() };
    let out = round_trip(&tree, &files, &o);
    assert!(out.len() > 75_000);
    if let Some(l) = by_7zz("stored", &out, None) {
        assert!(l.contains("Method = Copy"), "{l}");
    }
}

#[test]
fn levels() {
    let (tree, files) = sample(&[100_000, 30_000]);
    let mut sizes = Vec::new();
    for level in [1, 9] {
        let o = Options { level: Some(level), threads: 2, ..Options::default() };
        sizes.push(round_trip(&tree, &files, &o).len());
    }
    eprintln!("{sizes:?}");
}

#[test]
fn empty_and_directories() {
    let none = Tree { root: Extra::default(), entries: Vec::new() };
    let out = round_trip(&none, &Files(Vec::new()), &Options::default());
    by_7zz("none", &out, None);
    let only = Tree {
        root: Extra::default(),
        entries: vec![
            entry(None, "a", EntryNode::Dir, Extra::default()),
            entry(Some(0), "b", EntryNode::Dir, Extra::default()),
            entry(None, "e", EntryNode::File(0), Extra::default()),
        ],
    };
    let out = round_trip(&only, &Files(vec![Vec::new()]), &Options::default());
    if let Some(l) = by_7zz("dirs", &out, None) {
        assert!(l.contains("Path = a/b\n"), "{l}");
    }
}

#[test]
fn encrypted() {
    let (tree, files) = sample(&[50_000, 3 << 20, 100]);
    for level in [Some(0), None] {
        let o = Options { level, threads: 3, passphrase: Some("pässword".as_bytes().to_vec()), ..Options::default() };
        let out = round_trip(&tree, &files, &o);
        // (the names are not to be seen)
        assert!(!out.windows(4).any(|w| w == b"f\0i\0"));
        if sevenzz().is_some() {
            let l = by_7zz("enc", &out, Some("pässword")).unwrap();
            assert!(l.contains("7zAES"), "{l}");
            // 7zz does not open it with another passphrase
            let z = sevenzz().unwrap();
            let dir = std::env::temp_dir().join(format!("aqr-pack-7z-wrong-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("t.7z");
            std::fs::write(&path, &out).unwrap();
            let r = Command::new(z).args(["t", "-pwrong"]).arg(&path).output().unwrap();
            std::fs::remove_dir_all(&dir).ok();
            assert!(!r.status.success());
        }
    }
}

struct OnDisk(Vec<std::path::PathBuf>);

impl Contents for OnDisk {
    fn size(&self, c: usize) -> u64 {
        std::fs::metadata(&self.0[c]).map_or(0, |m| m.len())
    }

    fn read(&self, c: usize) -> std::io::Result<Bytes> {
        Ok(Bytes::from(std::fs::read(&self.0[c])?))
    }
}

fn gather(dir: &Path, parent: Option<usize>, t: &mut Vec<Entry>, files: &mut Vec<std::path::PathBuf>) {
    let mut names: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    names.sort();
    for p in names {
        let m = std::fs::symlink_metadata(&p).unwrap();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if m.is_dir() {
            t.push(entry(parent, &name, EntryNode::Dir, Extra::default()));
            gather(&p, Some(t.len() - 1), t, files);
        } else if m.is_file() {
            files.push(p);
            t.push(entry(parent, &name, EntryNode::File(files.len() - 1), Extra::default()));
        }
    }
}

/// A directory (`AQR_7Z_DIR`) written with 1 and 10 threads: sizes and seconds.
#[test]
#[ignore]
fn timing() {
    let dir = std::env::var("AQR_7Z_DIR").expect("AQR_7Z_DIR");
    let (mut entries, mut paths) = (Vec::new(), Vec::new());
    gather(Path::new(&dir), None, &mut entries, &mut paths);
    let files = OnDisk(paths);
    let tree = Tree { root: Extra::default(), entries };
    for threads in [1, 10] {
        let o = Options { threads, ..Options::default() };
        let t = std::time::Instant::now();
        let (out, _, w) = archive(&tree, &files, &o);
        eprintln!("threads {threads}: {} -> {} bytes in {:.2} s", w.read, out.len(), t.elapsed().as_secs_f64());
        by_7zz("timing", &out, None);
    }
}
