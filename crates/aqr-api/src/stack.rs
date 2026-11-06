//! The working memory as a stack: an allocator (on WebAssembly, [`Stack`]) that puts each block
//! on top of the last one and takes it off as it is freed, so that what a module takes at most is
//! what it lays out, in the order it does: its plan, which the encoder declares as its memory
//! limit as it is (aqr-formula's `Module::with_plan`), computed by the functions here from the
//! sizes of its blocks.
//!
//! - Blocks are rounded up to [`GRAIN`] bytes ([`block`]), and aligned to it.
//! - The block on top grows and shrinks where it is (a vector reserved short grows so), and is
//!   taken off as it is freed; a block freed below it is a hole until those above it are taken
//!   off, and then it goes with them. A hole is never filled: a module freeing out of order
//!   takes more than its plan, which the memory limit then turns down.
//! - The stack starts at the end of the initial memory (its data and its stack), and the memory
//!   grows as the top passes its end: the working memory is the initial memory and the plan, in
//!   pages, at most.

/// Bytes that blocks are rounded up and aligned to.
pub const GRAIN: u64 = 16;

/// Bytes that a block of `n` bytes takes on the stack.
pub const fn block(n: u64) -> u64 {
    n.div_ceil(GRAIN) * GRAIN
}

/// Bytes of a vector or `Cow` of bytes, and of a slice of them, on WebAssembly (32-bit
/// pointers): the elements of the vectors of them that [`crate::whole`] makes.
const COW: u64 = 12;
const SLICE: u64 = 8;

/// Bytes that [`crate::whole`] takes of the stack before it calls its function, of parameters,
/// resources and inputs of these lengths: each copied, and the lists of them.
pub fn whole(params: u64, resources: &[u64], inputs: &[u64]) -> u64 {
    let list = |lens: &[u64]| match lens.len() as u64 {
        0 => 0,
        n => block(COW * n) + lens.iter().map(|&l| block(l)).sum::<u64>() + block(SLICE * n),
    };
    block(params) + list(resources) + list(inputs)
}

#[cfg(target_family = "wasm")]
pub use imp::Stack;

#[cfg(target_family = "wasm")]
mod imp {
    use core::alloc::{GlobalAlloc, Layout};
    use core::arch::wasm32::{memory_grow, memory_size};
    use core::cell::UnsafeCell;

    use super::GRAIN;

    const PAGE: usize = 65536;
    /// Holes kept at once at most: a module freeing more out of order traps.
    const HOLES: usize = 64;

    struct State {
        /// The top of the stack, and the end of the memory (0 before the first block).
        top: usize,
        end: usize,
        holes: [(usize, usize); HOLES],
        n: usize,
    }

    /// The allocator of a module laying its memory out as a stack (see [the module](super)):
    /// `#[global_allocator] static STACK: Stack = Stack::new();` in its binary.
    pub struct Stack(UnsafeCell<State>);

    // SAFETY: an instance runs on one thread (its tasks are instances of their own)
    unsafe impl Sync for Stack {}

    impl Stack {
        pub const fn new() -> Stack {
            Stack(UnsafeCell::new(State { top: 0, end: 0, holes: [(0, 0); HOLES], n: 0 }))
        }
    }

    impl Default for Stack {
        fn default() -> Stack {
            Stack::new()
        }
    }

    fn round(n: usize) -> Option<usize> {
        n.checked_next_multiple_of(GRAIN as usize)
    }

    impl State {
        /// Whether the memory reaches `to`, grown as needed.
        fn reach(&mut self, to: usize) -> bool {
            if self.end == 0 {
                self.end = memory_size(0) * PAGE;
                self.top = self.end;
            }
            if to <= self.end {
                return true;
            }
            let pages = (to - self.end).div_ceil(PAGE);
            if memory_grow(0, pages) == usize::MAX {
                return false;
            }
            self.end += pages * PAGE;
            true
        }

        fn hole(&mut self, from: usize, to: usize) {
            if from == to {
                return;
            }
            if self.n == HOLES {
                crate::log(b"aqr_api::stack: too many blocks freed out of order");
                crate::trap()
            }
            self.holes[self.n] = (from, to);
            self.n += 1;
        }

        /// Takes off the block from `p` on top, and the holes then on top.
        fn pop(&mut self, p: usize) {
            self.top = p;
            while let Some(i) = self.holes[..self.n].iter().position(|h| h.1 == self.top) {
                self.top = self.holes[i].0;
                self.n -= 1;
                self.holes[i] = self.holes[self.n];
            }
        }

        fn alloc(&mut self, layout: Layout) -> *mut u8 {
            let (Some(size), true) = (round(layout.size()), self.reach(0)) else { return core::ptr::null_mut() };
            // (an alignment past the grain leaves a hole below the block)
            let Some(p) = self.top.checked_next_multiple_of(layout.align().max(GRAIN as usize)) else {
                return core::ptr::null_mut();
            };
            let Some(end) = p.checked_add(size).filter(|&e| self.reach(e)) else { return core::ptr::null_mut() };
            let top = self.top;
            self.hole(top, p);
            self.top = end;
            p as *mut u8
        }

        fn dealloc(&mut self, p: *mut u8, layout: Layout) {
            let (p, size) = (p as usize, round(layout.size()).unwrap_or(0));
            match p + size == self.top {
                true => self.pop(p),
                false => self.hole(p, p + size),
            }
        }
    }

    unsafe impl GlobalAlloc for Stack {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: one thread, and no call of it within another
            unsafe { (*self.0.get()).alloc(layout) }
        }

        unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
            // SAFETY: as `alloc`
            unsafe { (*self.0.get()).dealloc(p, layout) }
        }

        unsafe fn realloc(&self, p: *mut u8, layout: Layout, new: usize) -> *mut u8 {
            // SAFETY: as `alloc`
            let s = unsafe { &mut *self.0.get() };
            let (Some(old), Some(size)) = (round(layout.size()), round(new)) else { return core::ptr::null_mut() };
            // (the block on top where it is)
            if p as usize + old == s.top {
                let Some(end) = (p as usize).checked_add(size).filter(|&e| s.reach(e)) else {
                    return core::ptr::null_mut();
                };
                s.top = end;
                return p;
            }
            let Ok(to) = Layout::from_size_align(new, layout.align()) else { return core::ptr::null_mut() };
            let q = s.alloc(to);
            if !q.is_null() {
                // SAFETY: two blocks of at least the bytes copied, apart
                unsafe { core::ptr::copy_nonoverlapping(p, q, layout.size().min(new)) };
                s.dealloc(p, layout);
            }
            q
        }
    }
}
