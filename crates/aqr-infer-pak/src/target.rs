//! What a directory's pointers point into: a file of the family, or one decoded in place
//! (its members decoded, the bytes before and between them kept unless they are zeros).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::codec::{self, Codec};

pub struct Part {
    pub len: u64,
    /// Where it comes from in the file.
    pub at: usize,
    pub src_len: usize,
    pub codec: Option<Codec>,
}

pub enum Kind<'a> {
    File,
    Decoded {
        file: usize,
        parts: Vec<Part>,
        starts: Vec<u64>,
        cache: RefCell<HashMap<usize, Rc<Vec<u8>>>>,
        src: &'a [u8],
    },
}

pub struct Target<'a> {
    pub name: String,
    pub len: u64,
    /// The file it is or comes from (by index in the family).
    pub file: usize,
    /// The directory whose members, decoded in place, it is (by index among those found).
    pub parent: Option<usize>,
    pub kind: Kind<'a>,
    data: &'a [u8],
}

impl<'a> Target<'a> {
    pub fn file(name: &str, index: usize, data: &'a [u8]) -> Target<'a> {
        Target { name: name.to_string(), len: data.len() as u64, file: index, parent: None, kind: Kind::File, data }
    }

    pub fn decoded(name: String, file: usize, src: &'a [u8], parts: Vec<Part>) -> Target<'a> {
        let mut starts = Vec::with_capacity(parts.len() + 1);
        let mut s = 0;
        for p in &parts {
            starts.push(s);
            s += p.len;
        }
        starts.push(s);
        Target {
            name,
            len: s,
            file,
            parent: None,
            kind: Kind::Decoded { file, parts, starts, cache: RefCell::new(HashMap::new()), src },
            data: &[],
        }
    }

    pub fn with_parent(self, parent: usize) -> Target<'a> {
        Target { parent: Some(parent), ..self }
    }

    pub fn is_file(&self) -> bool {
        matches!(self.kind, Kind::File)
    }

    /// Bytes [o, o + n) (fewer at the end).
    pub fn read(&self, o: u64, n: u64) -> Vec<u8> {
        match &self.kind {
            Kind::File => {
                let a = (o as usize).min(self.data.len());
                let z = (o.saturating_add(n) as usize).min(self.data.len());
                self.data[a..z].to_vec()
            }
            Kind::Decoded { parts, starts, cache, src, .. } => {
                let mut out = Vec::with_capacity(n as usize);
                let mut i = starts.partition_point(|&s| s <= o).saturating_sub(1);
                let (mut o, end) = (o, o.saturating_add(n));
                while o < end && i < parts.len() {
                    let p = &parts[i];
                    let chunk: Rc<Vec<u8>> = {
                        let mut c = cache.borrow_mut();
                        if let Some(x) = c.get(&i) {
                            x.clone()
                        } else {
                            let raw = &src[p.at..p.at + p.src_len];
                            let d = match p.codec {
                                Some(k) => codec::decode(k, raw).unwrap_or_default(),
                                None => raw.to_vec(),
                            };
                            if c.len() >= 64 {
                                c.clear();
                            }
                            let d = Rc::new(d);
                            c.insert(i, d.clone());
                            d
                        }
                    };
                    let a = (o - starts[i]) as usize;
                    let z = ((end - starts[i]) as usize).min(chunk.len());
                    if a < z {
                        out.extend_from_slice(&chunk[a..z]);
                    }
                    o = starts[i + 1];
                    i += 1;
                }
                out
            }
        }
    }
}
