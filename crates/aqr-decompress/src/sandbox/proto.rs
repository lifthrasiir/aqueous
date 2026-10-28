//! The messages between the decoder and a worker, over the worker's standard input and output:
//! integers little endian, byte strings of a u64 length.
//!
//! The decoder reads what a worker writes as untrusted: a length is checked against what it
//! can be before anything of it is allocated.
//!
//! The inputs and outputs of a large application are in the worker's arena instead, at the
//! places [`layout`] gives (the arena the decoder made and sent last, or one sent with the
//! request: [`ARENA`], [`NEW_ARENA`]); those of a small one in the messages ([`INLINE`]).

use std::io::{self, Read, Write};

use aqr_formula::aqr::Failure;

/// The first bytes each way: the decoder's (then the threads of the worker), and the worker's
/// once it is locked down.
pub const HELLO: &[u8; 8] = b"AQRSBX\x00\x01";
pub const READY: &[u8; 8] = b"AQRSBX\x01\x01";

/// The requests.
pub const LOAD: u8 = 1;
pub const APPLY: u8 = 2;
pub const PROBE: u8 = 3;

/// Where the inputs and outputs of an application are.
pub const INLINE: u8 = 0;
pub const ARENA: u8 = 1;
/// ... in a new arena, of its length and the token of [`os::Child::share`](super::os).
pub const NEW_ARENA: u8 = 2;

/// The alignment of the inputs and outputs in an arena.
pub const ALIGN: u64 = 1 << 16;

/// An arena longer is used for one application only, so that a large one leaves no memory
/// held after it.
pub const KEEP: u64 = 256 << 20;

/// The places of byte strings of `lens` in an arena, each aligned to [`ALIGN`], and the length
/// of the arena they take; `None` where it overflows.
pub fn layout(lens: impl IntoIterator<Item = u64>) -> Option<(Vec<std::ops::Range<u64>>, u64)> {
    let mut end = 0u64;
    let mut places = Vec::new();
    for len in lens {
        let at = end.checked_next_multiple_of(ALIGN)?;
        end = at.checked_add(len)?;
        places.push(at..end);
    }
    Some((places, end.checked_next_multiple_of(ALIGN)?))
}

/// The answers: done, or failed (of a [`Failure`]).
const OK: u8 = 0;
const INVALID: u8 = 1;
const RESOURCE: u8 = 2;
const UNSUPPORTED: u8 = 3;

/// The longest message of a failure the decoder takes.
pub const MAX_MESSAGE: u64 = 1 << 20;

pub fn put_u8(w: &mut impl Write, v: u8) -> io::Result<()> {
    w.write_all(&[v])
}

pub fn put_u64(w: &mut impl Write, v: u64) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}

pub fn put_bytes(w: &mut impl Write, b: &[u8]) -> io::Result<()> {
    put_u64(w, b.len() as u64)?;
    w.write_all(b)
}

pub fn get_u8(r: &mut impl Read) -> io::Result<u8> {
    let mut b = [0];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

pub fn get_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

pub fn get_hash(r: &mut impl Read) -> io::Result<[u8; 32]> {
    let mut h = [0; 32];
    r.read_exact(&mut h)?;
    Ok(h)
}

/// A byte string of at most `max` bytes.
pub fn get_bytes(r: &mut impl Read, max: u64) -> io::Result<Vec<u8>> {
    let n = get_u64(r)?;
    if n > max {
        return Err(violation(format!("{n} bytes where at most {max}")));
    }
    let mut b = vec![0; n as usize];
    r.read_exact(&mut b)?;
    Ok(b)
}

/// A count of at most `max`.
pub fn get_count(r: &mut impl Read, max: u64) -> io::Result<usize> {
    let n = get_u64(r)?;
    if n > max {
        return Err(violation(format!("{n} items where at most {max}")));
    }
    Ok(n as usize)
}

pub fn violation(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// The head of an answer: done (its payload follows), or failed.
pub fn put_result(w: &mut impl Write, r: &Result<(), Failure>) -> io::Result<()> {
    let (tag, msg) = match r {
        Ok(()) => return put_u8(w, OK),
        Err(Failure::Invalid(m)) => (INVALID, m),
        Err(Failure::Resource(m)) => (RESOURCE, m),
        Err(Failure::Unsupported(m)) => (UNSUPPORTED, m),
    };
    put_u8(w, tag)?;
    put_bytes(w, &msg.as_bytes()[..msg.len().min(MAX_MESSAGE as usize)])
}

/// The head of an answer: `Ok` where its payload follows.
pub fn get_result(r: &mut impl Read) -> io::Result<Result<(), Failure>> {
    let tag = get_u8(r)?;
    if tag == OK {
        return Ok(Ok(()));
    }
    let msg = String::from_utf8_lossy(&get_bytes(r, MAX_MESSAGE)?).into_owned();
    Ok(Err(match tag {
        INVALID => Failure::Invalid(msg),
        RESOURCE => Failure::Resource(msg),
        UNSUPPORTED => Failure::Unsupported(msg),
        _ => return Err(violation(format!("an answer of tag {tag}"))),
    }))
}
