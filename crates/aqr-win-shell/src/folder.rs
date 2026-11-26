//! A folder of an archive: the archive file itself (a junction: the shell initializes it with
//! the file's ID list) or a directory in it (bound to from its parent), its entries listed by
//! the server, their columns of what [`crate::pidl::Item`]s hold, the view the shell's own
//! (DefView).

use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use windows::Win32::Foundation::{
    E_FAIL, E_INVALIDARG, E_NOINTERFACE, E_NOTIMPL, E_POINTER, FILETIME, HWND, LPARAM, PROPERTYKEY, S_FALSE, S_OK,
    SysAllocStringLen,
};
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES};
use windows::Win32::System::Com::{IBindCtx, IPersist_Impl, IStream};
use windows::Win32::System::Variant::{InitVariantFromFileTime, VARIANT};
use windows::Win32::UI::Shell::Common::{
    ITEMIDLIST, SHCOLSTATE, SHCOLSTATE_ONBYDEFAULT, SHCOLSTATE_SECONDARYUI, SHCOLSTATE_TYPE_DATE, SHCOLSTATE_TYPE_INT,
    SHCOLSTATE_TYPE_STR, SHELLDETAILS, STRRET,
};
use windows::Win32::UI::Shell::{
    IEnumExtraSearch, IEnumIDList, IEnumIDList_Impl, IExtractIconW, ILClone, ILCombine, ILFree, IPersistFolder_Impl,
    IPersistFolder2_Impl, IPersistFolder3, IPersistFolder3_Impl, IPersistIDList, IPersistIDList_Impl,
    IShellFolder_Impl, IShellFolder2, IShellFolder2_Impl, IShellView, PERSIST_FOLDER_TARGET_INFO, SFV_CREATE,
    SHCreateFileExtractIconW, SHCreateShellFolderView, SHFILEINFOW, SHFormatDateTimeW, SHGDN_FORPARSING,
    SHGDN_INFOLDER, SHGDNF, SHGFI_TYPENAME, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW, SHGetNameFromIDList, SHStrDupW,
    SIGDN_FILESYSPATH, StrCmpLogicalW, StrFormatKBSizeW,
};
use windows::core::{GUID, HRESULT, IUnknownImpl, Interface, PCWSTR, Ref, implement};

use crate::client;
use crate::pidl::{self, Item, Kind};

/// The class of the folders of archives.
pub const FOLDER: GUID = GUID::from_u128(0xf76e4146_e32e_4811_a0de_465ae547da1d);

/// The class of the folders of archives in tests (`.aqrtest` files), served by the server of
/// [`TEST_SERVER`], which the test starts itself.
pub const TEST_FOLDER: GUID = GUID::from_u128(0x2b0c7a35_8d4e_4f61_9a3c_6e1d5f7b8c92);
pub const TEST_SERVER: GUID = GUID::from_u128(0x5e1f0b6a_92c4_4d1e_a7f3_0c2b9d8e7a61);

// (the attributes of an item, `SFGAO_*`)
const CANCOPY: u32 = 0x1;
const READONLY: u32 = 0x4_0000;
const HIDDEN: u32 = 0x8_0000;
const STREAM: u32 = 0x40_0000;
const FOLDER_ATTR: u32 = 0x2000_0000;
const HASSUBFOLDER: u32 = 0x8000_0000;

/// `HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)`.
const NOT_FOUND: HRESULT = HRESULT(0x8007_0002_u32 as i32);

// (`SHCONTF_*`)
const SHCONTF_FOLDERS: u32 = 0x20;
const SHCONTF_NONFOLDERS: u32 = 0x40;
const SHCONTF_INCLUDEHIDDEN: u32 = 0x80;

/// The storage properties (`FMTID_Storage`) of the columns.
const STORAGE: GUID = GUID::from_u128(0xb725f130_47ef_101a_a5f1_02608c9eebac);

/// The columns: their names (where the property system does not name them), what they are, the
/// width in characters, and the property.
struct Column {
    title: &'static str,
    state: SHCOLSTATE,
    width: i32,
    right: bool,
    pid: u32,
}

const COLUMNS: [Column; 5] = [
    Column {
        title: "Name",
        state: SHCOLSTATE(SHCOLSTATE_TYPE_STR.0 | SHCOLSTATE_ONBYDEFAULT.0),
        width: 30,
        right: false,
        pid: 10,
    },
    Column {
        title: "Date modified",
        state: SHCOLSTATE(SHCOLSTATE_TYPE_DATE.0 | SHCOLSTATE_ONBYDEFAULT.0),
        width: 18,
        right: false,
        pid: 14,
    },
    Column {
        title: "Type",
        state: SHCOLSTATE(SHCOLSTATE_TYPE_STR.0 | SHCOLSTATE_ONBYDEFAULT.0),
        width: 18,
        right: false,
        pid: 4,
    },
    Column {
        title: "Size",
        state: SHCOLSTATE(SHCOLSTATE_TYPE_INT.0 | SHCOLSTATE_ONBYDEFAULT.0),
        width: 10,
        right: true,
        pid: 12,
    },
    Column {
        title: "Attributes",
        state: SHCOLSTATE(SHCOLSTATE_TYPE_STR.0 | SHCOLSTATE_SECONDARYUI.0),
        width: 6,
        right: false,
        pid: 13,
    },
];

/// An ID list of COM's memory, owned.
pub struct Pidl(*mut ITEMIDLIST);

// SAFETY: memory of COM's, of no thread
unsafe impl Send for Pidl {}
unsafe impl Sync for Pidl {}

impl Pidl {
    fn as_ptr(&self) -> *const ITEMIDLIST {
        self.0
    }

    /// A copy, the caller's.
    fn cloned(&self) -> *mut ITEMIDLIST {
        // SAFETY: an ID list of its own
        unsafe { ILClone(self.0) }
    }
}

impl Drop for Pidl {
    fn drop(&mut self) {
        // SAFETY: of its own
        unsafe { ILFree(Some(self.0)) };
    }
}

/// Where a folder is.
struct At {
    /// Its absolute ID list.
    pidl: Pidl,
    archive: Arc<client::Use>,
    /// Its path in the archive, names joined with `/` (empty: the root).
    dir: String,
}

#[implement(IShellFolder2, IPersistFolder3, IPersistIDList)]
pub struct Folder {
    clsid: GUID,
    at: RwLock<Option<At>>,
    _counted: crate::Counted,
}

/// A panic caught (as the process is another program's) as a failure.
fn guard<T>(what: &str, f: impl FnOnce() -> windows::core::Result<T>) -> windows::core::Result<T> {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(windows::Win32::Foundation::E_UNEXPECTED.into()));
    if let Err(e) = &r {
        crate::log(|| format!("{what}: {e}"));
    }
    r
}

fn guard_hr(what: &str, f: impl FnOnce() -> HRESULT) -> HRESULT {
    let r =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or(windows::Win32::Foundation::E_UNEXPECTED);
    if r.is_err() {
        crate::log(|| format!("{what}: {r:?}"));
    }
    r
}

/// `path` with `name` after it.
fn joined(path: &str, name: &str) -> String {
    if path.is_empty() { name.to_owned() } else { format!("{path}/{name}") }
}

/// A string of COM's memory, for a `STRRET`.
fn strret(s: &str) -> windows::core::Result<STRRET> {
    let w: Vec<u16> = s.encode_utf16().chain([0]).collect();
    let mut r = STRRET { uType: 0, ..Default::default() }; // STRRET_WSTR
    // SAFETY: a string ended by 0
    r.Anonymous.pOleStr = unsafe { SHStrDupW(PCWSTR(w.as_ptr()))? };
    Ok(r)
}

/// `VARIANT`, as it is laid out.
#[repr(C)]
struct Var {
    vt: u16,
    reserved: [u16; 3],
    value: u64,
    record: usize,
}

fn variant(vt: u16, value: u64) -> VARIANT {
    // SAFETY: of the same layout
    unsafe { std::mem::transmute(Var { vt, reserved: [0; 3], value, record: 0 }) }
}

fn variant_text(s: &str) -> VARIANT {
    let w: Vec<u16> = s.encode_utf16().collect();
    // SAFETY: a BSTR made of `w`, the VARIANT's
    let b = unsafe { SysAllocStringLen(Some(&w)) };
    variant(8, unsafe { std::mem::transmute::<windows::core::BSTR, usize>(b) } as u64)
}

/// What Explorer calls the type of `item` (of its extension, as of a file of that name).
fn type_text(item: &Item) -> String {
    let w: Vec<u16> = item.name.encode_utf16().chain([0]).collect();
    let mut info = SHFILEINFOW::default();
    // SAFETY: a name ended by 0, and a structure of its size
    unsafe {
        SHGetFileInfoW(
            PCWSTR(w.as_ptr()),
            file_attributes(item),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES,
        )
    };
    let n = info.szTypeName.iter().position(|&c| c == 0).unwrap_or(info.szTypeName.len());
    String::from_utf16_lossy(&info.szTypeName[..n])
}

fn file_attributes(item: &Item) -> FILE_FLAGS_AND_ATTRIBUTES {
    match item.is_dir() {
        true => FILE_ATTRIBUTE_DIRECTORY,
        false => FILE_FLAGS_AND_ATTRIBUTES(FILE_ATTRIBUTE_NORMAL.0 | (item.attrs & 0x7)),
    }
}

fn filetime(item: &Item) -> FILETIME {
    FILETIME { dwLowDateTime: item.mtime as u32, dwHighDateTime: (item.mtime as u64 >> 32) as u32 }
}

/// Column `col` of `item` as it is shown.
fn shown(item: &Item, col: usize) -> String {
    match col {
        0 => item.name.clone(),
        1 if item.mtime != 0 => {
            let mut buf = [0u16; 128];
            // FDTF_SHORTTIME | FDTF_SHORTDATE | FDTF_NOAUTOREADINGORDER (no marks of direction)
            let mut flags = 0x1 | 0x2 | 0x400;
            // SAFETY: a FILETIME and a buffer of its length
            unsafe { SHFormatDateTimeW(&filetime(item), Some(&mut flags), &mut buf) };
            String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(0)])
        }
        2 => type_text(item),
        3 if item.kind == Kind::File => {
            let mut buf = [0u16; 64];
            // SAFETY: a buffer of its length
            match unsafe { StrFormatKBSizeW(item.size as i64, &mut buf) } {
                p if p.is_null() => String::new(),
                _ => String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(0)]),
            }
        }
        4 => [(0x1, 'R'), (0x2, 'H'), (0x4, 'S'), (0x20, 'A')]
            .iter()
            .filter(|(b, _)| item.attrs & b != 0)
            .map(|(_, c)| *c)
            .collect(),
        _ => String::new(),
    }
}

impl Folder {
    pub fn new(clsid: GUID) -> Folder {
        Folder { clsid, at: RwLock::new(None), _counted: crate::Counted::new() }
    }

    /// Where it is, cloned: its archive, its path in it, its ID list.
    fn at(&self) -> windows::core::Result<(Arc<client::Use>, String)> {
        let at = self.at.read().unwrap();
        let at = at.as_ref().ok_or(windows::core::Error::from(E_FAIL))?;
        Ok((at.archive.clone(), at.dir.clone()))
    }

    /// Initialized at absolute ID list `pidl`: of the archive file, and of entries of it after.
    fn initialize(&self, pidl: *const ITEMIDLIST) -> windows::core::Result<()> {
        if pidl.is_null() {
            return Err(E_INVALIDARG.into());
        }
        // SAFETY: the shell's ID list
        let ids = unsafe { pidl::ids(pidl) };
        let k = ids.iter().position(|b| Item::decode(b).is_some()).unwrap_or(ids.len());
        let file = pidl::alloc(&ids[..k]);
        // SAFETY: an ID list just made, freed after
        let path = unsafe {
            let p = SHGetNameFromIDList(file, SIGDN_FILESYSPATH);
            ILFree(Some(file));
            let p = p?;
            let s = p.to_string().map_err(|_| windows::core::Error::from(E_FAIL));
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
            s?
        };
        let names: Vec<String> = ids[k..].iter().filter_map(|b| Item::decode(b)).map(|i| i.name).collect();
        // SAFETY: the shell's ID list, copied
        let pidl = Pidl(unsafe { ILClone(pidl) });
        let archive = Arc::new(client::Use::of(Path::new(&path)));
        crate::log(|| format!("initialized: {path} at {names:?}"));
        *self.at.write().unwrap() = Some(At { pidl, archive, dir: names.join("/") });
        Ok(())
    }

    /// The folder of path `dir` of the same archive, at absolute ID list `pidl`.
    fn child(&self, archive: Arc<client::Use>, dir: String, pidl: *mut ITEMIDLIST) -> Folder {
        let f = Folder::new(self.clsid);
        *f.at.write().unwrap() = Some(At { pidl: Pidl(pidl), archive, dir });
        f
    }

    /// The entries of the folder, as items.
    fn entries(&self) -> windows::core::Result<Vec<Item>> {
        let (archive, dir) = self.at()?;
        Ok(client::list(archive.path(), &dir)?.iter().map(Item::of).collect())
    }
}

/// The items of relative ID list `p` (none of another namespace).
fn items_of(p: *const ITEMIDLIST) -> windows::core::Result<Vec<Item>> {
    // SAFETY: the shell's ID list
    match unsafe { pidl::items(p) } {
        Some(i) if !i.is_empty() => Ok(i),
        _ => Err(E_INVALIDARG.into()),
    }
}

fn attributes_of(item: &Item) -> u32 {
    let hidden = if item.attrs & 0x2 != 0 { HIDDEN } else { 0 };
    hidden
        | match item.kind {
            Kind::Dir => FOLDER_ATTR | HASSUBFOLDER | CANCOPY | READONLY,
            _ => STREAM | CANCOPY | READONLY,
        }
}

impl IShellFolder_Impl for Folder_Impl {
    fn ParseDisplayName(
        &self,
        _: HWND,
        _: Ref<IBindCtx>,
        name: &PCWSTR,
        eaten: *const u32,
        ppidl: *mut *mut ITEMIDLIST,
        attributes: *mut u32,
    ) -> windows::core::Result<()> {
        guard("ParseDisplayName", || {
            if ppidl.is_null() || name.is_null() {
                return Err(E_POINTER.into());
            }
            // SAFETY: the caller's string, ended by 0
            let text = unsafe { name.to_string() }.map_err(|_| windows::core::Error::from(E_INVALIDARG))?;
            let (archive, dir) = self.at()?;
            let mut path = dir;
            let mut items = Vec::new();
            for n in text.split(['\\', '/']).filter(|n| !n.is_empty()) {
                path = joined(&path, n);
                // (not there: as the file system tells a path not found, for the shell to know it)
                let e = client::stat(archive.path(), &path).map_err(|e| {
                    let missing = e.message().contains("not in the archive");
                    if missing { windows::core::Error::new(NOT_FOUND, e.message()) } else { e }
                })?;
                items.push(Item::of(&e));
            }
            if items.is_empty() {
                return Err(E_INVALIDARG.into());
            }
            // SAFETY: the caller's pointers (`eaten` and `attributes` may be null)
            unsafe {
                if !eaten.is_null() {
                    *(eaten as *mut u32) = text.encode_utf16().count() as u32;
                }
                if !attributes.is_null() {
                    *attributes &= attributes_of(items.last().unwrap());
                }
                *ppidl = pidl::of_items(&items);
            }
            Ok(())
        })
    }

    fn EnumObjects(&self, _: HWND, flags: u32, out: windows::core::OutRef<IEnumIDList>) -> HRESULT {
        guard_hr("EnumObjects", || {
            let items = match self.entries() {
                Ok(i) => i,
                Err(e) => return e.code(),
            };
            let items: Vec<Vec<u8>> = items
                .iter()
                .filter(|i| if i.is_dir() { flags & SHCONTF_FOLDERS != 0 } else { flags & SHCONTF_NONFOLDERS != 0 })
                .filter(|i| i.attrs & 0x2 == 0 || flags & SHCONTF_INCLUDEHIDDEN != 0)
                .map(Item::encode)
                .collect();
            let e: IEnumIDList =
                Enum { items: Arc::new(items), at: Mutex::new(0), _counted: crate::Counted::new() }.into();
            match out.write(Some(e)) {
                Ok(()) => S_OK,
                Err(e) => e.code(),
            }
        })
    }

    fn BindToObject(
        &self,
        pidl: *const ITEMIDLIST,
        _: Ref<IBindCtx>,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        guard("BindToObject", || {
            if ppv.is_null() || riid.is_null() {
                return Err(E_POINTER.into());
            }
            // SAFETY: the caller's pointer
            unsafe { *ppv = std::ptr::null_mut() };
            let items = items_of(pidl)?;
            let (archive, dir) = self.at()?;
            let path = items.iter().fold(dir, |p, i| joined(&p, &i.name));
            // SAFETY: the caller's GUID
            let iid = unsafe { *riid };
            if iid == IStream::IID {
                if items.last().unwrap().is_dir() {
                    return Err(E_INVALIDARG.into());
                }
                let s = client::stream(archive.path(), &path)?;
                // SAFETY: the caller's pointer, given a reference
                return unsafe { s.query(riid, ppv).ok() };
            }
            if !items.iter().all(Item::is_dir) {
                return Err(E_INVALIDARG.into());
            }
            let abs = {
                let at = self.at.read().unwrap();
                // SAFETY: two ID lists, combined into one of COM's memory
                unsafe { ILCombine(Some(at.as_ref().unwrap().pidl.as_ptr()), Some(pidl)) }
            };
            let f: IShellFolder2 = self.child(archive, path, abs).into();
            // SAFETY: the caller's pointer, given a reference
            unsafe { f.query(riid, ppv).ok() }
        })
    }

    fn BindToStorage(
        &self,
        pidl: *const ITEMIDLIST,
        bc: Ref<IBindCtx>,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        // SAFETY: the caller's GUID, where there is one
        if !riid.is_null() && unsafe { *riid } == IStream::IID {
            return self.BindToObject(pidl, bc, riid, ppv);
        }
        Err(E_NOTIMPL.into())
    }

    fn CompareIDs(&self, lparam: LPARAM, a: *const ITEMIDLIST, b: *const ITEMIDLIST) -> HRESULT {
        guard_hr("CompareIDs", || {
            // SAFETY: the shell's ID lists
            let (Some(x), Some(y)) = (unsafe { pidl::items(a) }, unsafe { pidl::items(b) }) else {
                return E_INVALIDARG;
            };
            let col = (lparam.0 & 0xffff) as usize;
            let canonical = lparam.0 as u32 & 0x1000_0000 != 0; // SHCIDS_CANONICALONLY
            let n = compare(&x, &y, col, canonical);
            HRESULT(n as i16 as u16 as i32)
        })
    }

    fn CreateViewObject(
        &self,
        _: HWND,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        guard("CreateViewObject", || {
            if ppv.is_null() || riid.is_null() {
                return Err(E_POINTER.into());
            }
            // SAFETY: the caller's pointers
            unsafe {
                *ppv = std::ptr::null_mut();
                if *riid != IShellView::IID {
                    return Err(E_NOINTERFACE.into());
                }
                let create = SFV_CREATE {
                    cbSize: std::mem::size_of::<SFV_CREATE>() as u32,
                    pshf: std::mem::ManuallyDrop::new(Some(self.to_interface::<IShellFolder2>().into())),
                    psvOuter: std::mem::ManuallyDrop::new(None),
                    psfvcb: std::mem::ManuallyDrop::new(None),
                };
                let v = SHCreateShellFolderView(&create);
                drop(std::mem::ManuallyDrop::into_inner(create.pshf));
                v?.query(riid, ppv).ok()
            }
        })
    }

    fn GetAttributesOf(
        &self,
        cidl: u32,
        apidl: *const *const ITEMIDLIST,
        inout: *mut u32,
    ) -> windows::core::Result<()> {
        guard("GetAttributesOf", || {
            if inout.is_null() {
                return Err(E_POINTER.into());
            }
            let mut all = FOLDER_ATTR | HASSUBFOLDER | READONLY;
            if cidl > 0 {
                all = u32::MAX;
                for k in 0..cidl as usize {
                    // SAFETY: the caller's array of `cidl` ID lists
                    let item = unsafe { pidl::first(*apidl.add(k)) }.ok_or(windows::core::Error::from(E_INVALIDARG))?;
                    all &= attributes_of(&item);
                }
            }
            // SAFETY: the caller's pointer
            unsafe { *inout &= all };
            Ok(())
        })
    }

    fn GetUIObjectOf(
        &self,
        _: HWND,
        cidl: u32,
        apidl: *const *const ITEMIDLIST,
        riid: *const GUID,
        _: *const u32,
        ppv: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        guard("GetUIObjectOf", || {
            if ppv.is_null() || riid.is_null() {
                return Err(E_POINTER.into());
            }
            // SAFETY: the caller's pointers; an array of `cidl` ID lists
            unsafe {
                *ppv = std::ptr::null_mut();
                let iid = *riid;
                if cidl == 1 && iid == IExtractIconW::IID {
                    let item = pidl::first(*apidl).ok_or(windows::core::Error::from(E_INVALIDARG))?;
                    let w: Vec<u16> = item.name.encode_utf16().chain([0]).collect();
                    let x: IExtractIconW = SHCreateFileExtractIconW(PCWSTR(w.as_ptr()), file_attributes(&item).0)?;
                    return x.query(riid, ppv).ok();
                }
                Err(E_NOINTERFACE.into())
            }
        })
    }

    fn GetDisplayNameOf(&self, pidl: *const ITEMIDLIST, flags: SHGDNF, name: *mut STRRET) -> windows::core::Result<()> {
        guard("GetDisplayNameOf", || {
            if name.is_null() {
                return Err(E_POINTER.into());
            }
            let items = items_of(pidl)?;
            let text = if flags.0 & SHGDN_FORPARSING.0 != 0 {
                let rel: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
                match flags.0 & SHGDN_INFOLDER.0 != 0 {
                    true => rel.join("\\"),
                    false => {
                        let (archive, dir) = self.at()?;
                        let mut p = archive.path().to_string_lossy().into_owned();
                        for n in dir.split('/').filter(|n| !n.is_empty()).chain(rel) {
                            p.push('\\');
                            p.push_str(n);
                        }
                        p
                    }
                }
            } else {
                items.last().unwrap().name.clone()
            };
            // SAFETY: the caller's structure
            unsafe { *name = strret(&text)? };
            Ok(())
        })
    }

    fn SetNameOf(
        &self,
        _: HWND,
        _: *const ITEMIDLIST,
        _: &PCWSTR,
        _: SHGDNF,
        _: *mut *mut ITEMIDLIST,
    ) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
}

/// Of two ID lists, which is first by column `col` (folders first; then by name): their first
/// items, then the rest. Of `canonical`, whether they are the same, names as they are.
fn compare(x: &[Item], y: &[Item], col: usize, canonical: bool) -> i32 {
    for (a, b) in x.iter().zip(y) {
        let by_name = || {
            if canonical {
                return a.name.cmp(&b.name) as i32;
            }
            let (wa, wb): (Vec<u16>, Vec<u16>) =
                (a.name.encode_utf16().chain([0]).collect(), b.name.encode_utf16().chain([0]).collect());
            // SAFETY: two strings ended by 0
            match unsafe { StrCmpLogicalW(PCWSTR(wa.as_ptr()), PCWSTR(wb.as_ptr())) } {
                0 => a.name.cmp(&b.name) as i32,
                n => n.signum(),
            }
        };
        let n = match (a.is_dir(), b.is_dir()) {
            _ if canonical => by_name(),
            (true, false) => -1,
            (false, true) => 1,
            _ => {
                let n = match col {
                    1 => a.mtime.cmp(&b.mtime) as i32,
                    2 => type_text(a).to_lowercase().cmp(&type_text(b).to_lowercase()) as i32,
                    3 => a.size.cmp(&b.size) as i32,
                    4 => a.attrs.cmp(&b.attrs) as i32,
                    _ => 0,
                };
                if n != 0 { n } else { by_name() }
            }
        };
        if n != 0 {
            return n;
        }
    }
    x.len().cmp(&y.len()) as i32
}

impl IShellFolder2_Impl for Folder_Impl {
    fn GetDefaultSearchGUID(&self) -> windows::core::Result<GUID> {
        Err(E_NOTIMPL.into())
    }

    fn EnumSearches(&self) -> windows::core::Result<IEnumExtraSearch> {
        Err(E_NOTIMPL.into())
    }

    fn GetDefaultColumn(&self, _: u32, sort: *mut u32, display: *mut u32) -> windows::core::Result<()> {
        // SAFETY: the caller's pointers, where there are
        unsafe {
            if !sort.is_null() {
                *sort = 0;
            }
            if !display.is_null() {
                *display = 0;
            }
        }
        Ok(())
    }

    fn GetDefaultColumnState(&self, col: u32) -> windows::core::Result<SHCOLSTATE> {
        COLUMNS.get(col as usize).map(|c| c.state).ok_or(E_INVALIDARG.into())
    }

    fn GetDetailsEx(&self, pidl: *const ITEMIDLIST, key: *const PROPERTYKEY) -> windows::core::Result<VARIANT> {
        guard("GetDetailsEx", || {
            if key.is_null() {
                return Err(E_POINTER.into());
            }
            // SAFETY: the shell's ID list and key
            let (item, key) = unsafe { (pidl::first(pidl), *key) };
            let item = item.ok_or(windows::core::Error::from(E_INVALIDARG))?;
            if key.fmtid != STORAGE {
                return Err(E_FAIL.into());
            }
            Ok(match key.pid {
                10 => variant_text(&item.name),
                4 => variant_text(&type_text(&item)),
                12 if item.kind == Kind::File => variant(21, item.size), // VT_UI8
                13 => variant(19, item.attrs as u64),                    // VT_UI4
                // SAFETY: a FILETIME
                14 if item.mtime != 0 => unsafe { InitVariantFromFileTime(&filetime(&item))? },
                12 | 14 => variant(0, 0),
                _ => return Err(E_FAIL.into()),
            })
        })
    }

    fn GetDetailsOf(&self, pidl: *const ITEMIDLIST, col: u32, details: *mut SHELLDETAILS) -> windows::core::Result<()> {
        guard("GetDetailsOf", || {
            let c = COLUMNS.get(col as usize).ok_or(windows::core::Error::from(E_INVALIDARG))?;
            if details.is_null() {
                return Err(E_POINTER.into());
            }
            let text = match pidl.is_null() {
                true => c.title.to_owned(),
                // SAFETY: the shell's ID list
                false => {
                    shown(&unsafe { pidl::first(pidl) }.ok_or(windows::core::Error::from(E_INVALIDARG))?, col as usize)
                }
            };
            // SAFETY: the caller's structure
            unsafe {
                (*details).fmt = if c.right { 1 } else { 0 }; // LVCFMT_RIGHT, LVCFMT_LEFT
                (*details).cxChar = c.width;
                (*details).str = strret(&text)?;
            }
            Ok(())
        })
    }

    fn MapColumnToSCID(&self, col: u32, key: *mut PROPERTYKEY) -> windows::core::Result<()> {
        let c = COLUMNS.get(col as usize).ok_or(windows::core::Error::from(E_INVALIDARG))?;
        if key.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: the caller's structure
        unsafe { *key = PROPERTYKEY { fmtid: STORAGE, pid: c.pid } };
        Ok(())
    }
}

impl IPersist_Impl for Folder_Impl {
    fn GetClassID(&self) -> windows::core::Result<GUID> {
        Ok(self.clsid)
    }
}

impl IPersistFolder_Impl for Folder_Impl {
    fn Initialize(&self, pidl: *const ITEMIDLIST) -> windows::core::Result<()> {
        guard("Initialize", || self.initialize(pidl))
    }
}

impl IPersistFolder2_Impl for Folder_Impl {
    fn GetCurFolder(&self) -> windows::core::Result<*mut ITEMIDLIST> {
        let at = self.at.read().unwrap();
        Ok(at.as_ref().map_or(std::ptr::null_mut(), |a| a.pidl.cloned()))
    }
}

impl IPersistFolder3_Impl for Folder_Impl {
    fn InitializeEx(
        &self,
        _: Ref<IBindCtx>,
        root: *const ITEMIDLIST,
        _: *const PERSIST_FOLDER_TARGET_INFO,
    ) -> windows::core::Result<()> {
        guard("InitializeEx", || self.initialize(root))
    }

    fn GetFolderTargetInfo(&self, _: *mut PERSIST_FOLDER_TARGET_INFO) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
}

impl IPersistIDList_Impl for Folder_Impl {
    fn SetIDList(&self, pidl: *const ITEMIDLIST) -> windows::core::Result<()> {
        guard("SetIDList", || self.initialize(pidl))
    }

    fn GetIDList(&self) -> windows::core::Result<*mut ITEMIDLIST> {
        self.GetCurFolder()
    }
}

/// The entries of a folder, as `EnumObjects` gives them.
#[implement(IEnumIDList)]
struct Enum {
    items: Arc<Vec<Vec<u8>>>,
    at: Mutex<usize>,
    _counted: crate::Counted,
}

impl IEnumIDList_Impl for Enum_Impl {
    fn Next(&self, n: u32, out: *mut *mut ITEMIDLIST, fetched: *mut u32) -> HRESULT {
        guard_hr("Next", || {
            if out.is_null() || (n > 1 && fetched.is_null()) {
                return E_POINTER;
            }
            let mut at = self.at.lock().unwrap();
            let mut k = 0;
            while k < n as usize && *at < self.items.len() {
                // SAFETY: the caller's array of `n`
                unsafe { *out.add(k) = pidl::alloc(&[&self.items[*at]]) };
                *at += 1;
                k += 1;
            }
            if !fetched.is_null() {
                // SAFETY: the caller's pointer
                unsafe { *fetched = k as u32 };
            }
            if k == n as usize { S_OK } else { S_FALSE }
        })
    }

    fn Skip(&self, n: u32) -> HRESULT {
        let mut at = self.at.lock().unwrap();
        *at = (*at + n as usize).min(self.items.len());
        if *at < self.items.len() { S_OK } else { S_FALSE }
    }

    fn Reset(&self) -> HRESULT {
        *self.at.lock().unwrap() = 0;
        S_OK
    }

    fn Clone(&self, out: windows::core::OutRef<IEnumIDList>) -> HRESULT {
        let at = *self.at.lock().unwrap();
        let e: IEnumIDList =
            Enum { items: self.items.clone(), at: Mutex::new(at), _counted: crate::Counted::new() }.into();
        match out.write(Some(e)) {
            Ok(()) => S_OK,
            Err(e) => e.code(),
        }
    }
}
