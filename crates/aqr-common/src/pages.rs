//! Pages of files mapped given back to the system as they are no longer needed, so that the
//! resident set holds what is in use rather than all that was ever touched: the spill's
//! regions dropped ([`discard`]: their bytes of no use), the inputs read through ([`let_go`]:
//! read again from their files where touched again). Only the pages wholly within the bytes
//! told are given back; a page shared with bytes still in use is left.

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
/// `base` is the start of a mapping (shared, writable) of all of `file`, `at..at + len` within
/// it, and nothing reads or writes those bytes until they are written again.
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
}
