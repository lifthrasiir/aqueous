//! Looking up indirect objects by number, without the cross-reference table: an index of
//! every `N G obj` in the file, built on first use (later definitions win, as incremental
//! updates do).

use crate::lex::{Obj, Parser, is_white};
use std::collections::HashMap;

pub struct Objects<'a> {
    file: &'a [u8],
    index: Option<HashMap<u32, usize>>,
}

impl<'a> Objects<'a> {
    pub fn new(file: &'a [u8]) -> Self {
        Objects { file, index: None }
    }

    fn build(file: &[u8]) -> HashMap<u32, usize> {
        let mut m = HashMap::new();
        for i in memchr::memmem::find_iter(file, b"obj") {
            // `N G obj`, with the object starting after `obj`.
            let mut p = i;
            let back = |p: &mut usize, digits: bool| -> Option<usize> {
                let end = *p;
                if digits {
                    while *p > 0 && file[*p - 1].is_ascii_digit() {
                        *p -= 1;
                    }
                    (*p < end).then_some(*p)
                } else {
                    while *p > 0 && is_white(file[*p - 1]) {
                        *p -= 1;
                    }
                    (*p < end).then_some(*p)
                }
            };
            if back(&mut p, false).is_none() {
                continue;
            }
            if back(&mut p, true).is_none() {
                continue;
            }
            if back(&mut p, false).is_none() {
                continue;
            }
            let ne = p;
            let Some(ns) = back(&mut p, true) else { continue };
            if ns > 0 && !is_white(file[ns - 1]) && !crate::lex::is_delim(file[ns - 1]) {
                continue;
            }
            if let Some(n) = crate::lex::parse_int(&file[ns..ne]).and_then(|n| u32::try_from(n).ok()) {
                m.insert(n, i + 3);
            }
        }
        m
    }

    /// Object `n`, if it parses (streams: their dictionary).
    pub fn get(&mut self, n: u32) -> Option<Obj<'a>> {
        let file = self.file;
        let idx = self.index.get_or_insert_with(|| Self::build(file));
        let &at = idx.get(&n)?;
        Parser::new(file, at).object()
    }

    /// `o`, or the object it refers to (once).
    pub fn deref(&mut self, o: &Obj<'a>) -> Option<Obj<'a>> {
        match *o {
            Obj::Ref(n, _) => self.get(n),
            _ => Some(o.clone()),
        }
    }
}
