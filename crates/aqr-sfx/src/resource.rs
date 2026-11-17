//! The pack that aqr carries (`aqr-sfx-pack`'s `sfx.pack`, with its `pack.id`), as resources of
//! its executable (`RT_RCDATA`, [`PACK`] and [`ID`]): written into it after it is linked
//! ([`embed`], `aqr-sfx-pack --embed`), so that aqr is built without waiting for the pack, and
//! read in place as it runs ([`embedded`]: the resource section is of its image, mapped).

use std::path::Path;

/// The resource of the pack (`sfx.pack`).
pub const PACK: &str = "AQRSFXPACK";
/// The resource of the pack's id (`pack.id`: a digest of its files, as text).
pub const ID: &str = "AQRSFXID";

/// `RT_RCDATA`
const RCDATA: usize = 10;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn BeginUpdateResourceW(file: *const u16, delete_existing: i32) -> *mut core::ffi::c_void;
    fn UpdateResourceW(
        update: *mut core::ffi::c_void,
        kind: *const u16,
        name: *const u16,
        language: u16,
        data: *const core::ffi::c_void,
        len: u32,
    ) -> i32;
    fn EndUpdateResourceW(update: *mut core::ffi::c_void, discard: i32) -> i32;
    fn FindResourceW(module: *mut core::ffi::c_void, name: *const u16, kind: *const u16) -> *mut core::ffi::c_void;
    fn LoadResource(module: *mut core::ffi::c_void, info: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    fn LockResource(data: *mut core::ffi::c_void) -> *const u8;
    fn SizeofResource(module: *mut core::ffi::c_void, info: *mut core::ffi::c_void) -> u32;
}

/// Writes `pack` and `id` into the executable at `exe` as the resources [`PACK`] and [`ID`] (in
/// place of those it has; its other resources kept).
pub fn embed(exe: &Path, pack: &[u8], id: &str) -> Result<(), String> {
    let fail = |what: &str| format!("{}: {what}: {}", exe.display(), std::io::Error::last_os_error());
    let path: Vec<u16> = exe.as_os_str().encode_wide_nul();
    // SAFETY: a NUL-terminated path; the handle used until it is ended
    unsafe {
        let h = BeginUpdateResourceW(path.as_ptr(), 0);
        if h.is_null() {
            return Err(fail("its resources not opened"));
        }
        for (name, data) in [(PACK, pack), (ID, id.as_bytes())] {
            let n = wide(name);
            if UpdateResourceW(h, RCDATA as *const u16, n.as_ptr(), 0, data.as_ptr().cast(), data.len() as u32) == 0 {
                let e = fail(name);
                EndUpdateResourceW(h, 1);
                return Err(e);
            }
        }
        if EndUpdateResourceW(h, 0) == 0 {
            return Err(fail("its resources not written"));
        }
    }
    Ok(())
}

/// The resource `name` of this program's executable, in place (empty where it has none).
fn resource(name: &str) -> &'static [u8] {
    let n = wide(name);
    // SAFETY: this program's module (null); a resource's data stays mapped as long as it runs
    unsafe {
        let module = std::ptr::null_mut();
        let info = FindResourceW(module, n.as_ptr(), RCDATA as *const u16);
        if info.is_null() {
            return &[];
        }
        let (data, len) = (LockResource(LoadResource(module, info)), SizeofResource(module, info));
        if data.is_null() { &[] } else { std::slice::from_raw_parts(data, len as usize) }
    }
}

/// The pack and its id that this program's executable carries ([`embed`]); empty where it carries
/// none.
pub fn embedded() -> (&'static [u8], &'static str) {
    (resource(PACK), std::str::from_utf8(resource(ID)).unwrap_or("").trim())
}

trait WideNul {
    fn encode_wide_nul(&self) -> Vec<u16>;
}

impl WideNul for std::ffi::OsStr {
    fn encode_wide_nul(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain([0]).collect()
    }
}
