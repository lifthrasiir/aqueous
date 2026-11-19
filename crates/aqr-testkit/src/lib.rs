//! What the tests share, for the edge cases they all have: inputs damaged deterministically
//! ([`damaged`]), a bound on the time a case takes ([`within`]), and an allocator refusing
//! large requests ([`Refusing`]), which is how a size taken from an input before it is checked
//! shows (an allocation of gigabytes aborts the process; refused, it names its case).
//!
//! A test of a decoder or a parser runs it on every damaged input within a time, and asserts
//! that what it makes of it, if anything, is right: no panic, no hang, no allocation the data
//! cannot hold.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::io::Write;
use std::sync::mpsc;
use std::time::Duration;

/// How an input is damaged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// A bit flipped.
    Flip(u8),
    /// The byte set to a value (0x00, 0x7f, 0x80, 0xff: of lengths and counts the extremes).
    Set(u8),
    /// The input cut at the place.
    Cut,
    /// A byte (0x00) inserted at the place.
    Insert,
    /// The byte at the place removed.
    Remove,
}

/// A damaged input: how, and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Damage {
    pub at: usize,
    pub how: How,
}

impl Damage {
    /// `d` damaged so.
    pub fn apply(&self, d: &[u8]) -> Vec<u8> {
        let mut v = d.to_vec();
        match self.how {
            How::Flip(b) => v[self.at] ^= 1 << b,
            How::Set(x) => v[self.at] = x,
            How::Cut => v.truncate(self.at),
            How::Insert => v.insert(self.at, 0),
            How::Remove => _ = v.remove(self.at),
        }
        v
    }
}

impl std::fmt::Display for Damage {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self.how {
            How::Flip(b) => write!(f, "bit {b} at {} flipped", self.at),
            How::Set(x) => write!(f, "byte at {} set to {x:#04x}", self.at),
            How::Cut => write!(f, "cut at {}", self.at),
            How::Insert => write!(f, "a byte inserted at {}", self.at),
            How::Remove => write!(f, "the byte at {} removed", self.at),
        }
    }
}

/// The ways an input of `len` bytes is damaged, at about `per` places for each way (every place
/// where `per >= len`), the first `head` bytes (where headers are) at every place: of the same
/// list whatever runs it.
pub fn damages(len: usize, per: usize, head: usize) -> Vec<Damage> {
    let step = (len / per.max(1)).max(1);
    let mut places: Vec<usize> = (0..len.min(head)).chain((0..len).step_by(step)).collect();
    places.sort_unstable();
    places.dedup();
    let mut out = Vec::new();
    for &at in &places {
        out.push(Damage { at, how: How::Flip((at * 5 % 8) as u8) });
        for x in [0x00, 0x7f, 0x80, 0xff] {
            out.push(Damage { at, how: How::Set(x) });
        }
        out.push(Damage { at, how: How::Cut });
        out.push(Damage { at, how: How::Insert });
        out.push(Damage { at, how: How::Remove });
    }
    out
}

/// `f` run on `d` damaged in every way of [`damages`], each run [`within`] `limit` and named
/// by its damage for the refusing allocator ([`case`]): the damages `f` panicked on, as a panic
/// of all of them.
pub fn damaged(d: &[u8], per: usize, head: usize, limit: Duration, f: impl Fn(&[u8]) + Sync) {
    let mut bad = Vec::new();
    for x in damages(d.len(), per, head) {
        let v = x.apply(d);
        if v == d {
            continue;
        }
        let name = x.to_string();
        let r = within(limit, || {
            case(&name);
            f(&v)
        });
        if let Err(e) = r {
            bad.push(format!("{name}: {e}"));
        }
    }
    assert!(bad.is_empty(), "{} of the damaged inputs failed:\n{}", bad.len(), bad.join("\n"));
}

/// `f`'s result, run on a thread of its own: an error naming the panic, or the time passed
/// where it takes longer than `limit` (the thread is left running: the test fails).
pub fn within<R: Send>(limit: Duration, f: impl FnOnce() -> R + Send) -> Result<R, String> {
    std::thread::scope(|s| {
        let (tx, rx) = mpsc::channel();
        let h = s.spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
            let _ = tx.send(());
            r
        });
        match rx.recv_timeout(limit) {
            Ok(()) => h.join().unwrap().map_err(|p| panic_text(&*p)),
            Err(_) => {
                // (a thread of a scope is joined at its end: one past its time ends the process
                // rather than waiting for ever)
                eprintln!("a case took longer than {limit:?}");
                std::process::exit(101)
            }
        }
    })
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    match (p.downcast_ref::<&str>(), p.downcast_ref::<String>()) {
        (Some(s), _) => format!("panicked: {s}"),
        (_, Some(s)) => format!("panicked: {s}"),
        _ => "panicked".into(),
    }
}

/// A global allocator that remembers the largest single request of each thread since
/// [`reset`] and refuses (as an exhausted heap does) those of its limit and above, telling the
/// case (named by [`case`]) on standard error before the process aborts:
///
/// ```ignore
/// #[global_allocator]
/// static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);
/// ```
pub struct Refusing(pub usize);

thread_local! {
    static MOST: Cell<usize> = const { Cell::new(0) };
    static CASE: RefCell<[u8; 160]> = const { RefCell::new([0; 160]) };
}

impl Refusing {
    fn note(&self, n: usize) -> bool {
        let _ = MOST.try_with(|m| m.set(m.get().max(n)));
        if n < self.0 {
            return true;
        }
        let _ = CASE.try_with(|c| {
            if let Ok(c) = c.try_borrow() {
                let end = c.iter().position(|&b| b == 0).unwrap_or(c.len());
                let mut e = std::io::stderr();
                let _ =
                    writeln!(e, "refused an allocation of {n} bytes, in case: {}", String::from_utf8_lossy(&c[..end]));
            }
        });
        false
    }
}

// SAFETY: System's, with requests refused by returning null, as GlobalAlloc allows.
unsafe impl GlobalAlloc for Refusing {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if self.note(l.size()) { unsafe { System.alloc(l) } } else { std::ptr::null_mut() }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        if self.note(l.size()) { unsafe { System.alloc_zeroed(l) } } else { std::ptr::null_mut() }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if self.note(n) { unsafe { System.realloc(p, l, n) } } else { std::ptr::null_mut() }
    }
}

/// Names the case this thread runs, for [`Refusing`] to tell, and forgets its largest request.
pub fn case(name: &str) {
    let _ = CASE.try_with(|c| {
        let mut c = c.borrow_mut();
        let n = name.len().min(c.len() - 1);
        c[..n].copy_from_slice(&name.as_bytes()[..n]);
        c[n] = 0;
    });
    reset();
}

/// Forgets this thread's largest request.
pub fn reset() {
    let _ = MOST.try_with(|m| m.set(0));
}

/// This thread's largest single request since [`reset`] (or [`case`]), under [`Refusing`].
pub fn most() -> usize {
    MOST.try_with(Cell::get).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damages_applied() {
        let d = b"abcdef";
        let all = damages(d.len(), 100, 0);
        assert_eq!(all.len(), 6 * 8);
        assert_eq!(Damage { at: 2, how: How::Cut }.apply(d), b"ab");
        assert_eq!(Damage { at: 0, how: How::Remove }.apply(d), b"bcdef");
        assert_eq!(Damage { at: 1, how: How::Insert }.apply(d), b"a\0bcdef");
        assert_eq!(Damage { at: 0, how: How::Flip(0) }.apply(d), b"`bcdef");
        // a stride, and every place of the head
        let some = damages(1000, 10, 4);
        let places: std::collections::BTreeSet<usize> = some.iter().map(|x| x.at).collect();
        assert!(places.contains(&3) && places.contains(&900) && !places.contains(&5), "{places:?}");
    }

    #[test]
    fn panics_and_times_told() {
        assert_eq!(within(Duration::from_secs(5), || 7), Ok(7));
        let e = within(Duration::from_secs(5), || panic!("bad {}", 1)).unwrap_err();
        assert!(e.contains("bad 1"), "{e}");
        let r = std::panic::catch_unwind(|| {
            damaged(b"xyz", 10, 3, Duration::from_secs(5), |v| assert!(v.len() != 2, "cut"));
        });
        let e = panic_text(&*r.unwrap_err());
        assert!(e.contains("cut at 2") && e.contains("the byte at 0 removed"), "{e}");
    }
}
