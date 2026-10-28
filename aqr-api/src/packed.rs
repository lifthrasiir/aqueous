//! Packed byte sequences (spec, "Packed byte sequences"): a list of byte sequences as one input,
//! for a module taking a number of them that varies between applications.

use core::marker::PhantomData;
use core::ops::Range;

use crate::{Input, Memory};

/// A packed byte sequence from offset 0 of the memory `M`, its header checked: the number of
/// items, and where each of them is in `M` ([`Packed::range`]).
///
/// ```ignore
/// aqr_api::entry! {
///     whole fn cat(params_len, in0_len, size) {
///         let p = Packed::<Input<0>>::new(in0_len);
///         let mut at = 0;
///         for i in 0..p.count() {
///             let r = p.range(i);
///             Output::<0>::copy_from::<Input<0>>(at, r.start, r.end - r.start);
///             at += r.end - r.start;
///         }
///     }
/// }
/// ```
pub struct Packed<M> {
    count: u64,
    _memory: PhantomData<M>,
}

impl<M: Memory> Packed<M> {
    /// The packed byte sequence of `len` bytes, which is all there is to read of `M`. Traps
    /// where it is not one, as the spec's "unpack" fails.
    pub fn new(len: u64) -> Packed<M> {
        let count = u64_at::<M>(0, len);
        if count >= len / 8 {
            invalid();
        }
        // the ends, a chunk at a time
        let (h, mut last, mut buf) = (8 * (count + 1), 0, [0u8; 256]);
        let mut i = 0;
        while i < count {
            let n = (count - i).min(32) as usize;
            M::read(8 * (i + 1), &mut buf[..8 * n]);
            for &e in buf[..8 * n].as_chunks::<8>().0 {
                let e = u64::from_le_bytes(e);
                if e < last {
                    invalid();
                }
                last = e;
            }
            i += n as u64;
        }
        if len - h != last {
            invalid();
        }
        Packed { count, _memory: PhantomData }
    }

    /// The number of items.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// The offsets of the item `i` in `M`, for `i` < [`Packed::count`].
    pub fn range(&self, i: u64) -> Range<u64> {
        assert!(i < self.count);
        let h = 8 * (self.count + 1);
        let start = if i == 0 { 0 } else { u64_at::<M>(8 * i, h) };
        h + start..h + u64_at::<M>(8 * (i + 1), h)
    }
}

impl<const K: u32> Packed<Input<K>>
where
    Input<K>: Memory,
{
    /// [`Packed::new`] for a part module: the header requested first.
    pub fn request(len: u64) -> Packed<Input<K>> {
        if len < 8 {
            invalid();
        }
        Input::<K>::request(8);
        let count = u64_at::<Input<K>>(0, len);
        if count >= len / 8 {
            invalid();
        }
        Input::<K>::request(8 * (count + 1));
        Packed::new(len)
    }
}

/// The u64le at offset `at` of `M`, within its first `len` bytes.
fn u64_at<M: Memory>(at: u64, len: u64) -> u64 {
    if len < 8 || at > len - 8 {
        invalid();
    }
    let mut b = [0; 8];
    M::read(at, &mut b);
    u64::from_le_bytes(b)
}

fn invalid() -> ! {
    crate::log(b"aqr_api: not a packed byte sequence");
    crate::trap()
}
