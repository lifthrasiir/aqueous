//! Other files under `.git/`: runs of exactly 40 lowercase hex digits and 20-byte windows
//! that are ids in the table become references; a trailing SHA-1 of the rest is derived.
//!
//! `entry`: per id varint(gap << 1 | hex), up to the end of the file's part; `text`: the
//! bytes between; `refs`: the references (`cur` from 0).

use crate::obj::{self, is_lhex};
use crate::rd::{Rd, Ref, get_ref, put_ref, put_uv};
use crate::table::{Ids, Table};
use crate::{Result, S_ENTRY, S_REFS, S_TEXT, Streams, corrupt};

pub const F_TRAILER: u8 = 1;

/// Codes `b`; returns the flags.
pub fn encode(b: &[u8], tab: &Table, s: &mut Streams) -> u8 {
    let (body, flags) = if b.len() >= 20 && obj::sha1(&[&b[..b.len() - 20]])[..] == b[b.len() - 20..] {
        (&b[..b.len() - 20], F_TRAILER)
    } else {
        (b, 0)
    };
    let n = body.len();
    let mut last = 0;
    let mut cur = 0i64;
    let mut found = |s: &mut Streams, p: usize, hex: bool, j: u32| {
        put_uv(&mut s[S_ENTRY], ((p - last) as u64) << 1 | hex as u64);
        s[S_TEXT].extend_from_slice(&body[last..p]);
        put_ref(&mut s[S_REFS], Some(j), &[], &mut cur);
        last = p + if hex { 40 } else { 20 };
    };
    let mut i = 0;
    while i + 20 <= n {
        if is_lhex(body[i]) && (i == 0 || !is_lhex(body[i - 1])) {
            let e = i + body[i..].iter().take(41).take_while(|&&c| is_lhex(c)).count();
            if e - i == 40
                && let Some(j) = tab.find(&obj::parse_hex(&body[i..e]).unwrap())
            {
                found(s, i, true, j);
                i = e;
                continue;
            }
        }
        if let Some(j) = tab.find_window(&body[i..i + 20]) {
            found(s, i, false, j);
            i += 20;
            continue;
        }
        i += 1;
    }
    s[S_TEXT].extend_from_slice(&body[last..]);
    flags
}

/// Rebuilds a file from its parts of the streams (`text` exactly its own).
pub fn decode(flags: u8, entry: &mut Rd, text: &[u8], refs: &mut Rd, ids: Ids) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() + 64);
    let mut t = 0usize;
    let mut cur = 0i64;
    while !entry.at_end() {
        let v = entry.uv()?;
        let gap = usize::try_from(v >> 1).map_err(|_| corrupt())?;
        out.extend_from_slice(text.get(t..t.checked_add(gap).ok_or_else(corrupt)?).ok_or_else(corrupt)?);
        t += gap;
        let Ref::Idx(j) = get_ref(refs, &mut cur, ids.len())? else { return Err(corrupt()) };
        let id = ids.get(j as usize);
        if v & 1 != 0 {
            obj::put_hex(id, &mut out);
        } else {
            out.extend_from_slice(id);
        }
    }
    out.extend_from_slice(&text[t..]);
    if flags & F_TRAILER != 0 {
        let h = obj::sha1(&[&out]);
        out.extend_from_slice(&h);
    }
    Ok(out)
}
