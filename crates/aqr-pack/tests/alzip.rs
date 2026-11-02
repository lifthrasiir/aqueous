//! Trees written as ALZ and EGG archives, read back by aqr-unpack: the entries, contents and
//! times kept (ALZip's own programs are not at hand, so compatibility with them is untested).

use std::collections::BTreeMap;
use std::sync::Mutex;

use aqr_pack::alzip::{write_alz, write_egg};
use aqr_pack::{Bytes, Contents, Entry, EntryNode, Extra, Options, Progress, Time, Tree, Written};
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
/// modification time.
fn listed(t: &Tree, contents: &[Vec<u8>]) -> BTreeMap<String, (String, Option<Time>)> {
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
                n => format!("{n:?}"),
            };
            (path(i), (what, e.extra.mtime))
        })
        .collect()
}

type Writer = fn(&Tree, &dyn Contents, &Options, &mut dyn std::io::Write, &aqr_pack::Tell) -> std::io::Result<Written>;

const KINDS: [(Format, Writer); 2] = [(Format::Alz, write_alz), (Format::Egg, write_egg)];

struct Sample {
    tree: Tree,
    files: Files,
    text: usize,
    noise: usize,
}

fn sample(secs: i64) -> Sample {
    let t0 = Time { secs, nanos: 0 };
    let x = Extra { mtime: Some(t0), ..Extra::default() };
    let text: Vec<u8> = (0..5000).flat_map(|i| format!("line {i}\n").into_bytes()).collect();
    let noise: Vec<u8> = (0..3000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    // (past 64 KiB: sizes of 4 bytes in ALZ)
    let big: Vec<u8> = (0..70_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 11) as u8).collect();
    let (text_len, noise_len) = (text.len(), noise.len() + big.len());
    let files = Files(vec![text, noise, Vec::new(), b"tiny".to_vec(), big]);
    let tree = Tree {
        root: Extra::default(),
        entries: vec![
            entry(None, "dir", EntryNode::Dir, x.clone()),
            entry(Some(0), "text.txt", EntryNode::File(0), x.clone()),
            entry(Some(0), "한글 이름.bin", EntryNode::File(1), x.clone()),
            entry(None, "empty", EntryNode::File(2), x.clone()),
            entry(Some(0), "link", EntryNode::Link(b"text.txt".to_vec()), x.clone()),
            entry(None, "same", EntryNode::File(0), x.clone()),
            entry(None, "fifo", EntryNode::Fifo, x.clone()),
            entry(Some(0), "sub", EntryNode::Dir, x.clone()),
            entry(Some(7), "깊은 디렉터리", EntryNode::Dir, x.clone()),
            entry(Some(8), "tiny", EntryNode::File(3), x.clone()),
            entry(None, "big", EntryNode::File(4), x),
        ],
    };
    Sample { tree, files, text: text_len, noise: noise_len }
}

/// Written with `threads` threads at `level`, read back with the same.
fn round_trip(format: Format, write: Writer, s: &Sample, threads: usize, level: Option<u8>) -> Vec<u8> {
    let steps = Mutex::new(Vec::new());
    let mut out = Vec::new();
    let o = Options { level, threads, batch: 4000, ..Options::default() };
    let w = write(&s.tree, &s.files, &o, &mut out, &|p| steps.lock().unwrap().push(p)).unwrap();
    assert_eq!(w.len, out.len() as u64);
    let total = (2 * s.text + s.noise + 4) as u64;
    assert_eq!(w.read, total);
    let name = if format == Format::Alz { "ALZ" } else { "EGG" };
    assert!(
        w.notes.contains(&format!("link: a symbolic link, not kept in {name}")) || w.notes.len() == 2,
        "{:?}",
        w.notes
    );
    assert!(w.notes.iter().any(|n| n == &format!("fifo: a FIFO or device, not kept in {name}")), "{:?}", w.notes);
    let steps = steps.into_inner().unwrap();
    assert_eq!(steps.last(), Some(&Progress::Done { done: total, total }));
    if level != Some(0) {
        // (text deflated, noise stored)
        assert!(out.len() < 2 * s.text / 2 + s.noise + 1000, "{}", out.len());
    } else {
        assert!(out.len() > 2 * s.text + s.noise);
    }

    let opened = Opened::open(format, &out).unwrap();
    let kept = Kept::default();
    let done = opened.unpack(threads, &kept).unwrap();
    let contents: Vec<Vec<u8>> = kept.0.into_inner().unwrap().into_values().collect();
    let mut want = listed(&s.tree, &s.files.0);
    // (neither format stores a link nor a FIFO; the "link" is under "dir/")
    want.remove("fifo");
    want.remove("dir/link");
    // (the reader's contents are numbered in the archive's order: "same" repeats "dir/text.txt")
    assert_eq!(listed(&done.tree, &contents), want);
    out
}

#[test]
fn round_trip_both() {
    for (format, write) in KINDS {
        // (even seconds, and ALZ's local time round trips outside the hour a clock change repeats)
        let s = sample(1_700_000_000);
        for threads in [1, 4] {
            for level in [None, Some(0), Some(9)] {
                round_trip(format, write, &s, threads, level);
            }
        }
    }
}

/// ALZ names are CP949, where all can be; EGG's UTF-8.
#[test]
fn name_encodings() {
    let s = sample(1_700_000_000);
    let hangul = "한글".as_bytes();
    let cp949 = [0xc7, 0xd1, 0xb1, 0xdb];
    let has = |d: &[u8], n: &[u8]| d.windows(n.len()).any(|w| w == n);
    let alz = round_trip(Format::Alz, write_alz, &s, 2, None);
    assert!(has(&alz, &cp949) && !has(&alz, hangul));
    let egg = round_trip(Format::Egg, write_egg, &s, 2, None);
    assert!(has(&egg, "한글".as_bytes()));

    // a name outside CP949: all names UTF-8 (told)
    let mut t = s.tree.clone();
    t.entries.push(entry(None, "🙂.txt", EntryNode::File(3), Extra::default()));
    let mut out = Vec::new();
    let w = write_alz(&t, &s.files, &Options::default(), &mut out, &|_| {}).unwrap();
    assert!(w.notes.iter().any(|n| n.contains("CP949")), "{:?}", w.notes);
    assert!(has(&out, hangul) && !has(&out, &cp949));
    let opened = Opened::open(Format::Alz, &out).unwrap();
    let done = opened.unpack(1, &Kept::default()).unwrap();
    assert!(done.tree.entries.iter().any(|e| e.name == "🙂.txt".as_bytes()));
    assert!(done.tree.entries.iter().any(|e| e.name == "한글 이름.bin".as_bytes()));
}

/// EGG keeps the time to 100 ns; entries without a time have none.
#[test]
fn egg_times() {
    let t0 = Time { secs: 1_700_000_001, nanos: 123_456_700 };
    let tree = Tree {
        root: Extra::default(),
        entries: vec![
            entry(None, "a", EntryNode::File(0), Extra { mtime: Some(t0), ..Extra::default() }),
            entry(None, "b", EntryNode::File(0), Extra::default()),
        ],
    };
    let files = Files(vec![b"x".to_vec()]);
    let mut out = Vec::new();
    write_egg(&tree, &files, &Options::default(), &mut out, &|_| {}).unwrap();
    let done = Opened::open(Format::Egg, &out).unwrap().unpack(1, &Kept::default()).unwrap();
    assert_eq!(done.tree.entries[0].extra.mtime, Some(t0));
    assert_eq!(done.tree.entries[1].extra.mtime, None);
}

#[test]
fn passphrase_refused() {
    let s = sample(1_700_000_000);
    let o = Options { passphrase: Some(b"x".to_vec()), ..Options::default() };
    for (_, write) in KINDS {
        assert!(write(&s.tree, &s.files, &o, &mut Vec::new(), &|_| {}).is_err());
    }
}

/// Speed over 48 files of 1 MiB (`--ignored --nocapture`): threads 1 and 10.
#[test]
#[ignore]
fn speed() {
    let files: Vec<Vec<u8>> = (0..48u32)
        .map(|k| {
            let mut x = k + 1;
            (0..1 << 20)
                .map(|i: u32| {
                    if i % 7 < 4 {
                        (i / 64) as u8
                    } else {
                        x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                        (x >> 24) as u8
                    }
                })
                .collect()
        })
        .collect();
    let entries = (0..48).map(|k| entry(None, &format!("f{k}"), EntryNode::File(k), Extra::default())).collect();
    let tree = Tree { root: Extra::default(), entries };
    let files = Files(files);
    for (name, write) in [("alz", write_alz as Writer), ("egg", write_egg)] {
        for threads in [1, 10] {
            let t = std::time::Instant::now();
            let o = Options { threads, ..Options::default() };
            let w = write(&tree, &files, &o, &mut std::io::sink(), &|_| {}).unwrap();
            eprintln!("{name} threads {threads}: {:?}, {} -> {}", t.elapsed(), w.read, w.len);
        }
    }
}
