//! Copies of working memories for tasks: the memory of a spawner at a spawn ([`Snapshot`]), and
//! the working memory of each task made of it ([`Snapshot::copy_to`]), copied on write where the
//! system can (macOS, `vm_copy` of whole pages), else by `memcpy`.
//!
//! (Wasmtime makes the memories of the host with its own allocator, whatever
//! `Config::with_host_memory` is, so the copies are made over its mappings.)

/// The working memory of a spawner at a spawn, page-aligned.
pub struct Snapshot {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: only read once made
unsafe impl Send for Snapshot {}
unsafe impl Sync for Snapshot {}

impl Snapshot {
    /// A copy of `mem`.
    pub fn new(mem: &[u8]) -> Snapshot {
        let len = mem.len();
        if len == 0 {
            return Snapshot { ptr: std::ptr::NonNull::dangling().as_ptr(), len };
        }
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
        let ptr = p as *mut u8;
        // SAFETY: `ptr` is a fresh mapping of `len` bytes
        unsafe { copy(mem.as_ptr(), ptr, len) };
        Snapshot { ptr, len }
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: a mapping of `len` bytes (or none), not written after `new`
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    /// Makes `mem`, of the same length, a copy of the snapshot.
    pub fn copy_to(&self, mem: &mut [u8]) {
        assert_eq!(mem.len(), self.len);
        if self.len > 0 {
            // SAFETY: both of `len` bytes, apart
            unsafe { copy(self.ptr, mem.as_mut_ptr(), self.len) }
        }
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        if self.len > 0 {
            // SAFETY: the mapping of `new`
            unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.len) };
        }
    }
}

/// Copies `len` bytes from `src` to `dst`: copied on write on macOS where all is of whole pages
/// (`vm_copy`, which maps the pages of `src` into `dst` so), else by `memcpy`.
///
/// # Safety
/// `src` is readable and `dst` writable for `len` bytes, apart, and `dst` is of anonymous
/// mappings.
unsafe fn copy(src: *const u8, dst: *mut u8, len: usize) {
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            static mach_task_self_: libc::mach_port_t;
            fn vm_copy(task: libc::mach_port_t, src: usize, size: usize, dst: usize) -> libc::c_int;
        }
        // SAFETY: no requirement
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };
        if (src as usize | dst as usize | len).is_multiple_of(page)
            && unsafe { vm_copy(mach_task_self_, src as usize, len, dst as usize) } == 0
        {
            return;
        }
    }
    unsafe { std::ptr::copy_nonoverlapping(src, dst, len) }
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
        let want: Vec<u8> = (0..4 << 16).map(|i: usize| (i * 7 % 251) as u8).collect();
        // (page-aligned, written once made, as a memory is)
        let mut src = Snapshot::new(&[0; 4 << 16]);
        whole(&mut src).copy_from_slice(&want);
        let s = Snapshot::new(src.as_slice());
        whole(&mut src)[0] = 99;
        assert_eq!(s.as_slice(), &want[..]);
        let (mut a, mut b) = (Snapshot::new(&[0; 4 << 16]), Snapshot::new(&[1; 4 << 16]));
        s.copy_to(whole(&mut a));
        s.copy_to(whole(&mut b));
        whole(&mut a)[5] = 1;
        assert_eq!(b.as_slice(), &want[..]);
        assert_eq!(s.as_slice(), &want[..]);
        assert_eq!(a.as_slice()[5], 1);
        assert_eq!(Snapshot::new(&[]).as_slice(), &[] as &[u8]);
        // not of whole pages
        let mut odd = vec![0; 1000];
        Snapshot::new(&want[..1000]).copy_to(&mut odd);
        assert_eq!(odd, want[..1000]);
    }
}
