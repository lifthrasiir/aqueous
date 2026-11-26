//! The archives as `aqr-win --serve` serves them (`crates/aqr-win/src/serve.rs`): its class
//! and archive objects (late-bound), one of each for the process. They are held in the
//! process's multithreaded apartment and called there alone ([`on_mta`]), whatever apartment
//! the shell calls from: Explorer's windows each have a thread of their own, which ends as the
//! window is closed, and what an apartment gone held (its proxies, its entries of the Global
//! Interface Table) is gone with it (`E_INVALIDARG` of a cookie no longer there). A thread of
//! this library's keeps the apartment for as long as the process (the library pinned), and
//! looks at the archives' files every [`WATCH`]: one changed is let go of, the views of its
//! folders told to refresh ([`watch`]). Where
//! the server is gone (it ended, or failed), what was held is dropped and the call made again, of
//! a server started anew.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, Once};

use serde_json::Value;
use windows::Win32::Foundation::{E_FAIL, HMODULE, SysAllocStringLen, SysFreeString, SysStringLen};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
    DISPATCH_METHOD, DISPPARAMS, EXCEPINFO, IDispatch, IGlobalInterfaceTable, IStream,
};
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN, GetModuleHandleExW,
};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{BSTR, GUID, HRESULT, IUnknown, Interface, PCWSTR};

/// The class of `aqr-win --serve`.
pub const SERVER: GUID = GUID::from_u128(0xc6b7e992_0338_4d02_8f5f_1bbd5a9fbab9);

/// The server's class, where another is to be taken (a test's).
static SERVER_CLASS: Mutex<Option<GUID>> = Mutex::new(None);

pub fn set_server(g: GUID) {
    *SERVER_CLASS.lock().unwrap() = Some(g);
}

/// An object of the multithreaded apartment, used (and let go of) there alone.
struct Mta(IDispatch);

// SAFETY: used on threads of the multithreaded apartment alone ([`on_mta`]), where a proxy is
// any thread's
unsafe impl Send for Mta {}
unsafe impl Sync for Mta {}

/// `f` run in the multithreaded apartment (on a thread of its own, waited for), which a thread
/// of this library's keeps for as long as the process.
fn on_mta<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    static KEEPER: Once = Once::new();
    KEEPER.call_once(|| {
        // (never unloaded while its thread is: pinned)
        let mut m = HMODULE::default();
        // SAFETY: an address of this library's, and a handle to fill
        _ = unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
                PCWSTR(git as *const () as *const u16),
                &mut m,
            )
        };
        let (ready, started) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // SAFETY: this thread's COM, for as long as the process
            _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            _ = ready.send(());
            // (and the archives' files watched, while it waits)
            loop {
                std::thread::sleep(WATCH);
                watch();
            }
        });
        _ = started.recv();
    });
    std::thread::scope(|s| {
        s.spawn(|| {
            // SAFETY: this thread's COM, of the apartment the keeper holds
            let init = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
            let r = f();
            if init {
                // SAFETY: initialized above
                unsafe { CoUninitialize() };
            }
            r
        })
        .join()
        .unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}

/// What is held let go of, in the apartment it is of.
fn release(held: Vec<Mta>) {
    if !held.is_empty() {
        on_mta(move || drop(held));
    }
}

/// `CLSID_StdGlobalInterfaceTable`.
const GLOBAL_TABLE: GUID = GUID::from_u128(0x00000323_0000_0000_c000_000000000046);

fn git() -> windows::core::Result<IGlobalInterfaceTable> {
    // SAFETY: the system's class
    unsafe { CoCreateInstance(&GLOBAL_TABLE, None, CLSCTX_INPROC_SERVER) }
}

/// The server's class object, and the archives opened by path, each with the uses of it
/// ([`Use`]): let go of with the last, for the server to close it (and to end, idle).
struct Connection {
    server: Option<Mta>,
    archives: HashMap<PathBuf, (Option<Mta>, usize)>,
    /// By archive held, its file's size and time as it was opened ([`changed`]).
    stamps: HashMap<PathBuf, (u64, Option<std::time::SystemTime>)>,
}

// (never locked over a COM call, nor as what it holds is let go of)
static CONNECTION: LazyLock<Mutex<Connection>> =
    LazyLock::new(|| Mutex::new(Connection { server: None, archives: HashMap::new(), stamps: HashMap::new() }));

/// Of the server gone: the error of a call to an object of a process no longer there.
fn disconnected(e: &windows::core::Error) -> bool {
    matches!(
        e.code().0 as u32,
        0x8001_0108 // RPC_E_DISCONNECTED
            | 0x8007_06BA // RPC_S_SERVER_UNAVAILABLE
            | 0x8007_06BF // RPC_S_CALL_FAILED_DNE
            | 0x8001_0007 // RPC_E_SERVER_DIED
            | 0x8001_0012 // RPC_E_SERVER_DIED_DNE
            | 0x8004_01FD // CO_E_OBJNOTCONNECTED
            // (DISP_E_UNKNOWNNAME: a server of an older build, still running as a newer one is
            // registered; it ends once what it serves is let go of)
            | 0x8002_0006
    )
}

/// A use of archive `path` (a folder's): the archive is kept open while it has one.
pub struct Use(PathBuf);

impl Use {
    pub fn of(path: &Path) -> Use {
        CONNECTION.lock().unwrap().archives.entry(path.to_owned()).or_insert((None, 0)).1 += 1;
        Use(path.to_owned())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Use {
    fn drop(&mut self) {
        let gone = {
            let mut c = CONNECTION.lock().unwrap();
            let last = c.archives.get_mut(&self.0).map(|e| {
                e.1 -= 1;
                e.1 == 0
            });
            let mut gone: Vec<Mta> = Vec::new();
            if last == Some(true) {
                gone.extend(c.archives.remove(&self.0).and_then(|e| e.0));
                c.stamps.remove(&self.0);
            }
            // (none left: the server too, for it to end once idle)
            if c.archives.is_empty() {
                gone.extend(c.server.take());
            }
            gone
        };
        release(gone);
    }
}

/// Archive `path` as the server has it open (opened now, where it is not); in the
/// multithreaded apartment.
fn archive(path: &Path) -> windows::core::Result<IDispatch> {
    let (archive, server) = {
        let c = CONNECTION.lock().unwrap();
        let archive = c.archives.get(path).and_then(|e| e.0.as_ref()).map(|m| m.0.clone());
        (archive, c.server.as_ref().map(|m| m.0.clone()))
    };
    if let Some(a) = archive {
        return Ok(a);
    }
    let server: IDispatch = match server {
        Some(s) => s,
        None => {
            let class = SERVER_CLASS.lock().unwrap().unwrap_or(SERVER);
            // SAFETY: a class of a local server
            let s: IDispatch = unsafe { CoCreateInstance(&class, None, CLSCTX_LOCAL_SERVER)? };
            let other = CONNECTION.lock().unwrap().server.replace(Mta(s.clone()));
            drop(other);
            s
        }
    };
    // (an encrypted one asks for its passphrase over the window of the shell's: the server's
    // dialog let come to the front)
    let owner = OWNER.load(std::sync::atomic::Ordering::Relaxed);
    // SAFETY: a proxy of the server's object
    unsafe { allow_foreground(&server) };
    let stamp = stamp(path);
    let a = match call(&server, "Open", &[&path.to_string_lossy(), &owner.to_string()])? {
        Got::Object(o) => o.cast::<IDispatch>()?,
        Got::Text(_) => return Err(E_FAIL.into()),
    };
    // (held where it has a use, of which one opening it at once: the other let go of)
    let other = {
        let mut c = CONNECTION.lock().unwrap();
        c.stamps.insert(path.to_owned(), stamp);
        match c.archives.get_mut(path) {
            Some(e) if e.0.is_none() => e.0.replace(Mta(a.clone())),
            _ => None,
        }
    };
    drop(other);
    Ok(a)
}

/// The window of the shell's that a question is asked over (the last one a folder was given).
pub static OWNER: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// The size and modification time of file `path`.
fn stamp(path: &Path) -> (u64, Option<std::time::SystemTime>) {
    std::fs::metadata(path).map_or((0, None), |m| (m.len(), m.modified().ok()))
}

/// Whether archive `path` changed since it was opened (its file's size or time): then it is let
/// go of, to be opened anew.
pub fn changed(path: &Path) -> bool {
    let now = stamp(path);
    let gone = {
        let mut c = CONNECTION.lock().unwrap();
        match c.stamps.get(path) {
            Some(&s) if s != now => {
                c.stamps.remove(path);
                c.archives.get_mut(path).and_then(|e| e.0.take())
            }
            _ => return false,
        }
    };
    release(gone.into_iter().collect());
    true
}

/// How often the files of the archives in use are looked at ([`watch`]).
const WATCH: std::time::Duration = std::time::Duration::from_secs(2);

/// The views of folders of archives (by archive: their ID lists, absolute), told to refresh as
/// an archive's file changes.
static VIEWS: Mutex<Vec<(u64, PathBuf, Vec<u8>)>> = Mutex::new(Vec::new());

/// A folder of archive `path` at ID list `pidl` (absolute), told to refresh where the archive's
/// file changes, for as long as it is kept.
pub struct View(u64);

impl View {
    /// # Safety
    ///
    /// `pidl` is an ID list.
    pub unsafe fn of(path: &Path, pidl: *const windows::Win32::UI::Shell::Common::ITEMIDLIST) -> View {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // SAFETY: as the caller's
        let ids = unsafe { crate::pidl::ids(pidl) };
        let mut bytes: Vec<u8> = ids.concat();
        bytes.extend([0, 0]);
        VIEWS.lock().unwrap().push((id, path.to_owned(), bytes));
        View(id)
    }
}

impl Drop for View {
    fn drop(&mut self) {
        VIEWS.lock().unwrap().retain(|v| v.0 != self.0);
    }
}

/// The archives whose files changed since they were opened (their size or time), let go of (to
/// be opened anew), and the views of their folders told to refresh. On the keeper's thread.
fn watch() {
    let stamps: Vec<(PathBuf, (u64, Option<std::time::SystemTime>))> =
        CONNECTION.lock().unwrap().stamps.iter().map(|(p, s)| (p.clone(), *s)).collect();
    for (path, was) in stamps {
        if stamp(&path) == was {
            continue;
        }
        let gone = {
            let mut c = CONNECTION.lock().unwrap();
            // (where nothing else saw it first)
            if c.stamps.get(&path) != Some(&was) {
                continue;
            }
            c.stamps.remove(&path);
            c.archives.get_mut(&path).and_then(|e| e.0.take())
        };
        drop(gone);
        crate::log(|| format!("watched: {} changed", path.display()));
        if crate::TESTING.load(std::sync::atomic::Ordering::Relaxed) {
            continue;
        }
        let views: Vec<Vec<u8>> = VIEWS.lock().unwrap().iter().filter(|v| v.1 == path).map(|v| v.2.clone()).collect();
        for v in views {
            // SAFETY: an ID list of its own
            unsafe {
                windows::Win32::UI::Shell::SHChangeNotify(
                    windows::Win32::UI::Shell::SHCNE_UPDATEDIR,
                    windows::Win32::UI::Shell::SHCNF_IDLIST,
                    Some(v.as_ptr() as *const _),
                    None,
                )
            };
        }
    }
}

/// Lets the process of `server` come to the front (its dialogs, asked for by a call).
unsafe fn allow_foreground(server: &IDispatch) {
    windows::core::link!("ole32.dll" "system" fn CoAllowSetForegroundWindow(punk: *mut core::ffi::c_void, reserved: *mut core::ffi::c_void) -> HRESULT);
    // SAFETY: an object of the caller's
    _ = unsafe { CoAllowSetForegroundWindow(server.as_raw(), std::ptr::null_mut()) };
}

/// `f` of archive `path`, in the multithreaded apartment; again of a server started anew, where
/// it was gone.
fn with<T: Send>(path: &Path, f: impl Fn(&IDispatch) -> windows::core::Result<T> + Sync) -> windows::core::Result<T> {
    on_mta(|| {
        let r = archive(path).and_then(|a| f(&a));
        match r {
            Err(e) if disconnected(&e) => {
                let dropped = {
                    let mut c = CONNECTION.lock().unwrap();
                    let mut held: Vec<Mta> = c.archives.values_mut().filter_map(|e| e.0.take()).collect();
                    held.extend(c.server.take());
                    held
                };
                drop(dropped);
                f(&archive(path)?)
            }
            r => r,
        }
    })
}

/// What a method gave.
enum Got {
    Text(String),
    Object(IUnknown),
}

/// `VARIANT`, as it is laid out (of a `BSTR` or an object here).
#[repr(C)]
struct Var {
    vt: u16,
    reserved: [u16; 3],
    value: usize,
    record: usize,
}

const _: () = assert!(std::mem::size_of::<Var>() == std::mem::size_of::<VARIANT>());

/// Method `name` of late-bound `o` called with the strings `args`.
fn call(o: &IDispatch, name: &str, args: &[&str]) -> windows::core::Result<Got> {
    let w: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let mut id = 0;
    // SAFETY: COM calls of the object, of the arguments made here (strings let go of after)
    unsafe {
        o.GetIDsOfNames(&GUID::zeroed(), &PCWSTR(w.as_ptr()), 1, 0, &mut id)?;
        let vs: Vec<Var> = args
            .iter()
            .rev()
            .map(|a| {
                let u: Vec<u16> = a.encode_utf16().collect();
                let b = SysAllocStringLen(Some(&u));
                Var { vt: 8, reserved: [0; 3], value: std::mem::transmute::<BSTR, usize>(b), record: 0 }
            })
            .collect();
        let p = DISPPARAMS {
            rgvarg: vs.as_ptr() as *mut VARIANT,
            rgdispidNamedArgs: std::ptr::null_mut(),
            cArgs: vs.len() as u32,
            cNamedArgs: 0,
        };
        let mut r = Var { vt: 0, reserved: [0; 3], value: 0, record: 0 };
        let mut x = EXCEPINFO::default();
        let hr = o.Invoke(
            id,
            &GUID::zeroed(),
            0,
            DISPATCH_METHOD,
            &p,
            Some(&mut r as *mut Var as *mut VARIANT),
            Some(&mut x),
            None,
        );
        for v in vs {
            SysFreeString(&std::mem::transmute::<usize, BSTR>(v.value));
        }
        if let Err(e) = hr {
            let m = x.bstrDescription.to_string();
            return Err(match m.is_empty() {
                true => e,
                false => windows::core::Error::new(if x.scode != 0 { HRESULT(x.scode) } else { E_FAIL }, m),
            });
        }
        match r.vt {
            8 => {
                let b: BSTR = std::mem::transmute(r.value);
                let n = SysStringLen(&b);
                let t = String::from_utf16_lossy(std::slice::from_raw_parts(b.as_ptr(), n as usize));
                Ok(Got::Text(t))
            }
            9 | 13 => Ok(Got::Object(IUnknown::from_raw(r.value as *mut _))),
            _ => Err(E_FAIL.into()),
        }
    }
}

fn json(o: &IDispatch, name: &str, args: &[&str]) -> windows::core::Result<Value> {
    match call(o, name, args)? {
        Got::Text(t) => serde_json::from_str(&t).map_err(|e| windows::core::Error::new(E_FAIL, e.to_string())),
        Got::Object(_) => Err(E_FAIL.into()),
    }
}

/// The entries of directory `dir` (names joined with `/`) of archive `path`.
/// With what `-t -v` tells too where `verbose` (all of the archive's blocks read for it, once).
pub fn list(path: &Path, dir: &str, verbose: bool) -> windows::core::Result<Vec<Value>> {
    let how = if verbose { "verbose" } else { "" };
    with(path, |a| Ok(json(a, "List", &[dir, how])?.as_array().cloned().unwrap_or_default()))
}

/// The entry at `at` of archive `path`.
/// With what `-t -v` tells too where `verbose`.
pub fn stat(path: &Path, at: &str, verbose: bool) -> windows::core::Result<Value> {
    let how = if verbose { "verbose" } else { "" };
    with(path, |a| json(a, "Stat", &[at, how]))
}

/// A stream of file `at` of archive `path`, for the calling apartment.
pub fn stream(path: &Path, at: &str) -> windows::core::Result<IStream> {
    // (handed over by the Global Interface Table, from the apartment it is got in)
    let cookie = with(path, |a| match call(a, "OpenStream", &[at])? {
        // SAFETY: the system's table, and a stream
        Got::Object(o) => unsafe { git()?.RegisterInterfaceInGlobal(&o, &IStream::IID) },
        Got::Text(_) => Err(E_FAIL.into()),
    })?;
    let mut p = std::ptr::null_mut();
    // SAFETY: the table's stream of `cookie`, taken once
    unsafe {
        let g = git()?;
        let r = g.GetInterfaceFromGlobal(cookie, &IStream::IID, &mut p);
        _ = g.RevokeInterfaceFromGlobal(cookie);
        r?;
        Ok(IStream::from_raw(p))
    }
}

/// File `at` of archive `path` written out by the server, to be opened by a program: its path.
pub fn materialize(path: &Path, at: &str) -> windows::core::Result<PathBuf> {
    with(path, |a| match call(a, "Materialize", &[at])? {
        Got::Text(t) => Ok(PathBuf::from(t)),
        Got::Object(_) => Err(E_FAIL.into()),
    })
}

/// How far file `at` of archive `path` is being made: `(done, total)` of its bytes written
/// where it is, else of the archive's data decompressed.
pub fn progress(path: &Path, at: &str) -> windows::core::Result<(u64, u64)> {
    let v = with(path, |a| json(a, "Progress", &[at]))?;
    let pair = |k: &str| Some((v[k][0].as_u64()?, v[k][1].as_u64()?));
    Ok(pair("written").or_else(|| pair("decompressed")).unwrap_or((0, 0)))
}

/// File `at` of archive `path`, being written out, stopped (removed, its Materialize failing).
pub fn cancel(path: &Path, at: &str) -> windows::core::Result<()> {
    with(path, |a| json(a, "Cancel", &[at]).map(drop))
}

/// The bytes of file `at` of archive `path` (at most `max`: none past them).
pub fn read_all(path: &Path, at: &str, max: u64) -> windows::core::Result<Vec<u8>> {
    with(path, |a| match call(a, "OpenStream", &[at])? {
        Got::Object(o) => {
            let s: IStream = o.cast()?;
            let (mut all, mut buf) = (Vec::new(), vec![0u8; 1 << 20]);
            loop {
                let mut n = 0;
                // SAFETY: a buffer of its length
                unsafe { s.Read(buf.as_mut_ptr() as *mut _, buf.len() as u32, Some(&mut n)).ok()? };
                if n == 0 || all.len() as u64 >= max {
                    return Ok(all);
                }
                all.extend_from_slice(&buf[..n as usize]);
            }
        }
        Got::Text(_) => Err(E_FAIL.into()),
    })
}
