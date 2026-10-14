//! Typed literals: object content (a whole object, or a delta's inserts) coded through a
//! state machine over the target, so that the ids in trees, commits and tags become
//! references. Bytes a delta copies are fed in too; only literals are coded.

use crate::obj;
use crate::rd::{Rd, Ref, get_ref, put_ref, put_uv};
use crate::table::{Ids, Table};
use crate::{Result, S_BLOB, S_COMMIT, S_TREE, corrupt};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lit {
    Plain,
    Tree,
    Commit,
}

impl Lit {
    pub fn of_type(t: u8) -> Lit {
        match t {
            obj::TREE => Lit::Tree,
            obj::COMMIT | obj::TAG => Lit::Commit,
            _ => Lit::Plain,
        }
    }
    pub fn stream(self) -> usize {
        match self {
            Lit::Plain => S_BLOB,
            Lit::Tree => S_TREE,
            Lit::Commit => S_COMMIT,
        }
    }
}

#[derive(Clone)]
pub struct State {
    pub kind: Lit,
    /// Tree: id bytes still to come (0: in a mode or name).
    id_left: u8,
    /// Commit/tag: past the header.
    body: bool,
    /// Commit/tag: bytes of the current line so far (saturating), the first 8 kept.
    lp: u8,
    lbuf: [u8; 8],
}

impl State {
    pub fn new(kind: Lit) -> Self {
        State { kind, id_left: 0, body: false, lp: 0, lbuf: [0; 8] }
    }

    #[inline]
    fn feed(&mut self, b: u8) {
        match self.kind {
            Lit::Plain => {}
            Lit::Tree => {
                if self.id_left > 0 {
                    self.id_left -= 1;
                } else if b == 0 {
                    self.id_left = 20;
                }
            }
            Lit::Commit => {
                if self.body {
                } else if b == b'\n' {
                    self.body = self.lp == 0;
                    self.lp = 0;
                } else {
                    if (self.lp as usize) < 8 {
                        self.lbuf[self.lp as usize] = b;
                    }
                    self.lp = self.lp.saturating_add(1);
                }
            }
        }
    }

    /// Feeds bytes the target got elsewhere (copied).
    pub fn feed_all(&mut self, s: &[u8]) {
        match self.kind {
            Lit::Plain => {}
            Lit::Tree => {
                let mut i = 0;
                while i < s.len() {
                    if self.id_left > 0 {
                        let k = (self.id_left as usize).min(s.len() - i);
                        self.id_left -= k as u8;
                        i += k;
                    } else {
                        match memchr(0, &s[i..]) {
                            Some(z) => {
                                i += z + 1;
                                self.id_left = 20;
                            }
                            None => break,
                        }
                    }
                }
            }
            Lit::Commit => {
                for &b in s {
                    if self.body {
                        break;
                    }
                    self.feed(b);
                }
            }
        }
    }

    /// The length of an id starting here (20 binary, 40 hex), if `left` bytes of the literal
    /// run can hold it.
    #[inline]
    fn id_here(&self, left: usize) -> Option<usize> {
        match self.kind {
            Lit::Plain => None,
            Lit::Tree => (self.id_left == 20 && left >= 20).then_some(20),
            Lit::Commit => (!self.body
                && left >= 40
                && ((self.lp == 5 && &self.lbuf[..5] == b"tree ")
                    || (self.lp == 7 && (&self.lbuf[..7] == b"parent " || &self.lbuf[..7] == b"object "))))
                .then_some(40),
        }
    }

    /// How many bytes from here can go plain without an id starting (at least 1 unless the
    /// state is at an id).
    #[inline]
    fn plain_span(&self, s: &[u8]) -> usize {
        match self.kind {
            Lit::Plain => s.len(),
            Lit::Tree if self.id_left > 0 => (self.id_left as usize).min(s.len()),
            Lit::Tree => memchr(0, s).map_or(s.len(), |z| z + 1),
            Lit::Commit if self.body => s.len(),
            Lit::Commit => 1,
        }
    }
}

#[inline]
fn memchr(b: u8, s: &[u8]) -> Option<usize> {
    s.iter().position(|&c| c == b)
}

/// Where coded literals go.
pub struct Sinks<'a> {
    pub lit: &'a mut Vec<u8>,
    pub refs: &'a mut Vec<u8>,
}

/// Codes a literal run. Hex references: 0 = not an id (one plain byte follows), 1 = an id
/// not in the table (20 bytes follow), else `zz(j - cur) + 2`.
pub fn enc_run(st: &mut State, run: &[u8], tab: &Table, cur: &mut i64, out: &mut Sinks) {
    let mut i = 0;
    while i < run.len() {
        match st.id_here(run.len() - i) {
            Some(20) => {
                let id = &run[i..i + 20];
                put_ref(out.refs, tab.find(id), id, cur);
                st.feed_all(id);
                i += 20;
            }
            Some(_) => match obj::parse_hex(&run[i..i + 40]) {
                Some(id) => {
                    match tab.find(&id) {
                        Some(j) => {
                            put_uv(out.refs, recons_common::varint::zz(j as i64 - *cur) + 2);
                            *cur = j as i64;
                        }
                        None => {
                            out.refs.push(1);
                            out.refs.extend_from_slice(&id);
                        }
                    }
                    st.lp = st.lp.saturating_add(40);
                    i += 40;
                }
                None => {
                    out.refs.push(0);
                    out.lit.push(run[i]);
                    st.feed(run[i]);
                    i += 1;
                }
            },
            None => {
                let n = st.plain_span(&run[i..]);
                out.lit.extend_from_slice(&run[i..i + n]);
                st.feed_all(&run[i..i + n]);
                i += n;
            }
        }
    }
}

/// Reading side of the literal streams.
pub struct Srcs<'a> {
    pub lit: Rd<'a>,
    pub refs: Rd<'a>,
}

/// Decodes a literal run of `n` bytes onto `out`.
pub fn dec_run(st: &mut State, n: usize, ids: Ids, cur: &mut i64, src: &mut Srcs, out: &mut Vec<u8>) -> Result<()> {
    let end = out.len() + n;
    while out.len() < end {
        let left = end - out.len();
        match st.id_here(left) {
            Some(20) => {
                let id = match get_ref(&mut src.refs, cur, ids.len())? {
                    Ref::Idx(j) => *ids.get(j as usize),
                    Ref::Id(id) => id,
                };
                out.extend_from_slice(&id);
                st.feed_all(&id);
            }
            Some(_) => {
                let v = src.refs.uv()?;
                let id = match v {
                    0 => {
                        let b = src.lit.u8()?;
                        out.push(b);
                        st.feed(b);
                        continue;
                    }
                    1 => src.refs.bytes(20)?.try_into().unwrap(),
                    _ => {
                        let j = *cur + recons_common::varint::unzz(v - 2);
                        if j < 0 || j as usize >= ids.len() {
                            return Err(corrupt());
                        }
                        *cur = j;
                        *ids.get(j as usize)
                    }
                };
                obj::put_hex(&id, out);
                st.lp = st.lp.saturating_add(40);
            }
            None => {
                let rest = &src.lit.d[src.lit.p..];
                let avail = &rest[..left.min(rest.len())];
                if avail.is_empty() {
                    return Err(corrupt());
                }
                let k = st.plain_span(avail);
                out.extend_from_slice(&avail[..k]);
                st.feed_all(&avail[..k]);
                src.lit.p += k;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obj::Id;

    fn roundtrip(kind: Lit, data: &[u8], tab: &Table) -> (usize, usize) {
        let (mut lit, mut refs) = (vec![], vec![]);
        let mut st = State::new(kind);
        let mut cur = 0;
        // Split into runs at arbitrary points, the middle part "copied".
        let (a, b) = (data.len() / 3, data.len() * 2 / 3);
        enc_run(&mut st, &data[..a], tab, &mut cur, &mut Sinks { lit: &mut lit, refs: &mut refs });
        st.feed_all(&data[a..b]);
        enc_run(&mut st, &data[b..], tab, &mut cur, &mut Sinks { lit: &mut lit, refs: &mut refs });
        let mut st = State::new(kind);
        let mut cur = 0;
        let mut src = Srcs { lit: Rd::new(&lit), refs: Rd::new(&refs) };
        let mut out = vec![];
        dec_run(&mut st, a, Ids(&tab.ids), &mut cur, &mut src, &mut out).unwrap();
        out.extend_from_slice(&data[a..b]);
        st.feed_all(&data[a..b]);
        dec_run(&mut st, data.len() - b, Ids(&tab.ids), &mut cur, &mut src, &mut out).unwrap();
        assert_eq!(out, data);
        assert!(src.lit.at_end() && src.refs.at_end());
        (lit.len(), refs.len())
    }

    #[test]
    fn tree_and_commit() {
        let mut tab = Table::new();
        let ids: Vec<Id> = (0..10u8).map(|i| obj::sha1(&[&[i]])).collect();
        for id in &ids[..8] {
            tab.add(id);
        }
        let mut tree = vec![];
        for (k, id) in ids.iter().enumerate() {
            tree.extend_from_slice(format!("100644 file{k}\0").as_bytes());
            tree.extend_from_slice(id);
        }
        let (lit, _) = roundtrip(Lit::Tree, &tree, &tab);
        assert!(lit < tree.len() - 100);
        let commit = format!(
            "tree {}\nparent {}\nparent {}\nparent {}xyz\nauthor A <a> 1 +0000\n\ntree {}\n",
            obj::hex(&ids[0]),
            obj::hex(&ids[9]),
            "G".repeat(40),
            obj::hex(&ids[1]),
            obj::hex(&ids[2])
        );
        roundtrip(Lit::Commit, commit.as_bytes(), &tab);
        roundtrip(Lit::Commit, b"tree 123\n", &tab);
        roundtrip(Lit::Tree, b"40000 x\0abc", &tab);
    }
}
