//! What the tests of the writers share: contents held in memory, a sink keeping what is read
//! back, and tree entries.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::Mutex;

use aqr_pack::{Bytes, Contents, Entry, EntryNode, Extra, Tree};
use aqr_unpack::Sink;

pub struct Files(pub Vec<Vec<u8>>);

impl Contents for Files {
    fn size(&self, c: usize) -> u64 {
        self.0[c].len() as u64
    }

    fn read(&self, c: usize) -> std::io::Result<Bytes> {
        Ok(Bytes::from(self.0[c].clone()))
    }
}

#[derive(Default)]
pub struct Kept(pub Mutex<BTreeMap<usize, Vec<u8>>>);

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

pub fn entry(parent: Option<usize>, name: impl AsRef<[u8]>, node: EntryNode, extra: Extra) -> Entry {
    Entry { parent, name: name.as_ref().to_vec(), node, extra }
}
