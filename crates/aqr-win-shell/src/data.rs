//! What a drop into another program than the shell takes of entries of an archive (a browser's
//! upload, a mail's attachment): the files as `CFSTR_FILEDESCRIPTORW` (each with its path from
//! the folder, those of the folders chosen and under them too) and `CFSTR_FILECONTENTS` (a
//! stream of each, by its index). The inner object of the shell's data object
//! (`SHCreateDataObject`), which has the items' ID lists the shell takes.

use std::sync::{Arc, OnceLock};

use windows::Win32::Foundation::{
    DV_E_FORMATETC, DV_E_LINDEX, DV_E_TYMED, E_NOTIMPL, E_POINTER, OLE_E_ADVISENOTSUPPORTED, S_OK,
};
use windows::Win32::System::Com::{
    DVASPECT_CONTENT, FORMATETC, IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, STGMEDIUM,
    STGMEDIUM_0, TYMED_HGLOBAL, TYMED_ISTREAM,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GMEM_ZEROINIT, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::UI::Shell::{CFSTR_FILECONTENTS, CFSTR_FILEDESCRIPTORW, FILEDESCRIPTORW, SHCreateStdEnumFmtEtc};
use windows::core::{BOOL, HRESULT, Ref, implement};

use crate::client;
use crate::pidl::Item;

/// A file or folder dropped: its path in the archive, its path from the folder (with `\`), and
/// what it is.
struct Dropped {
    path: String,
    shown: String,
    item: Item,
}

#[implement(IDataObject)]
pub struct Files {
    archive: Arc<client::Use>,
    /// The folder's path in the archive, and the entries chosen.
    dir: String,
    chosen: Vec<Item>,
    /// The entries chosen and those under the folders of them, as they are first asked for.
    all: OnceLock<Result<Vec<Dropped>, HRESULT>>,
    _counted: crate::Counted,
}

fn formats() -> (u16, u16) {
    // SAFETY: names of formats, ended by 0
    unsafe {
        (RegisterClipboardFormatW(CFSTR_FILEDESCRIPTORW) as u16, RegisterClipboardFormatW(CFSTR_FILECONTENTS) as u16)
    }
}

impl Files {
    pub fn new(archive: Arc<client::Use>, dir: String, chosen: Vec<Item>) -> Files {
        Files { archive, dir, chosen, all: OnceLock::new(), _counted: crate::Counted::new() }
    }

    fn all(&self) -> Result<&[Dropped], HRESULT> {
        let r = self.all.get_or_init(|| {
            let mut out = Vec::new();
            let mut stack: Vec<Dropped> = self
                .chosen
                .iter()
                .rev()
                .map(|i| Dropped {
                    path: crate::folder::joined(&self.dir, &i.name),
                    shown: i.name.clone(),
                    item: i.clone(),
                })
                .collect();
            while let Some(d) = stack.pop() {
                if d.item.is_dir() {
                    let under = client::list(self.archive.path(), &d.path).map_err(|e| e.code())?;
                    for e in under.iter().rev() {
                        let i = Item::of(e);
                        stack.push(Dropped {
                            path: format!("{}/{}", d.path, i.name),
                            shown: format!("{}\\{}", d.shown, i.name),
                            item: i,
                        });
                    }
                }
                out.push(d);
            }
            Ok(out)
        });
        r.as_deref().map_err(|e| *e)
    }

    /// The descriptors of all, in an `HGLOBAL`.
    fn descriptors(&self) -> windows::core::Result<STGMEDIUM> {
        let all = self.all()?;
        let size = std::mem::size_of::<FILEDESCRIPTORW>();
        // SAFETY: memory of its size, filled before it is given
        unsafe {
            let h = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, 4 + size * all.len())?;
            let p = GlobalLock(h) as *mut u8;
            (p as *mut u32).write_unaligned(all.len() as u32);
            for (k, d) in all.iter().enumerate() {
                let mut f = FILEDESCRIPTORW {
                    // FD_ATTRIBUTES | FD_WRITESTIME | FD_PROGRESSUI | FD_UNICODE, FD_FILESIZE of files
                    dwFlags: 0x4 | 0x20 | 0x4000 | 0x8000_0000 | if d.item.is_dir() { 0 } else { 0x40 },
                    dwFileAttributes: match d.item.attrs & 0x27 {
                        _ if d.item.is_dir() => 0x10,
                        0 => 0x80, // FILE_ATTRIBUTE_NORMAL
                        a => a,
                    },
                    nFileSizeHigh: (d.item.size >> 32) as u32,
                    nFileSizeLow: d.item.size as u32,
                    ..Default::default()
                };
                f.ftLastWriteTime.dwLowDateTime = d.item.mtime as u32;
                f.ftLastWriteTime.dwHighDateTime = (d.item.mtime as u64 >> 32) as u32;
                let w: Vec<u16> = d.shown.encode_utf16().take(259).collect();
                let mut name = [0u16; 260];
                name[..w.len()].copy_from_slice(&w);
                f.cFileName = name;
                (p.add(4 + k * size) as *mut FILEDESCRIPTORW).write_unaligned(f);
            }
            _ = GlobalUnlock(h);
            Ok(STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 { hGlobal: h },
                pUnkForRelease: std::mem::ManuallyDrop::new(None),
            })
        }
    }

    /// A stream of the `k`th of all, a file.
    fn contents(&self, k: i32) -> windows::core::Result<STGMEDIUM> {
        let d = self.all()?.get(k as usize).filter(|_| k >= 0).ok_or(windows::core::Error::from(DV_E_LINDEX))?;
        if d.item.is_dir() {
            return Err(DV_E_LINDEX.into());
        }
        let s = client::stream(self.archive.path(), &d.path)?;
        Ok(STGMEDIUM {
            tymed: TYMED_ISTREAM.0 as u32,
            u: STGMEDIUM_0 { pstm: std::mem::ManuallyDrop::new(Some(s)) },
            pUnkForRelease: std::mem::ManuallyDrop::new(None),
        })
    }
}

impl IDataObject_Impl for Files_Impl {
    fn GetData(&self, f: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // SAFETY: the caller's FORMATETC
            let f = unsafe { f.as_ref() }.ok_or(windows::core::Error::from(E_POINTER))?;
            let (descriptor, contents) = formats();
            match f.cfFormat {
                c if c == descriptor && f.tymed & TYMED_HGLOBAL.0 as u32 != 0 => self.descriptors(),
                c if c == contents && f.tymed & TYMED_ISTREAM.0 as u32 != 0 => self.contents(f.lindex),
                c if c == descriptor || c == contents => Err(DV_E_TYMED.into()),
                _ => Err(DV_E_FORMATETC.into()),
            }
        }));
        let r = r.unwrap_or_else(|_| Err(windows::Win32::Foundation::E_UNEXPECTED.into()));
        if let Err(e) = &r {
            crate::log(|| format!("GetData: {e}"));
        }
        r
    }

    fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, f: *const FORMATETC) -> HRESULT {
        // SAFETY: the caller's FORMATETC
        let Some(f) = (unsafe { f.as_ref() }) else { return E_POINTER };
        let (descriptor, contents) = formats();
        match f.cfFormat {
            c if c == descriptor && f.tymed & TYMED_HGLOBAL.0 as u32 != 0 => S_OK,
            c if c == contents && f.tymed & TYMED_ISTREAM.0 as u32 != 0 => S_OK,
            c if c == descriptor || c == contents => DV_E_TYMED,
            _ => DV_E_FORMATETC,
        }
    }

    fn GetCanonicalFormatEtc(&self, _: *const FORMATETC, out: *mut FORMATETC) -> HRESULT {
        if !out.is_null() {
            // SAFETY: the caller's FORMATETC
            unsafe { (*out).ptd = std::ptr::null_mut() };
        }
        HRESULT(0x0004_0130) // DATA_S_SAMEFORMATETC
    }

    fn SetData(&self, _: *const FORMATETC, _: *const STGMEDIUM, _: BOOL) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn EnumFormatEtc(&self, direction: u32) -> windows::core::Result<IEnumFORMATETC> {
        if direction != 1 {
            // (DATADIR_GET alone)
            return Err(E_NOTIMPL.into());
        }
        let (descriptor, contents) = formats();
        let f = |c: u16, tymed: u32, lindex: i32| FORMATETC {
            cfFormat: c,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex,
            tymed,
        };
        // SAFETY: an array of its own
        unsafe {
            SHCreateStdEnumFmtEtc(&[f(descriptor, TYMED_HGLOBAL.0 as u32, -1), f(contents, TYMED_ISTREAM.0 as u32, -1)])
        }
    }

    fn DAdvise(&self, _: *const FORMATETC, _: u32, _: Ref<IAdviseSink>) -> windows::core::Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _: u32) -> windows::core::Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}
