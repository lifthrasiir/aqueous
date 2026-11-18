//! Pages of files mapped given back to the system as they are no longer needed, so that the
//! resident set holds what is in use rather than all that was ever touched: the spill's
//! regions dropped ([`discard`]: their bytes of no use), the inputs read through ([`let_go`]:
//! read again from their files where touched again), and pages of the process's own mapped
//! for that ([`map_own`]) given back ([`forget`]). Only the pages wholly within the bytes told
//! are given back; a page shared with bytes still in use is left.

use std::fs::File;

/// Bytes of a page of memory (4 KiB where not known).
pub fn size() -> usize {
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    static SIZE: AtomicUsize = AtomicUsize::new(0);
    match SIZE.load(Relaxed) {
        0 => {
            let n = page_size().filter(|n| n.is_power_of_two()).unwrap_or(4 << 10);
            SIZE.store(n, Relaxed);
            n
        }
        n => n,
    }
}

#[cfg(unix)]
fn page_size() -> Option<usize> {
    // SAFETY: of no preconditions
    usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()
}

#[cfg(windows)]
fn page_size() -> Option<usize> {
    /// SYSTEM_INFO, as far as `dwPageSize` and the fields after it (of which nothing is read).
    #[repr(C)]
    struct SystemInfo {
        arch: u32,
        page_size: u32,
        rest: [usize; 6],
    }
    unsafe extern "system" {
        fn GetSystemInfo(info: *mut SystemInfo);
    }
    let mut info = SystemInfo { arch: 0, page_size: 0, rest: [0; 6] };
    // SAFETY: a SYSTEM_INFO (its layout: two 32-bit fields, then pointers and their likes, no
    // more than six of them), written by the call
    unsafe { GetSystemInfo(&mut info) };
    Some(info.page_size as usize)
}

#[cfg(not(any(unix, windows)))]
fn page_size() -> Option<usize> {
    None
}

/// The pages wholly within bytes `at..at + len`: where they start, and their bytes (none: 0).
fn within(at: usize, len: usize) -> (usize, usize) {
    let page = size();
    let start = at.next_multiple_of(page);
    let end = (at + len) & !(page - 1);
    (start, end.saturating_sub(start))
}

/// Gives back the pages wholly within bytes `at..at + len` of a mapping of all of `file` (its
/// byte 0 at `base`), whose bytes are of no use any longer: in a region of the spill dropped.
/// What they read after is not known (zeros, or what they were); the file takes no room for
/// them where the system can tell it so.
///
/// - Linux: a hole punched in the file (`FALLOC_FL_PUNCH_HOLE`), its pages out of the page
///   cache and the mapping; and the pages out of the mapping (`MADV_DONTNEED`), those the file
///   system keeps (or all, where it cannot punch: written back as the system goes, then
///   dropped).
/// - macOS: a hole punched (`F_PUNCHHOLE`), and the pages out of the mapping
///   (`MADV_DONTNEED`).
/// - Windows: nothing. It cannot drop changed pages of a file but by writing them: those taken
///   out of the working set (`VirtualUnlock` of pages not locked) went to the modified list and
///   were written within seconds, though memory was plenty, and counted in use until then.
///   Left in place, they are taken again by the regions after them (as the spill takes the
///   smallest free run that fits), written to the file only as memory runs short. Neither
///   `MEM_RESET`, `DiscardVirtualMemory` nor `OfferVirtualMemory` takes pages of a file
///   (`ERROR_INVALID_ADDRESS`), and `FSCTL_SET_ZERO_DATA` of a sparse file writes them first.
///
/// # Safety
///
/// `base` is the start of a mapping (shared) of all of `file`, `at..at + len` within it, and
/// nothing reads or writes those bytes until they are written again.
pub unsafe fn discard(file: &File, base: *const u8, at: *const u8, len: usize) {
    let (start, n) = within(at as usize, len);
    if n == 0 {
        return;
    }
    let offset = start - base as usize;
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let mode = libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE;
        // SAFETY: the file's own descriptor; the bytes are of no use (as the caller tells)
        unsafe { libc::fallocate(file.as_raw_fd(), mode, offset as libc::off_t, n as libc::off_t) };
        // (and out of the mapping: those the file system keeps, as ext4 does the last page of a
        // hole, or all where it cannot punch)
        // SAFETY: pages of a shared mapping, their bytes kept by the file
        unsafe { libc::madvise(start as *mut libc::c_void, n, libc::MADV_DONTNEED) };
    }
    #[cfg(target_vendor = "apple")]
    {
        use std::os::fd::AsRawFd;
        let hole = libc::fpunchhole_t {
            fp_flags: 0,
            reserved: 0,
            fp_offset: offset as libc::off_t,
            fp_length: n as libc::off_t,
        };
        // SAFETY: the file's own descriptor and a fpunchhole_t; pages of a shared mapping
        unsafe {
            libc::fcntl(file.as_raw_fd(), libc::F_PUNCHHOLE, &hole);
            libc::madvise(start as *mut libc::c_void, n, libc::MADV_DONTNEED);
        }
    }
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    let _ = (file, offset);
}

/// Drops the pages wholly within bytes `at..at + len` of a mapping of a file, never written
/// through it, out of the resident set: read again from the file (or the system's cache of
/// it) where they are touched again. Linux: `MADV_DONTNEED`; macOS: `MADV_DONTNEED` (which
/// only lowers their priority there); Windows: out of the working set, on standby.
///
/// # Safety
///
/// `at..at + len` is within a mapping, read only (or of pages of zeros, anonymous, put in it),
/// of a file.
pub unsafe fn let_go(at: *const u8, len: usize) {
    let (start, n) = within(at as usize, len);
    if n == 0 {
        return;
    }
    #[cfg(unix)]
    // SAFETY: pages of a mapping not written: the same bytes read again
    unsafe {
        libc::madvise(start as *mut libc::c_void, n, libc::MADV_DONTNEED)
    };
    #[cfg(windows)]
    // SAFETY: pages of the process's own mapping
    unsafe {
        trim(start, n)
    };
}

/// The pages of `start..start + n` out of the working set: `VirtualUnlock` of pages not locked
/// does that (and fails, `ERROR_NOT_LOCKED`, as they were not).
#[cfg(windows)]
unsafe fn trim(start: usize, n: usize) {
    unsafe extern "system" {
        fn VirtualUnlock(address: *mut core::ffi::c_void, size: usize) -> i32;
    }
    // SAFETY: as the callers tell
    unsafe { VirtualUnlock(start as *mut core::ffi::c_void, n) };
}

/// `n` bytes (at least 1) of zeroed pages of the process's own, mapped from the system rather
/// than the allocator (which may give a block of pages already touched), each page taking
/// memory only once touched, to be given back in part by [`forget`] and whole by
/// [`unmap_own`]. Fails where the system has no such memory to give.
pub fn map_own(n: usize) -> std::io::Result<*mut u8> {
    let n = n.max(1);
    #[cfg(unix)]
    {
        let (prot, flags) = (libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANON);
        // SAFETY: a new mapping, not at a fixed address
        let p = unsafe { libc::mmap(std::ptr::null_mut(), n, prot, flags, -1, 0) };
        if p == libc::MAP_FAILED { Err(std::io::Error::last_os_error()) } else { Ok(p.cast()) }
    }
    #[cfg(windows)]
    {
        // SAFETY: new pages reserved and committed, at no address given
        let p = unsafe { win::VirtualAlloc(std::ptr::null_mut(), n, win::MEM_RESERVE | win::MEM_COMMIT, win::RW) };
        if p.is_null() { Err(std::io::Error::last_os_error()) } else { Ok(p.cast()) }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let l = std::alloc::Layout::from_size_align(n, size()).map_err(std::io::Error::other)?;
        // SAFETY: a layout of a nonzero size
        let p = unsafe { std::alloc::alloc_zeroed(l) };
        if p.is_null() { Err(std::io::ErrorKind::OutOfMemory.into()) } else { Ok(p) }
    }
}

/// Gives back the `n` bytes [`map_own`] gave at `p`.
///
/// # Safety
///
/// `p` and `n` are as [`map_own`] gave them, and nothing uses the bytes any longer.
pub unsafe fn unmap_own(p: *mut u8, n: usize) {
    let n = n.max(1);
    #[cfg(windows)]
    let _ = n;
    #[cfg(unix)]
    // SAFETY: as the caller tells
    unsafe {
        libc::munmap(p.cast(), n)
    };
    #[cfg(windows)]
    // SAFETY: as the caller tells: the whole of a reservation
    unsafe {
        win::VirtualFree(p.cast(), 0, win::MEM_RELEASE)
    };
    #[cfg(not(any(unix, windows)))]
    // SAFETY: as the caller tells
    unsafe {
        std::alloc::dealloc(p, std::alloc::Layout::from_size_align_unchecked(n, size()))
    };
}

/// Gives back the pages wholly within bytes `at..at + len` of memory [`map_own`] gave, whose
/// bytes are of no use any longer: what they read after is not known (zeros, or what they were)
/// until they are written again, and they are never written out to be kept (the pagefile, a
/// swap) as they are. Linux: `MADV_DONTNEED` (zeros); macOS: `MADV_FREE_REUSABLE` (out of the
/// footprint); Windows: `MEM_RESET` and out of the working set (on the free list, as they are
/// not to be written).
///
/// # Safety
///
/// `at..at + len` is within memory [`map_own`] gave, and nothing reads those bytes until they
/// are written again.
pub unsafe fn forget(at: *const u8, len: usize) {
    let (start, n) = within(at as usize, len);
    if n == 0 {
        return;
    }
    #[cfg(target_os = "linux")]
    // SAFETY: pages of a private mapping of the process's own, as the caller tells
    unsafe {
        libc::madvise(start as *mut libc::c_void, n, libc::MADV_DONTNEED)
    };
    #[cfg(target_vendor = "apple")]
    // SAFETY: as above
    unsafe {
        libc::madvise(start as *mut libc::c_void, n, libc::MADV_FREE_REUSABLE)
    };
    #[cfg(windows)]
    // SAFETY: committed pages of the process's own, as the caller tells
    unsafe {
        win::VirtualAlloc(start as *mut core::ffi::c_void, n, win::MEM_RESET, win::RW);
        trim(start, n)
    };
    #[cfg(not(any(target_os = "linux", target_vendor = "apple", windows)))]
    let _ = start;
}

#[cfg(windows)]
mod win {
    pub const MEM_COMMIT: u32 = 0x1000;
    pub const MEM_RESERVE: u32 = 0x2000;
    pub const MEM_RESET: u32 = 0x8_0000;
    pub const MEM_RELEASE: u32 = 0x8000;
    /// `PAGE_READWRITE`.
    pub const RW: u32 = 0x04;

    unsafe extern "system" {
        pub fn VirtualAlloc(
            address: *mut core::ffi::c_void,
            size: usize,
            kind: u32,
            protect: u32,
        ) -> *mut core::ffi::c_void;
        pub fn VirtualFree(address: *mut core::ffi::c_void, size: usize, kind: u32) -> i32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pages wholly within the bytes told, none where they are within a page.
    #[test]
    fn within_pages() {
        let p = size();
        assert_eq!(within(0, p), (0, p));
        assert_eq!(within(1, p), (p, 0));
        assert_eq!(within(1, 3 * p), (p, 2 * p));
        assert_eq!(within(p - 1, 2 * p + 1), (p, 2 * p));
        assert_eq!(within(5, 10), (p, 0));
    }

    /// Pages of its own given back are written and read again as any.
    #[test]
    fn own_pages_forgotten() {
        let n = 16 * size();
        let p = map_own(n).unwrap();
        // SAFETY: the pages mapped, used by nothing else
        unsafe {
            let d = std::slice::from_raw_parts_mut(p, n);
            assert!(d.iter().all(|&b| b == 0));
            d.fill(7);
            forget(p.add(size()), 4 * size());
            d[size()..5 * size()].fill(9);
            assert!(d[..size()].iter().all(|&b| b == 7) && d[size()..5 * size()].iter().all(|&b| b == 9));
            assert!(d[5 * size()..].iter().all(|&b| b == 7));
            unmap_own(p, n);
        }
    }
}
