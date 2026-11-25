//! Copies on write on Windows: working memories made of placeholders ([`Creator`], for the
//! memories modules define, which the working memories of `wasm` are made as), so that the
//! accessible part of one can be given back to a placeholder and replaced by a view of a
//! snapshot (a section of the paging file, [`Section`]) mapped copied on write ([`map_over`]).
//! Placeholders are of Windows 10 1803 on: their functions are looked up ([`available`]), so that
//! the decoder starts on systems before it too (whose working memories are copied).

use std::collections::HashMap;
use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::{Arc, LazyLock, Mutex};

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleA, GetProcAddress};
use windows_sys::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MEM_COMMIT, MEM_EXTENDED_PARAMETER, MEM_RELEASE,
    MEM_REPLACE_PLACEHOLDER, MEM_RESERVE, MEM_RESERVE_PLACEHOLDER, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    PAGE_NOACCESS, PAGE_READWRITE, PAGE_WRITECOPY, UnmapViewOfFile, VirtualFree,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::core::BOOL;

// (of VirtualFree, not in windows-sys)
const MEM_COALESCE_PLACEHOLDERS: u32 = 0x1;
const MEM_PRESERVE_PLACEHOLDER: u32 = 0x2;

// The functions of placeholders (memoryapi.h), of kernelbase.dll.
type VirtualAlloc2 =
    unsafe extern "system" fn(HANDLE, *const c_void, usize, u32, u32, *mut MEM_EXTENDED_PARAMETER, u32) -> *mut c_void;
type MapViewOfFile3 = unsafe extern "system" fn(
    HANDLE,
    HANDLE,
    *const c_void,
    u64,
    usize,
    u32,
    u32,
    *mut MEM_EXTENDED_PARAMETER,
    u32,
) -> MEMORY_MAPPED_VIEW_ADDRESS;
type UnmapViewOfFile2 = unsafe extern "system" fn(HANDLE, MEMORY_MAPPED_VIEW_ADDRESS, u32) -> BOOL;
/// A function as GetProcAddress gives it.
type Proc = unsafe extern "system" fn() -> isize;

struct Placeholders {
    virtual_alloc2: VirtualAlloc2,
    map_view_of_file3: MapViewOfFile3,
    unmap_view_of_file2: UnmapViewOfFile2,
}

static PLACEHOLDERS: LazyLock<Option<Placeholders>> = LazyLock::new(|| {
    // SAFETY: of a library every process has loaded; the functions found are of the types above
    unsafe {
        let m = GetModuleHandleA(c"kernelbase.dll".as_ptr().cast());
        if m.is_null() {
            return None;
        }
        let f = |name: &std::ffi::CStr| GetProcAddress(m, name.as_ptr().cast());
        Some(Placeholders {
            virtual_alloc2: std::mem::transmute::<Proc, VirtualAlloc2>(f(c"VirtualAlloc2")?),
            map_view_of_file3: std::mem::transmute::<Proc, MapViewOfFile3>(f(c"MapViewOfFile3")?),
            unmap_view_of_file2: std::mem::transmute::<Proc, UnmapViewOfFile2>(f(c"UnmapViewOfFile2")?),
        })
    }
});

/// Whether the system has placeholders, which [`Creator`] makes its memories of.
pub fn available() -> bool {
    PLACEHOLDERS.is_some()
}

/// The functions of placeholders, of a memory of [`Creator`] (made only where [`available`]).
fn placeholders() -> &'static Placeholders {
    PLACEHOLDERS.as_ref().expect("the functions of placeholders")
}

/// The granularity of placeholders, as of allocations (that of wasm pages too).
const GRAIN: usize = 1 << 16;

/// A section of the paging file and a view of all of it, readable and writable.
pub struct Section {
    handle: OwnedHandle,
    pub ptr: *mut u8,
    pub len: usize,
}

impl Section {
    /// A section of `len` bytes (nonzero), of zeros.
    pub fn new(len: usize) -> Option<Section> {
        let n = len as u64;
        // SAFETY: a new section of no name
        let h = unsafe {
            CreateFileMappingW(INVALID_HANDLE_VALUE, null(), PAGE_READWRITE, (n >> 32) as u32, n as u32, null())
        };
        if h.is_null() {
            return None;
        }
        // SAFETY: a handle just made, owned by no one else
        let handle = unsafe { OwnedHandle::from_raw_handle(h) };
        // SAFETY: a new view of the section
        let v = unsafe { MapViewOfFile(h, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, len) };
        (!v.Value.is_null()).then(|| Section { handle, ptr: v.Value.cast(), len })
    }
}

impl Drop for Section {
    fn drop(&mut self) {
        // SAFETY: the view of `new`
        unsafe { UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.ptr.cast() }) };
    }
}

/// A piece of the accessible part of a memory, in order: committed, or a view.
struct Piece {
    at: usize,
    len: usize,
    view: bool,
}

/// A memory: its placeholder reserved (accessible part, then a placeholder to its end), of the
/// pieces it was made accessible by.
struct Pieces {
    base: usize,
    /// The bytes reserved, the guard included.
    reserved: usize,
    capacity: usize,
    size: usize,
    pieces: Vec<Piece>,
}

/// The memories made, by their bases, for [`map_over`].
static MEMORIES: LazyLock<Mutex<HashMap<usize, Arc<Mutex<Pieces>>>>> = LazyLock::new(Mutex::default);

impl Pieces {
    /// `len` bytes from `at` (the start of the placeholder ending the reservation) split off it.
    ///
    /// # Safety
    /// `at..at + len` is of that placeholder.
    unsafe fn split(&self, at: usize, len: usize) -> bool {
        // (the whole placeholder is no split)
        at + len == self.base + self.reserved
            || unsafe { VirtualFree(at as *mut c_void, len, MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER) } != 0
    }

    /// `len` bytes from `at`, a placeholder, committed.
    ///
    /// # Safety
    /// `at..at + len` is a placeholder of the memory's.
    unsafe fn commit(&mut self, at: usize, len: usize) -> bool {
        // SAFETY: of the placeholder
        let p = unsafe {
            (placeholders().virtual_alloc2)(
                GetCurrentProcess(),
                at as *const c_void,
                len,
                MEM_RESERVE | MEM_COMMIT | MEM_REPLACE_PLACEHOLDER,
                PAGE_READWRITE,
                null_mut(),
                0,
            )
        };
        if p.is_null() {
            return false;
        }
        self.pieces.push(Piece { at, len, view: false });
        true
    }

    /// The accessible part given back to a placeholder, of one piece.
    ///
    /// # Safety
    /// Nothing of the accessible part is in use.
    unsafe fn release(&mut self) -> bool {
        let n = self.pieces.len();
        for p in self.pieces.drain(..) {
            // SAFETY: of the piece, made by `commit` or `map_over`
            let ok = unsafe {
                if p.view {
                    (placeholders().unmap_view_of_file2)(
                        GetCurrentProcess(),
                        MEMORY_MAPPED_VIEW_ADDRESS { Value: p.at as *mut c_void },
                        MEM_PRESERVE_PLACEHOLDER,
                    ) != 0
                } else {
                    VirtualFree(p.at as *mut c_void, p.len, MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER) != 0
                }
            };
            assert!(ok, "a piece of a memory given back: {}", std::io::Error::last_os_error());
        }
        // SAFETY: of the placeholders just made, side by side
        n < 2
            || unsafe { VirtualFree(self.base as *mut c_void, self.size, MEM_RELEASE | MEM_COALESCE_PLACEHOLDERS) } != 0
    }
}

impl Drop for Pieces {
    fn drop(&mut self) {
        // SAFETY: of the pieces made, then what is left of the placeholder
        unsafe {
            for p in &self.pieces {
                if p.view {
                    UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: p.at as *mut c_void });
                } else {
                    VirtualFree(p.at as *mut c_void, 0, MEM_RELEASE);
                }
            }
            if self.size < self.reserved {
                VirtualFree((self.base + self.size) as *mut c_void, 0, MEM_RELEASE);
            }
        }
    }
}

/// A memory of [`Creator`].
struct Placeheld(Arc<Mutex<Pieces>>, usize);

impl Drop for Placeheld {
    fn drop(&mut self) {
        MEMORIES.lock().unwrap().remove(&self.1);
    }
}

impl Placeheld {
    fn size(&self) -> usize {
        self.0.lock().unwrap().size
    }

    fn capacity(&self) -> usize {
        self.0.lock().unwrap().capacity
    }

    /// The accessible part grown to `new_size` bytes (a multiple of grains, within the capacity).
    fn grow(&mut self, new_size: usize) -> Result<(), String> {
        let mut m = self.0.lock().unwrap();
        if new_size > m.capacity || !new_size.is_multiple_of(GRAIN) {
            return Err(format!("a memory of {new_size} bytes, of {} at most", m.capacity));
        }
        if new_size > m.size {
            let (at, len) = (m.base + m.size, new_size - m.size);
            // SAFETY: of the placeholder after the accessible part
            if !unsafe { m.split(at, len) && m.commit(at, len) } {
                return Err(format!("a memory grown to {new_size} bytes: {}", std::io::Error::last_os_error()));
            }
            m.size = new_size;
        }
        Ok(())
    }

    /// A memory of `minimum` bytes and a capacity of `reserved`, else `maximum`, else
    /// `unbounded`, guarded by `guard` bytes after it.
    fn new(
        minimum: usize,
        maximum: Option<usize>,
        reserved: Option<usize>,
        unbounded: usize,
        guard: usize,
    ) -> Result<Placeheld, String> {
        let capacity = reserved.unwrap_or(maximum.unwrap_or(unbounded)).max(minimum).next_multiple_of(GRAIN);
        // (a placeholder of a grain at least: memories of no byte too)
        let total = (capacity + guard.next_multiple_of(GRAIN)).max(GRAIN);
        // SAFETY: a new placeholder
        let base = unsafe {
            (placeholders().virtual_alloc2)(
                GetCurrentProcess(),
                null(),
                total,
                MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                PAGE_NOACCESS,
                null_mut(),
                0,
            )
        };
        if base.is_null() {
            return Err(format!("a placeholder of {total} bytes: {}", std::io::Error::last_os_error()));
        }
        let base = base as usize;
        let pieces = Arc::new(Mutex::new(Pieces { base, reserved: total, capacity, size: 0, pieces: Vec::new() }));
        let mut m = Placeheld(pieces.clone(), base);
        m.grow(minimum)?;
        MEMORIES.lock().unwrap().insert(base, pieces);
        Ok(m)
    }
}

/// Makes the memories modules define of placeholders, for Wasmtime and WAH.
pub struct Creator;

// SAFETY: page-aligned, a multiple of pages, and guarded by the placeholder after it
#[cfg(feature = "wasmtime")]
unsafe impl wasmtime::LinearMemory for Placeheld {
    fn byte_size(&self) -> usize {
        self.size()
    }

    fn byte_capacity(&self) -> usize {
        self.capacity()
    }

    fn grow_to(&mut self, new_size: usize) -> wasmtime::Result<()> {
        self.grow(new_size).map_err(wasmtime::Error::msg)
    }

    fn as_ptr(&self) -> *mut u8 {
        self.1 as *mut u8
    }
}

// SAFETY: memories of zeros, reserved as asked and guarded
#[cfg(feature = "wasmtime")]
unsafe impl wasmtime::MemoryCreator for Creator {
    fn new_memory(
        &self,
        _: wasmtime::MemoryType,
        minimum: usize,
        maximum: Option<usize>,
        reserved: Option<usize>,
        guard: usize,
    ) -> Result<Box<dyn wasmtime::LinearMemory>, String> {
        Ok(Box::new(Placeheld::new(minimum, maximum, reserved, minimum, guard)?))
    }
}

// SAFETY: as Wasmtime's
#[cfg(feature = "wah")]
unsafe impl wah::LinearMemory for Placeheld {
    fn byte_size(&self) -> usize {
        self.size()
    }

    fn byte_capacity(&self) -> usize {
        self.capacity()
    }

    fn grow_to(&mut self, new_size: usize) -> wah::Result<()> {
        self.grow(new_size).map_err(wah::Error::msg)
    }

    fn as_ptr(&self) -> *mut u8 {
        self.1 as *mut u8
    }
}

// SAFETY: as Wasmtime's
#[cfg(feature = "wah")]
unsafe impl wah::MemoryCreator for Creator {
    fn new_memory(
        &self,
        ty: wah::MemoryType,
        minimum: usize,
        maximum: Option<usize>,
        reserved: Option<usize>,
        guard: usize,
    ) -> Result<Box<dyn wah::LinearMemory>, String> {
        // (WAH reserves nothing without guard pages: a 32-bit memory of no maximum reserves 4 GiB)
        let unbounded = if ty.is_64() { minimum } else { 1 << 32 };
        Ok(Box::new(Placeheld::new(minimum, maximum, reserved, unbounded, guard)?))
    }
}

/// What [`map_over`] did.
pub enum Mapped {
    /// The memory is a copy on write of the section.
    Yes,
    /// The memory is not of [`Creator`], or not of the section's length: left as it was.
    No,
    /// The memory was given back but not mapped: of zeros, committed again.
    Zeroed,
}

/// `mem`, all the accessible part of a memory of [`Creator`], replaced by a view of `s` copied
/// on write.
pub fn map_over(s: &Section, mem: &mut [u8]) -> Mapped {
    let Some(m) = MEMORIES.lock().unwrap().get(&(mem.as_ptr() as usize)).cloned() else { return Mapped::No };
    let mut m = m.lock().unwrap();
    if m.size != mem.len() || s.len != mem.len() || mem.is_empty() {
        return Mapped::No;
    }
    let (base, len) = (m.base, m.size);
    // SAFETY: `mem`, borrowed mutably, is all of the accessible part; the placeholder made is
    // replaced by the view, or committed again
    unsafe {
        if !m.release() {
            panic!("a memory given back: {}", std::io::Error::last_os_error());
        }
        let v = (placeholders().map_view_of_file3)(
            s.handle.as_raw_handle(),
            GetCurrentProcess(),
            base as *const c_void,
            0,
            len,
            MEM_REPLACE_PLACEHOLDER,
            PAGE_WRITECOPY,
            null_mut(),
            0,
        );
        if !v.Value.is_null() {
            m.pieces.push(Piece { at: base, len, view: true });
            return Mapped::Yes;
        }
        assert!(m.commit(base, len), "a memory committed again: {}", std::io::Error::last_os_error());
    }
    Mapped::Zeroed
}
