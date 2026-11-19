//! Archives past 4 GiB (offsets, the central directory's place, sizes in 32 bits no more), of
//! files of repeated bytes made as they are read, stored (level 0) so that the output is as big
//! as the input. Written to a file under the scratch directory and read back through a map.
//! Ignored: each takes about a minute and 4.5 GiB of disk.

use std::io::{self, BufWriter};
use std::sync::Mutex;

use aqr_pack::{Bytes, Contents, EntryNode, Extra, Format, Options, Tree};
use aqr_unpack::Sink;

mod common;
use common::entry;

const FILE: usize = 256 << 20;
const FILES: usize = 18;

/// File `c` is `FILE` bytes of one value, made on each read.
struct Repeated;

fn value(c: usize) -> u8 {
    (c % 251 + 1) as u8
}

impl Contents for Repeated {
    fn size(&self, _: usize) -> u64 {
        FILE as u64
    }

    fn read(&self, c: usize) -> io::Result<Bytes> {
        Ok(Bytes::from(vec![value(c); FILE]))
    }
}

/// Checks each content to be its value, whole, and drops it.
#[derive(Default)]
struct Checked(Mutex<Vec<usize>>);

impl Sink for Checked {
    fn entries(&self, _: &Tree, _: &[usize]) -> Result<(), String> {
        Ok(())
    }

    fn content(&self, c: usize, d: Vec<u8>) -> Result<(), String> {
        if d.len() != FILE || d.iter().any(|&b| b != value(c)) {
            return Err(format!("content {c} differs ({} bytes)", d.len()));
        }
        self.0.lock().unwrap().push(c);
        Ok(())
    }

    fn note(&self, n: &str) {
        panic!("{n}");
    }
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(r"F:\Scratch").join(format!("aqr-pack-huge-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn huge(f: Format, read: aqr_unpack::Format, name: &str) {
    let tree = Tree {
        root: Extra::default(),
        entries: (0..FILES).map(|i| entry(None, format!("f{i}"), EntryNode::File(i), Extra::default())).collect(),
    };
    let path = scratch(name);
    let o = Options { level: Some(0), threads: 2, batch: 512 << 20, ..Options::default() };
    let file = std::fs::File::create(&path).unwrap();
    let mut w = BufWriter::with_capacity(1 << 20, file);
    let r = aqr_pack::write(f, &tree, &Repeated, &o, &mut w, &|_| {});
    drop(w);
    let result = r.map(|w| w.len).map_err(|e| e.to_string()).and_then(|len| {
        let file = std::fs::File::open(&path).unwrap();
        assert_eq!(file.metadata().unwrap().len(), len);
        assert!(len > 1 << 32, "{len}");
        let map = unsafe { memmap2::Mmap::map(&file) }.unwrap();
        let opened = aqr_unpack::Opened::open(read, &map)?;
        let sink = Checked::default();
        opened.unpack(2, &sink)?;
        let mut got = sink.0.into_inner().unwrap();
        got.sort();
        assert_eq!(got, (0..FILES).collect::<Vec<_>>());
        Ok(())
    });
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    result.unwrap();
}

#[test]
#[ignore = "4.5 GiB of disk, about a minute"]
fn zip_past_4_gib() {
    huge(Format::Zip, aqr_unpack::Format::Zip, "t.zip");
}

#[test]
#[ignore = "4.5 GiB of disk, about a minute"]
fn sevenz_past_4_gib() {
    huge(Format::SevenZip, aqr_unpack::Format::SevenZip, "t.7z");
}
