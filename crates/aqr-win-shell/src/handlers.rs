//! The system's handlers of a file's type for files of archives: what the shell finds through
//! a file's associations ([`associations`]: its extension, its perceived type, `*`, as of a file
//! of its name), and thumbnails ([`thumbnail`]) by the type's thumbnail provider, given the
//! file's bytes as a stream (`IInitializeWithStream`) as the shell gives it a file's. The preview pane
//! finds its handler by the associations, and reads the file by `BHID_Stream` itself.

use std::path::Path;
use std::sync::Mutex;

use windows::Win32::Foundation::{FILETIME, SIZE};
use windows::Win32::Graphics::Gdi::HBITMAP;
use windows::Win32::UI::Shell::{
    IEIFLAG_ASYNC, IExtractImage_Impl, IExtractImage2, IExtractImage2_Impl, WTS_ALPHATYPE,
};
use windows::core::implement;

use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CLSIDFromString, CoCreateInstance, STGM_READ};
use windows::Win32::UI::Shell::PropertiesSystem::IInitializeWithStream;
use windows::Win32::UI::Shell::{
    ASSOCCLASS_PROGID_STR, ASSOCCLASS_STAR, ASSOCCLASS_SYSTEM_STR, ASSOCF_NONE, ASSOCIATIONELEMENT,
    ASSOCSTR_SHELLEXTENSION, AssocCreateForClasses, AssocGetPerceivedType, IQueryAssociations, IThumbnailProvider,
    SHCreateMemStream,
};
use windows::core::{GUID, HSTRING, PCWSTR, PWSTR};

use crate::client;
use crate::pidl::Item;

/// The associations of a file named as `item`: of its extension (`HKCR\.ext`, and the ProgID it
/// names), of `SystemFileAssociations\.ext`, of its perceived type (`SystemFileAssociations\image`),
/// and `*`.
pub fn associations(item: &Item) -> windows::core::Result<IQueryAssociations> {
    let ext = Path::new(&item.name).extension().map(|e| format!(".{}", e.to_string_lossy()));
    let ext = ext.map(HSTRING::from);
    // (`image`, `video`, ...: `SystemFileAssociations\image`, where many types' handlers are)
    let perceived = ext.as_ref().and_then(|e| {
        let (mut kind, mut flag, mut text) = (Default::default(), 0, PWSTR::null());
        // SAFETY: an extension, and the type's name of COM's memory, freed after
        unsafe {
            AssocGetPerceivedType(e, &mut kind, &mut flag, Some(&mut text)).ok()?;
            let t = text.to_string().ok();
            windows::Win32::System::Com::CoTaskMemFree(Some(text.0 as *const _));
            t.map(HSTRING::from)
        }
    });
    let mut elements = Vec::new();
    if let Some(e) = &ext {
        elements.push(ASSOCIATIONELEMENT {
            ac: ASSOCCLASS_PROGID_STR,
            hkClass: Default::default(),
            pszClass: PCWSTR(e.as_ptr()),
        });
        elements.push(ASSOCIATIONELEMENT {
            ac: ASSOCCLASS_SYSTEM_STR,
            hkClass: Default::default(),
            pszClass: PCWSTR(e.as_ptr()),
        });
    }
    if let Some(p) = &perceived {
        elements.push(ASSOCIATIONELEMENT {
            ac: ASSOCCLASS_SYSTEM_STR,
            hkClass: Default::default(),
            pszClass: PCWSTR(p.as_ptr()),
        });
    }
    elements.push(ASSOCIATIONELEMENT { ac: ASSOCCLASS_STAR, hkClass: Default::default(), pszClass: PCWSTR::null() });
    // SAFETY: elements of strings alive for the call
    unsafe { AssocCreateForClasses(&elements) }
}

/// The class of the type's shell extension `handler` (`{...}` under `ShellEx`), where it has one.
fn handler_of(item: &Item, handler: &str) -> Option<GUID> {
    let a = associations(item).ok()?;
    let h = HSTRING::from(handler);
    let mut buf = [0u16; 64];
    let mut n = buf.len() as u32;
    // SAFETY: a buffer of `n` characters, and its length to tell
    unsafe { a.GetString(ASSOCF_NONE, ASSOCSTR_SHELLEXTENSION, &h, Some(PWSTR(buf.as_mut_ptr())), &mut n).ok()? };
    // SAFETY: the class's string, ended by 0
    unsafe { CLSIDFromString(PCWSTR(buf.as_ptr())).ok() }
}

/// `IThumbnailProvider`'s handler, under `ShellEx`.
const THUMBNAIL_PROVIDER: &str = "{e357fccd-a995-4576-b01f-234630154e96}";

/// The most bytes of a file a thumbnail is made of (read whole first).
const THUMBNAIL_MAX: u64 = 64 << 20;

/// A thumbnail provider of file `at` of archive `path` (`item`): the type's, given the file's
/// bytes as a stream in memory (read first: a provider may read it from threads of its own,
/// which a stream of another apartment would hold up); none where the type has none taking a
/// stream, or the file is over [`THUMBNAIL_MAX`].
pub fn thumbnail(path: &Path, at: &str, item: &Item) -> windows::core::Result<IThumbnailProvider> {
    let none = || windows::core::Error::from(windows::Win32::Foundation::E_NOINTERFACE);
    if item.size > THUMBNAIL_MAX {
        return Err(none());
    }
    let class = handler_of(item, THUMBNAIL_PROVIDER).ok_or_else(none)?;
    let bytes = client::read_all(path, at, THUMBNAIL_MAX)?;
    // SAFETY: a copy of the bytes, the stream's
    let stream = unsafe { SHCreateMemStream(Some(&bytes)) }.ok_or_else(none)?;
    // SAFETY: the type's handler, in this process as the shell takes it
    let init: IInitializeWithStream = unsafe { CoCreateInstance(&class, None, CLSCTX_INPROC_SERVER)? };
    // SAFETY: a stream of the file's, read only
    unsafe { init.Initialize(&stream, STGM_READ.0)? };
    windows::core::Interface::cast(&init)
}

/// A file's picture as the shell's image factory asks for it (`IExtractImage`: what Explorer's
/// views take, at each size of theirs): the size asked for taken by `GetLocation`, the picture
/// made by `Extract` of the type's thumbnail provider ([`thumbnail`]); dated by the file's time,
/// for the shell's cache.
#[implement(IExtractImage2)]
pub struct Extract {
    archive: std::sync::Arc<client::Use>,
    at: String,
    item: Item,
    size: Mutex<u32>,
    _counted: crate::Counted,
}

impl Extract {
    pub fn new(archive: std::sync::Arc<client::Use>, at: String, item: Item) -> Extract {
        Extract { archive, at, item, size: Mutex::new(96), _counted: crate::Counted::new() }
    }
}

impl IExtractImage_Impl for Extract_Impl {
    fn GetLocation(
        &self,
        buf: PWSTR,
        n: u32,
        _: *mut u32,
        size: *const SIZE,
        _: u32,
        flags: *mut u32,
    ) -> windows::core::Result<()> {
        // SAFETY: the caller's size, buffer of `n` characters, and flags
        unsafe {
            if let Some(s) = size.as_ref() {
                *self.size.lock().unwrap() = s.cx.max(s.cy).max(1) as u32;
            }
            // (where it is: what the shell keys the picture by)
            if !buf.is_null() && n > 0 {
                let place: Vec<u16> = format!("{}\\{}", self.archive.path().display(), self.at)
                    .encode_utf16()
                    .take(n as usize - 1)
                    .collect();
                std::ptr::copy_nonoverlapping(place.as_ptr(), buf.0, place.len());
                *buf.0.add(place.len()) = 0;
            }
            if !flags.is_null() {
                // (made at once as asked: not of a thread of the caller's)
                *flags &= !IEIFLAG_ASYNC;
            }
        }
        Ok(())
    }

    fn Extract(&self) -> windows::core::Result<HBITMAP> {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let t = thumbnail(self.archive.path(), &self.at, &self.item)?;
            let (mut bmp, mut alpha) = (HBITMAP::default(), WTS_ALPHATYPE::default());
            // SAFETY: places of the picture and its kind
            unsafe { t.GetThumbnail(*self.size.lock().unwrap(), &mut bmp, &mut alpha)? };
            Ok(bmp)
        }));
        let r = r.unwrap_or_else(|_| Err(windows::Win32::Foundation::E_UNEXPECTED.into()));
        if let Err(e) = &r {
            crate::log(|| format!("Extract {}: {e}", self.at));
        }
        r
    }
}

impl IExtractImage2_Impl for Extract_Impl {
    fn GetDateStamp(&self) -> windows::core::Result<FILETIME> {
        let t = self.item.mtime;
        Ok(FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t as u64 >> 32) as u32 })
    }
}
