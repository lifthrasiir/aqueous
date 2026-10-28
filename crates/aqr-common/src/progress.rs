//! How far the work on data is, told by where in the data it is: a recompressor calls [`done`]
//! with each part of its input it is done with, and whoever watches the data that the part is
//! of ([`watch`]) is told where the part is in it. Data made of watched data (a stream
//! inflated, say) is told of as the part of what it was made of in proportion ([`alias`]).
//! Parts may be told of in any order, more than once, and within each other (a member of a
//! container, and the container).
//!
//! The data is known by its addresses, so nothing is passed along: a recompressor tells of the
//! slices it is given, on any thread, and what nobody watches costs an atomic load.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

type Sink = Arc<dyn Fn(Range<u64>) + Send + Sync>;

enum Target {
    /// Told the offsets of the parts done.
    Sink(Sink),
    /// Told of as the data at `start` of `len` bytes, in proportion.
    Alias { start: usize, len: usize },
}

/// The watched data by its first address: where it ends, and what it tells.
static WATCHED: RwLock<BTreeMap<usize, (usize, Target)>> = RwLock::new(BTreeMap::new());
/// Entries of [`WATCHED`], read before its lock is taken.
static COUNT: AtomicUsize = AtomicUsize::new(0);

/// Aliases followed at most from the part told of.
const DEPTH: usize = 8;

/// The watch of some data, until dropped.
#[must_use = "the data is watched only until the watch is dropped"]
pub struct Watch(Option<usize>);

impl Drop for Watch {
    fn drop(&mut self) {
        if let Some(start) = self.0 {
            WATCHED.write().unwrap().remove(&start);
            COUNT.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

fn add(d: &[u8], t: Target) -> Watch {
    // (empty data has no address of its own)
    if d.is_empty() {
        return Watch(None);
    }
    let start = d.as_ptr() as usize;
    match WATCHED.write().unwrap().entry(start) {
        // (watched already: kept so)
        std::collections::btree_map::Entry::Occupied(_) => Watch(None),
        std::collections::btree_map::Entry::Vacant(e) => {
            e.insert((start + d.len(), t));
            COUNT.fetch_add(1, Ordering::Relaxed);
            Watch(Some(start))
        }
    }
}

/// Watches `d`: `f` is told the parts of it done (as offsets), from any thread. Data watched or
/// aliased already, from the same address, stays as it is.
pub fn watch(d: &[u8], f: impl Fn(Range<u64>) + Send + Sync + 'static) -> Watch {
    add(d, Target::Sink(Arc::new(f)))
}

/// Tells of the work on `d` as of that on `of`, `d` having been made of it: an offset into `d`
/// is the same share of `of`. `d` is data of its own, neither watched data nor part of it.
pub fn alias(d: &[u8], of: &[u8]) -> Watch {
    if COUNT.load(Ordering::Relaxed) == 0 {
        return Watch(None);
    }
    add(d, Target::Alias { start: of.as_ptr() as usize, len: of.len() })
}

/// Tells that the work on `part` of some data is done.
pub fn done(part: &[u8]) {
    if COUNT.load(Ordering::Relaxed) == 0 || part.is_empty() {
        return;
    }
    let mut at = part.as_ptr() as usize..part.as_ptr() as usize + part.len();
    let sink = {
        let w = WATCHED.read().unwrap();
        let mut depth = 0;
        loop {
            let Some((&from, &(end, ref t))) = w.range(..=at.start).next_back() else { return };
            if at.start >= end {
                return;
            }
            let part = (at.start - from) as u64..(at.end.min(end) - from) as u64;
            match t {
                Target::Sink(f) => break (f.clone(), part),
                &Target::Alias { start, len } => {
                    depth += 1;
                    if len == 0 || depth > DEPTH {
                        return;
                    }
                    let to = |n: u64| start + (n as u128 * len as u128 / (end - from) as u128) as usize;
                    at = to(part.start)..to(part.end);
                    if at.is_empty() {
                        return;
                    }
                }
            }
        }
    };
    // (told without the lock, which the sink may want)
    (sink.0)(sink.1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn watched_and_aliased() {
        let file = vec![0u8; 1000];
        let told = Arc::new(Mutex::new(Vec::new()));
        let t = told.clone();
        let w = watch(&file, move |r| t.lock().unwrap().push(r));
        done(&file[..10]);
        done(&file[300..400]);
        // made of the second half of the file, twice as long
        let made = vec![0u8; 1000];
        let a = alias(&made, &file[500..]);
        // (as it was aliased first)
        let b = alias(&made, &file);
        done(&made[100..500]);
        done(&made);
        // (nobody's data)
        done(&[1, 2, 3]);
        drop((b, a));
        done(&made);
        drop(w);
        done(&file);
        assert_eq!(*told.lock().unwrap(), [0..10, 300..400, 550..750, 500..1000]);
    }
}
