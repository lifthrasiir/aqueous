//! Copies of working memories for tasks: the memory of a spawner at a spawn ([`Snapshot`]), and
//! the working memory of each task made of it ([`Snapshot::copy_to`]), copied on write where the
//! system can, else by `memcpy` of the pages that are not all zeros (a new memory being of zeros
//! already).
//!
//! - macOS: `vm_copy` of whole pages, over the mappings of Wasmtime (which makes the memories of
//!   the host with its own allocator, whatever `Config::with_host_memory` is).
//! - Windows: the snapshot is a section of the paging file, mapped over the working memory
//!   copied on write ([`win`]). Wasmtime reserves its mappings as no placeholders, so the working
//!   memories are made as a module defines them, by [`Creator`] (of placeholders).

use std::ops::Range;

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::{Creator, available as placeholders};

/// The pages of zeros skipped are of this many bytes.
const PAGE: usize = 4096;

/// The working memory of a spawner at a spawn, page-aligned on Unix.
pub struct Snapshot {
    ptr: *mut u8,
    len: usize,
    /// The ranges not all zeros, in order (all of it where copied on write).
    data: Vec<Range<usize>>,
    /// The section `ptr` is a view of, where one could be made.
    #[cfg(windows)]
    section: Option<win::Section>,
}

// SAFETY: only read once made
unsafe impl Send for Snapshot {}
unsafe impl Sync for Snapshot {}

impl Snapshot {
    /// A copy of `mem`.
    pub fn new(mem: &[u8]) -> Snapshot {
        let len = mem.len();
        #[cfg(windows)]
        let section = (len > 0).then(|| win::Section::new(len)).flatten();
        #[cfg(windows)]
        let ptr = section.as_ref().map_or_else(|| zeroed(len), |s| s.ptr);
        #[cfg(not(windows))]
        let ptr = zeroed(len);
        let snapshot = |data| Snapshot {
            ptr,
            len,
            data,
            #[cfg(windows)]
            section,
        };
        // SAFETY: `ptr` is a fresh mapping of `len` bytes
        if len > 0 && unsafe { cow(mem.as_ptr(), ptr, len) } {
            return snapshot(std::iter::once(0..len).collect());
        }
        let mut data: Vec<Range<usize>> = Vec::new();
        for (i, p) in mem.chunks(PAGE).enumerate() {
            // (all of it read, which vectorizes, as most pages are not of zeros)
            if p.iter().fold(0, |a, &b| a | b) == 0 {
                continue;
            }
            let at = i * PAGE;
            // SAFETY: within both, of `len` bytes, apart
            unsafe { std::ptr::copy_nonoverlapping(p.as_ptr(), ptr.add(at), p.len()) };
            match data.last_mut() {
                Some(r) if r.end == at => r.end = at + p.len(),
                _ => data.push(at..at + p.len()),
            }
        }
        snapshot(data)
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: a mapping of `len` bytes (or none), not written after `new`
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    /// Makes `mem`, of the same length and of zeros from `clean` on, a copy of the snapshot.
    pub fn copy_to(&self, mem: &mut [u8], clean: usize) {
        assert_eq!(mem.len(), self.len);
        #[cfg(windows)]
        let clean = match self.section.as_ref().map(|s| win::map_over(s, mem)) {
            Some(win::Mapped::Yes) => return,
            Some(win::Mapped::Zeroed) => 0,
            Some(win::Mapped::No) | None => clean,
        };
        // SAFETY: both of `len` bytes, apart
        if self.len > 0 && unsafe { cow(self.ptr, mem.as_mut_ptr(), self.len) } {
            return;
        }
        let s = self.as_slice();
        let clean = clean.min(self.len);
        mem[..clean].copy_from_slice(&s[..clean]);
        for r in &self.data {
            let r = r.start.max(clean)..r.end;
            if !r.is_empty() {
                mem[r.clone()].copy_from_slice(&s[r]);
            }
        }
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.section.is_some() {
            return;
        }
        if self.len > 0 {
            // SAFETY: of `zeroed`
            unsafe { free(self.ptr, self.len) };
        }
    }
}

/// `len` bytes of zeros, of pages not touched yet: a mapping of its own on Unix, else of the
/// allocator (which has the system map large ones so).
fn zeroed(len: usize) -> *mut u8 {
    if len == 0 {
        return std::ptr::NonNull::dangling().as_ptr();
    }
    #[cfg(unix)]
    {
        // SAFETY: a new mapping
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        assert!(p != libc::MAP_FAILED, "memory for a snapshot");
        p as *mut u8
    }
    #[cfg(not(unix))]
    {
        Box::into_raw(vec![0u8; len].into_boxed_slice()) as *mut u8
    }
}

/// # Safety
/// `ptr` and `len` of [`zeroed`], `len` nonzero.
unsafe fn free(ptr: *mut u8, len: usize) {
    #[cfg(unix)]
    unsafe {
        libc::munmap(ptr as *mut libc::c_void, len);
    }
    #[cfg(not(unix))]
    unsafe {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)));
    }
}

/// Whether `len` bytes were copied from `src` to `dst` on write: on macOS where all is of whole
/// pages (`vm_copy`, which maps the pages of `src` into `dst` so).
///
/// # Safety
/// `src` is readable and `dst` writable for `len` bytes, apart, and `dst` is of anonymous
/// mappings.
#[allow(unused_variables)]
unsafe fn cow(src: *const u8, dst: *mut u8, len: usize) -> bool {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            static mach_task_self_: libc::mach_port_t;
            fn vm_copy(task: libc::mach_port_t, src: usize, size: usize, dst: usize) -> libc::c_int;
        }
        // SAFETY: no requirement
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };
        if (src as usize | dst as usize | len).is_multiple_of(page) {
            return unsafe { vm_copy(mach_task_self_, src as usize, len, dst as usize) } == 0;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whole(m: &mut Snapshot) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(m.ptr, m.len) }
    }

    /// A snapshot, apart from its source, and copies of it apart from it and each other.
    #[test]
    fn snapshots() {
        let mut want: Vec<u8> = (0..4 << 16).map(|i: usize| (i * 7 % 251) as u8).collect();
        // pages of zeros, skipped
        want[PAGE..3 * PAGE].fill(0);
        want[5 * PAGE..6 * PAGE].fill(0);
        // (page-aligned, written once made, as a memory is)
        let mut src = Snapshot::new(&[0; 4 << 16]);
        whole(&mut src).copy_from_slice(&want);
        let s = Snapshot::new(src.as_slice());
        whole(&mut src)[0] = 99;
        assert_eq!(s.as_slice(), &want[..]);
        let (mut a, mut b) = (Snapshot::new(&[0; 4 << 16]), Snapshot::new(&[0; 4 << 16]));
        s.copy_to(whole(&mut a), usize::MAX);
        // not of zeros before `clean` (as a data segment wrote)
        whole(&mut b)[..3 * PAGE].fill(1);
        s.copy_to(whole(&mut b), 3 * PAGE);
        whole(&mut a)[5] = 1;
        assert_eq!(b.as_slice(), &want[..]);
        assert_eq!(s.as_slice(), &want[..]);
        assert_eq!(a.as_slice()[5], 1);
        assert_eq!(Snapshot::new(&[]).as_slice(), &[] as &[u8]);
        // not of whole pages
        let mut odd = vec![0; 1000];
        Snapshot::new(&want[..1000]).copy_to(&mut odd, 0);
        assert_eq!(odd, want[..1000]);
        let mut odd = vec![0; PAGE + 10];
        Snapshot::new(&want[2 * PAGE - 10..3 * PAGE]).copy_to(&mut odd, 0);
        assert_eq!(odd, want[2 * PAGE - 10..3 * PAGE]);
    }
}
