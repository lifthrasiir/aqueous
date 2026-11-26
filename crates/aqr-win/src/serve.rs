//! `aqr-win --serve`: the archives a shell extension browses (`docs/win-shell-ext-design.md`),
//! served by a COM local server (`LocalServer32`, of [`CLSID`]) over `aqr --serve` (one
//! process for all, started as it is first needed and again where it is gone:
//! `aqr_ipc::serve`), so that what decodes is in no process that loaded the extension.
//!
//! The class's objects are late-bound (`IDispatch`, no type library: the system's proxies
//! marshal them), their methods named:
//!
//! - the class: `Open(path[, hwnd])`, an archive object, or an error (`EXCEPINFO`'s
//!   description). An encrypted archive has its passphrase asked for over window `hwnd` (a
//!   number, of the client's: its process lets this one come to the front), again where wrong;
//!   it is kept by the archive object, to open the archive again where `aqr --serve` is gone.
//! - an archive: `Info()` (what `open` answered, as JSON), `List(dir)` (the entries, a JSON
//!   array), `Stat(path)` (an entry, JSON), `OpenStream(path)` (an `IStream` of the file, read
//!   only, read a chunk of `read` after another), `Materialize(path)` (the file written out to
//!   be opened by a program, read only, in `%LOCALAPPDATA%\Aqueous\Open\<archive>\<path>`: its
//!   path; one there already of its size taken as it is), `Progress(path)` (JSON: `aqr --serve`'s
//!   `progress` of the archive, and `written`, `[done, total]` bytes of the file at `path` being
//!   materialized), `Cancel(path)` (the file at `path` being materialized stopped at its next
//!   piece, its part removed, the Materialize failing; what is evaluated of it kept). Once let
//!   go of (its last reference released,
//!   by the client or by COM as the client is gone), its handle is closed a [`LINGER`] later,
//!   unless the archive is opened again: what was evaluated of it kept while the client moves
//!   from one folder to another.
//!
//! The files written out are removed as the program ends (but those open still), and those of
//! a day before as it starts. The program ends once it has served no object for [`IDLE`] (the class revoked first, so that
//! a client coming then starts another).

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aqr_ipc::serve::{CHUNK, Error, Served};
use serde_json::{Value, json};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Foundation::{
    CLASS_E_NOAGGREGATION, DISP_E_EXCEPTION, DISP_E_MEMBERNOTFOUND, DISP_E_TYPEMISMATCH, DISP_E_UNKNOWNNAME, E_FAIL,
    E_NOINTERFACE, E_NOTIMPL, E_OUTOFMEMORY, E_POINTER, FILETIME, S_FALSE, S_OK, STG_E_ACCESSDENIED,
    STG_E_INVALIDFUNCTION, STG_E_READFAULT, SysAllocStringLen, SysStringLen,
};
use windows_sys::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, CLSIDFromString, COINIT_MULTITHREADED, CoInitializeEx, CoRegisterClassObject,
    CoRevokeClassObject, CoTaskMemAlloc, REGCLS_MULTIPLEUSE,
};
use windows_sys::core::{GUID, HRESULT};

/// The class of the server.
pub const CLSID: GUID = GUID::from_u128(0xc6b7e992_0338_4d02_8f5f_1bbd5a9fbab9);

/// How long an archive let go of stays open, for a client opening it again.
const LINGER: Duration = Duration::from_secs(60);

/// How long the program stays with no object served.
const IDLE: Duration = Duration::from_secs(600);

/// The objects served (the class's, archives, streams) and the locks of the server: none for
/// [`IDLE`], the program ends.
static OBJECTS: AtomicUsize = AtomicUsize::new(0);

/// Since when no object is served.
static IDLE_SINCE: Mutex<Option<Instant>> = Mutex::new(None);

fn object_made() {
    OBJECTS.fetch_add(1, Ordering::SeqCst);
}

fn object_gone() {
    if OBJECTS.fetch_sub(1, Ordering::SeqCst) == 1 {
        *IDLE_SINCE.lock().unwrap() = Some(Instant::now());
    }
}

/// `aqr --serve`, once started.
static SERVED: Mutex<Option<Arc<Served>>> = Mutex::new(None);

/// `aqr --serve`, started where it is not, or again where it is gone.
fn served() -> Result<Arc<Served>, String> {
    let mut s = SERVED.lock().unwrap();
    if let Some(s) = s.as_ref().filter(|s| !s.gone()) {
        return Ok(s.clone());
    }
    let aqr = aqr_ipc::aqr();
    let started = Served::start(&aqr, &[]).map_err(|e| format!("{}: {e}", aqr.display()))?;
    Ok(s.insert(Arc::new(started)).clone())
}

/// The handles let go of, closed once they have lingered ([`LINGER`]).
static LINGERING: Mutex<Vec<(Arc<Served>, u64, Instant)>> = Mutex::new(Vec::new());

/// The handles that have lingered `linger` closed (all of them, where none).
fn close_lingering(linger: Option<Duration>) {
    let now = Instant::now();
    let done: Vec<_> = {
        let mut l = LINGERING.lock().unwrap();
        let (done, kept) = l.drain(..).partition(|(_, _, at)| linger.is_none_or(|t| now.duration_since(*at) >= t));
        *l = kept;
        done
    };
    for (s, h, _) in done {
        _ = s.ask(json!({"close": h}));
    }
}

/// Serves the class until it has been idle for [`IDLE`] (`args`: COM's `-Embedding`; for tests,
/// the class's CLSID in place of [`CLSID`], and `AQR_SERVE_IDLE` the seconds of [`LINGER`] and
/// [`IDLE`]).
pub fn serve(args: &[std::ffi::OsString]) -> std::process::ExitCode {
    let mut clsid = CLSID;
    for a in args {
        let a = a.to_string_lossy();
        if a.starts_with('{') {
            let w: Vec<u16> = a.encode_utf16().chain([0]).collect();
            // SAFETY: a string ended by 0, and a GUID to fill
            if unsafe { CLSIDFromString(w.as_ptr(), &mut clsid) } < 0 {
                return std::process::ExitCode::FAILURE;
            }
        }
    }
    let (linger, idle) = match std::env::var("AQR_SERVE_IDLE").ok().and_then(|v| {
        let (l, i) = v.split_once(',')?;
        Some((Duration::from_secs_f64(l.parse().ok()?), Duration::from_secs_f64(i.parse().ok()?)))
    }) {
        Some(t) => t,
        None => (LINGER, IDLE),
    };
    // SAFETY: this thread's, of no other COM use; the calls come on COM's threads
    let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) };
    if hr < 0 {
        return std::process::ExitCode::FAILURE;
    }
    *IDLE_SINCE.lock().unwrap() = Some(Instant::now());
    // (what was opened before, of a day ago: let go of)
    sweep(Some(Duration::from_secs(24 * 3600)));
    let factory = Box::into_raw(Box::new(Factory { vtbl: &FACTORY_VTBL, refs: AtomicU32::new(1) }));
    let mut cookie = 0;
    // SAFETY: a class object of a static vtable, alive until the process ends
    let hr = unsafe {
        CoRegisterClassObject(
            &clsid,
            factory as *mut c_void,
            CLSCTX_LOCAL_SERVER,
            REGCLS_MULTIPLEUSE as u32,
            &mut cookie,
        )
    };
    if hr < 0 {
        return std::process::ExitCode::FAILURE;
    }
    let mut checked = Instant::now();
    let mut replaced = false;
    loop {
        std::thread::sleep(Duration::from_millis(250).min(linger / 4).max(Duration::from_millis(10)));
        close_lingering(Some(linger));
        // (registered anew, of another program: a newer one. The class revoked, for clients to
        // start that one; this one ends as soon as what it serves is let go of)
        if !replaced && checked.elapsed() >= Duration::from_secs(2) {
            checked = Instant::now();
            if superseded(&clsid) {
                // SAFETY: registered above
                unsafe { CoRevokeClassObject(cookie) };
                replaced = true;
            }
        }
        let since = *IDLE_SINCE.lock().unwrap();
        let idle = if replaced { Duration::ZERO } else { idle };
        if OBJECTS.load(Ordering::SeqCst) == 0 && since.is_some_and(|t| t.elapsed() >= idle) {
            if !replaced {
                // SAFETY: registered above: a client from now on starts another server
                unsafe { CoRevokeClassObject(cookie) };
            }
            // (an object made just before, served until it is let go of)
            while OBJECTS.load(Ordering::SeqCst) > 0 {
                std::thread::sleep(Duration::from_millis(100));
            }
            close_lingering(None);
            drop(SERVED.lock().unwrap().take());
            // (the files opened, but those open still)
            sweep(None);
            return std::process::ExitCode::SUCCESS;
        }
    }
}

/// Whether the class `clsid` is registered (for the user) as of another program than this one:
/// this one replaced (`cargo aqr-win --register` of another build, an update installed). Not of
/// a class not registered (a test's).
fn superseded(clsid: &GUID) -> bool {
    let key = format!(r"Software\Classes\CLSID\{}\LocalServer32", crate::drop::braced(clsid));
    let Some((command, _)) = crate::register::get(&key, None) else { return false };
    let Ok(exe) = std::env::current_exe() else { return false };
    // (`"PATH" --serve`)
    let registered = command.trim_start_matches('"').split('"').next().unwrap_or("");
    !std::path::Path::new(registered).as_os_str().eq_ignore_ascii_case(exe.as_os_str())
}

/// Where the files opened of archives are written (`%LOCALAPPDATA%\Aqueous\Open`): a directory
/// for each archive (by a hash of its path and identity), the files at their paths in it.
fn opened_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join(r"Aqueous\Open"))
}

/// File `path` of archive `a` written out to be opened by a program (read only, so that a change
/// made to it is saved elsewhere, not lost), where it is not already: its path.
fn materialize(a: &Archive, path: &str) -> Result<PathBuf, String> {
    let e = a.with(|s, h| s.ask(json!({"stat": h, "path": path})))?;
    let e = &e["entry"];
    if e["kind"] != "file" {
        return Err(format!("{path}: not a file"));
    }
    let size = e["size"].as_u64().unwrap_or(0);
    let identity = a.info()["identity"].as_str().unwrap_or("").to_owned();
    // (FNV-1a of the archive's path and identity)
    let key = format!("{}\n{identity}", a.path.display())
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    let dir = opened_dir().ok_or("no LOCALAPPDATA")?.join(format!("{key:016x}"));
    let to = path.split('/').filter(|n| !n.is_empty()).fold(dir, |p, n| p.join(n));
    if std::fs::metadata(&to).is_ok_and(|m| m.len() == size) {
        return Ok(to);
    }
    let parent = to.parent().ok_or("no folder")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let part = to.with_extension(format!("{}.part", std::process::id()));
    let r = (|| -> Result<(), String> {
        let f = std::fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
        let mut at = 0u64;
        // (how far, told; whether cancelled)
        let told = |at: u64| {
            let mut w = WRITING.lock().unwrap();
            match w.iter_mut().find(|e| e.of(&a.path, path)) {
                Some(e) => {
                    e.done = at;
                    e.cancelled
                }
                None => {
                    w.push(Writing {
                        archive: a.path.clone(),
                        path: path.to_owned(),
                        done: at,
                        size,
                        cancelled: false,
                    });
                    false
                }
            }
        };
        told(0);
        while at < size {
            let d = a.with(|s, h| s.read(h, path, at, CHUNK))?;
            if d.is_empty() {
                return Err(format!("{path}: cut short"));
            }
            write_at(&f, &d, at).map_err(|e| format!("{}: {e}", part.display()))?;
            at += d.len() as u64;
            if told(at) {
                return Err(format!("{path}: cancelled"));
            }
        }
        if let Some(t) = file_time(e) {
            _ = f.set_modified(t);
        }
        drop(f);
        let mut p = std::fs::metadata(&part).map_err(|e| e.to_string())?.permissions();
        p.set_readonly(true);
        std::fs::set_permissions(&part, p).map_err(|e| e.to_string())?;
        // (over one not of the size: cut short, or of another file before)
        _ = clear_readonly(&to).and_then(|()| std::fs::remove_file(&to));
        std::fs::rename(&part, &to).map_err(|e| format!("{}: {e}", to.display()))
    })();
    WRITING.lock().unwrap().retain(|e| !e.of(&a.path, path));
    if r.is_err() {
        _ = clear_readonly(&part).and_then(|()| std::fs::remove_file(&part));
    }
    r.map(|()| to)
}

fn write_at(f: &std::fs::File, d: &[u8], at: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    let (mut at, mut d) = (at, d);
    while !d.is_empty() {
        match f.seek_write(d, at)? {
            0 => return Err(std::io::ErrorKind::WriteZero.into()),
            n => (at, d) = (at + n as u64, &d[n..]),
        }
    }
    Ok(())
}

/// The modification time of `entry`, as `stat` tells it.
fn file_time(entry: &Value) -> Option<std::time::SystemTime> {
    let (s, n) = (entry["mtime"][0].as_i64()?, entry["mtime"][1].as_u64()?);
    let d = Duration::new(s.unsigned_abs(), n as u32);
    if s >= 0 { std::time::UNIX_EPOCH.checked_add(d) } else { std::time::UNIX_EPOCH.checked_sub(d) }
}

fn clear_readonly(p: &std::path::Path) -> std::io::Result<()> {
    let mut perm = std::fs::metadata(p)?.permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    std::fs::set_permissions(p, perm)
}

/// The files opened let go of: of the archives' directories not changed for `older` (all, where
/// none), each file but those open still (a program has it).
fn sweep(older: Option<Duration>) {
    let Some(root) = opened_dir() else { return };
    let Ok(dirs) = std::fs::read_dir(&root) else { return };
    for d in dirs.flatten() {
        let age = d.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
        if older.is_some_and(|o| age.is_some_and(|a| a < o)) {
            continue;
        }
        remove_tree(&d.path());
    }
}

/// What of `p` can be removed, removed (a file open stays, and the folders it is in).
fn remove_tree(p: &std::path::Path) {
    if let Ok(entries) = std::fs::read_dir(p) {
        for e in entries.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => remove_tree(&e.path()),
                _ => _ = clear_readonly(&e.path()).and_then(|()| std::fs::remove_file(e.path())),
            }
        }
        _ = std::fs::remove_dir(p);
    }
}

/// Why an archive did not open.
enum Opening {
    /// Encrypted: no passphrase given, or one that does not open it (`wrong`).
    Encrypted {
        wrong: bool,
    },
    Failed(String),
}

/// By archive and path, the bytes of a file written out so far ([`materialize`]), and all of it.
/// Where cancelled ([`Kind::Archive`]'s `Cancel`), it stops at the next piece.
static WRITING: Mutex<Vec<Writing>> = Mutex::new(Vec::new());

/// A file being written out: of which archive, its path, its bytes written and all of them, and
/// whether it is to stop.
struct Writing {
    archive: PathBuf,
    path: String,
    done: u64,
    size: u64,
    cancelled: bool,
}

impl Writing {
    fn of(&self, archive: &std::path::Path, path: &str) -> bool {
        self.archive == archive && self.path == path
    }
}

/// An archive open: its path, and the `aqr --serve` and handle it is open in, opened again
/// where that is gone.
struct Archive {
    path: PathBuf,
    /// The passphrase it was opened of, to open it again of (in this process alone).
    passphrase: Option<String>,
    open: Mutex<(Arc<Served>, u64, Value)>,
}

impl Archive {
    /// Archive `path` opened: an encrypted one of a passphrase asked for over `owner` (again
    /// where it was wrong), the user's cancel an error.
    fn open(path: PathBuf, owner: HWND) -> Result<Archive, String> {
        let mut passphrase = None;
        loop {
            match Archive::opened(&path, passphrase.as_deref()) {
                Ok((s, h, info)) => return Ok(Archive { path, passphrase, open: Mutex::new((s, h, info)) }),
                Err(Opening::Encrypted { wrong }) => {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
                    // (for tests: the answer of the dialog, once, and of none again)
                    let asked = match std::env::var("AQR_SERVE_TEST_PASSPHRASE") {
                        Ok(p) => (!wrong).then_some(p),
                        Err(_) => crate::passphrase::ask(owner, name.as_deref(), false, wrong, false),
                    };
                    match asked {
                        Some(p) => passphrase = Some(p),
                        None => return Err(format!("{}: encrypted, and no passphrase given", path.display())),
                    }
                }
                Err(Opening::Failed(e)) => return Err(e),
            }
        }
    }

    fn opened(path: &std::path::Path, passphrase: Option<&str>) -> Result<(Arc<Served>, u64, Value), Opening> {
        let s = served().map_err(Opening::Failed)?;
        let mut q = json!({"open": path});
        if let Some(p) = passphrase {
            q["passphrase"] = p.into();
        }
        let from = s.send(q).map_err(|e| Opening::Failed(e.to_string()))?;
        let (v, _) = from.recv().map_err(|_| Opening::Failed(Error::Gone.to_string()))?;
        if v["encrypted"] == true {
            return Err(Opening::Encrypted { wrong: v["wrong"] == true });
        }
        if let Some(e) = v["error"].as_str() {
            return Err(Opening::Failed(e.to_owned()));
        }
        let h = v["archive"].as_u64().ok_or(Opening::Failed("no handle opened".into()))?;
        Ok((s, h, v))
    }

    /// `f` of the server and handle; again of the archive opened again, where that server is
    /// gone.
    fn with<T>(&self, f: impl Fn(&Served, u64) -> Result<T, Error>) -> Result<T, String> {
        // (not locked while it is at work: the requests of an archive at once)
        let (s, h) = {
            let o = self.open.lock().unwrap();
            (o.0.clone(), o.1)
        };
        match f(&s, h) {
            Err(Error::Gone) => {}
            r => return r.map_err(|e| e.to_string()),
        }
        let (s, h) = {
            let mut o = self.open.lock().unwrap();
            // (where another call has not opened it again already)
            if Arc::ptr_eq(&o.0, &s) {
                *o = Archive::opened(&self.path, self.passphrase.as_deref()).map_err(|e| match e {
                    Opening::Failed(e) => e,
                    Opening::Encrypted { .. } => format!("{}: encrypted", self.path.display()),
                })?;
            }
            (o.0.clone(), o.1)
        };
        f(&s, h).map_err(|e| e.to_string())
    }

    fn info(&self) -> Value {
        self.open.lock().unwrap().2.clone()
    }
}

impl Drop for Archive {
    fn drop(&mut self) {
        let (s, h, _) = &*self.open.lock().unwrap();
        if !s.gone() {
            LINGERING.lock().unwrap().push((s.clone(), *h, Instant::now()));
        }
    }
}

// ---- COM: the class object, the late-bound objects, the streams ----

const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_ICLASSFACTORY: GUID = GUID::from_u128(0x00000001_0000_0000_c000_000000000046);
const IID_IDISPATCH: GUID = GUID::from_u128(0x00020400_0000_0000_c000_000000000046);
const IID_ISEQUENTIALSTREAM: GUID = GUID::from_u128(0x0c733a30_2a1c_11ce_ade5_00aa0044773d);
const IID_ISTREAM: GUID = GUID::from_u128(0x0000000c_0000_0000_c000_000000000046);

fn same(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

/// The class object.
#[repr(C)]
struct Factory {
    vtbl: &'static FactoryVtbl,
    refs: AtomicU32,
}

#[repr(C)]
struct FactoryVtbl {
    query_interface: unsafe extern "system" fn(*mut Factory, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut Factory) -> u32,
    release: unsafe extern "system" fn(*mut Factory) -> u32,
    create_instance: unsafe extern "system" fn(*mut Factory, *mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    lock_server: unsafe extern "system" fn(*mut Factory, i32) -> HRESULT,
}

static FACTORY_VTBL: FactoryVtbl = FactoryVtbl {
    query_interface: factory_qi,
    add_ref: factory_add_ref,
    release: factory_release,
    create_instance,
    lock_server,
};

unsafe extern "system" fn factory_qi(this: *mut Factory, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() || iid.is_null() {
        return E_POINTER;
    }
    // SAFETY: COM's pointers, valid for the call
    unsafe {
        if same(&*iid, &IID_IUNKNOWN) || same(&*iid, &IID_ICLASSFACTORY) {
            factory_add_ref(this);
            *out = this as *mut c_void;
            S_OK
        } else {
            *out = std::ptr::null_mut();
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn factory_add_ref(this: *mut Factory) -> u32 {
    // SAFETY: the class object, alive as long as the process
    unsafe { (*this).refs.fetch_add(1, Ordering::AcqRel) + 1 }
}

unsafe extern "system" fn factory_release(this: *mut Factory) -> u32 {
    // (never freed: the one of the process)
    // SAFETY: the class object, alive as long as the process
    unsafe { (*this).refs.fetch_sub(1, Ordering::AcqRel).saturating_sub(1) }
}

unsafe extern "system" fn create_instance(
    _: *mut Factory,
    outer: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    // SAFETY: COM's pointers, valid for the call
    unsafe {
        *out = std::ptr::null_mut();
        if !outer.is_null() {
            return CLASS_E_NOAGGREGATION;
        }
        let d = Dispatch::new(Kind::Server);
        let hr = dispatch_qi(d, iid, out);
        dispatch_release(d);
        hr
    }
}

unsafe extern "system" fn lock_server(_: *mut Factory, lock: i32) -> HRESULT {
    match lock {
        0 => object_gone(),
        _ => object_made(),
    }
    S_OK
}

/// `VARIANT`, of the kinds taken and given here (windows-sys's wants more features).
#[repr(C)]
#[derive(Clone, Copy)]
struct Variant {
    vt: u16,
    reserved: [u16; 3],
    value: usize,
    record: usize,
}

const VT_EMPTY: u16 = 0;
const VT_BSTR: u16 = 8;
const VT_DISPATCH: u16 = 9;
const VT_VARIANT: u16 = 12;
const VT_UNKNOWN: u16 = 13;
const VT_BYREF: u16 = 0x4000;

#[repr(C)]
struct DispParams {
    args: *const Variant,
    named: *const i32,
    count: u32,
    named_count: u32,
}

#[repr(C)]
struct ExcepInfo {
    code: u16,
    reserved: u16,
    source: *mut u16,
    description: *mut u16,
    help_file: *mut u16,
    help_context: u32,
    reserved2: *mut c_void,
    deferred: *mut c_void,
    scode: i32,
}

/// A string as a `BSTR`, owned by whom it is given.
fn bstr(s: &str) -> *mut u16 {
    let w: Vec<u16> = s.encode_utf16().collect();
    // SAFETY: `w.len()` units of `w`
    unsafe { SysAllocStringLen(w.as_ptr(), w.len() as u32) as *mut u16 }
}

/// The text of argument `v` (a `BSTR`, or one by reference).
unsafe fn text_of(v: &Variant) -> Option<String> {
    // SAFETY: a VARIANT of the caller's, of the kind its `vt` tells
    unsafe {
        let b = match v.vt {
            VT_BSTR => v.value as *const u16,
            x if x == VT_BSTR | VT_BYREF => *(v.value as *const *const u16),
            x if x == VT_VARIANT | VT_BYREF => return text_of(&*(v.value as *const Variant)),
            _ => return None,
        };
        if b.is_null() {
            return Some(String::new());
        }
        let n = SysStringLen(b);
        Some(String::from_utf16_lossy(std::slice::from_raw_parts(b, n as usize)))
    }
}

/// What a late-bound object is.
enum Kind {
    /// The class's: `Open`.
    Server,
    /// An archive's: `Info`, `List`, `Stat`, `OpenStream`.
    Archive(Arc<Archive>),
}

impl Kind {
    fn names(&self) -> &'static [&'static str] {
        match self {
            Kind::Server => &["Open"],
            Kind::Archive(_) => &["Info", "List", "Stat", "OpenStream", "Materialize", "Progress", "Cancel"],
        }
    }
}

/// What a method gives.
enum Out {
    Text(String),
    /// An object, of a reference of its own, as `IDispatch` or else `IUnknown`.
    Object(*mut c_void, bool),
}

#[repr(C)]
struct Dispatch {
    vtbl: &'static DispatchVtbl,
    refs: AtomicU32,
    kind: Kind,
}

impl Dispatch {
    /// A new object of `kind`, of one reference.
    fn new(kind: Kind) -> *mut Dispatch {
        object_made();
        Box::into_raw(Box::new(Dispatch { vtbl: &DISPATCH_VTBL, refs: AtomicU32::new(1), kind }))
    }

    /// Method `id` (from 1, of [`Kind::names`]) called with `args` (in their order).
    fn call(&self, id: i32, args: &[String]) -> Result<Out, String> {
        let arg = |k: usize| args.get(k).map(String::as_str).ok_or_else(|| "an argument missing".to_owned());
        let json = |r: Result<Value, String>| r.map(|v| Out::Text(v.to_string()));
        match (&self.kind, id) {
            (Kind::Server, 1) => {
                // (the window to ask a passphrase over, where there is one)
                let owner = args.get(1).and_then(|h| h.parse::<isize>().ok()).unwrap_or(0);
                let a = Archive::open(PathBuf::from(arg(0)?), owner as HWND)?;
                Ok(Out::Object(Dispatch::new(Kind::Archive(Arc::new(a))) as *mut c_void, true))
            }
            (Kind::Archive(a), 1) => Ok(Out::Text(a.info().to_string())),
            (Kind::Archive(a), 2) => {
                let dir = args.first().map_or("", String::as_str);
                json(a.with(|s, h| s.list(h, dir)).map(Value::from))
            }
            (Kind::Archive(a), 3) => {
                let path = arg(0)?;
                json(a.with(|s, h| s.ask(json!({"stat": h, "path": path}))).map(|v| v["entry"].clone()))
            }
            (Kind::Archive(a), 4) => {
                let path = arg(0)?.to_owned();
                let e = a.with(|s, h| s.ask(json!({"stat": h, "path": path})))?;
                let e = &e["entry"];
                if e["kind"] != "file" {
                    return Err(format!("{path}: not a file"));
                }
                Ok(Out::Object(Stream::new(a.clone(), path, e) as *mut c_void, false))
            }
            (Kind::Archive(a), 6) => {
                // (how far the file at `path` is written out, and the archive's evaluation is)
                let path = args.first().map_or("", String::as_str);
                let mut v = a.with(|s, h| s.ask(json!({"progress": h})))?;
                let w = WRITING.lock().unwrap();
                if let Some(e) = w.iter().find(|e| e.of(&a.path, path)) {
                    v["written"] = json!([e.done, e.size]);
                }
                Ok(Out::Text(v.to_string()))
            }
            (Kind::Archive(a), 7) => {
                // (the file at `path` being written out: stopped at its next piece, removed)
                let path = args.first().map_or("", String::as_str);
                let mut w = WRITING.lock().unwrap();
                let found = w.iter_mut().filter(|e| e.of(&a.path, path)).map(|e| e.cancelled = true).count();
                Ok(Out::Text(serde_json::json!({"cancelled": found > 0}).to_string()))
            }
            (Kind::Archive(a), 5) => {
                let path = arg(0)?;
                Ok(Out::Text(materialize(a, path)?.display().to_string()))
            }
            _ => Err("no such method".into()),
        }
    }
}

#[repr(C)]
struct DispatchVtbl {
    query_interface: unsafe extern "system" fn(*mut Dispatch, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut Dispatch) -> u32,
    release: unsafe extern "system" fn(*mut Dispatch) -> u32,
    type_info_count: unsafe extern "system" fn(*mut Dispatch, *mut u32) -> HRESULT,
    type_info: unsafe extern "system" fn(*mut Dispatch, u32, u32, *mut *mut c_void) -> HRESULT,
    ids_of_names:
        unsafe extern "system" fn(*mut Dispatch, *const GUID, *const *const u16, u32, u32, *mut i32) -> HRESULT,
    invoke: unsafe extern "system" fn(
        *mut Dispatch,
        i32,
        *const GUID,
        u32,
        u16,
        *const DispParams,
        *mut Variant,
        *mut ExcepInfo,
        *mut u32,
    ) -> HRESULT,
}

static DISPATCH_VTBL: DispatchVtbl = DispatchVtbl {
    query_interface: dispatch_qi,
    add_ref: dispatch_add_ref,
    release: dispatch_release,
    type_info_count,
    type_info,
    ids_of_names,
    invoke,
};

unsafe extern "system" fn dispatch_qi(this: *mut Dispatch, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() || iid.is_null() {
        return E_POINTER;
    }
    // SAFETY: COM's pointers, valid for the call; `this` a live object
    unsafe {
        if same(&*iid, &IID_IUNKNOWN) || same(&*iid, &IID_IDISPATCH) {
            dispatch_add_ref(this);
            *out = this as *mut c_void;
            S_OK
        } else {
            *out = std::ptr::null_mut();
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn dispatch_add_ref(this: *mut Dispatch) -> u32 {
    // SAFETY: a live object
    unsafe { (*this).refs.fetch_add(1, Ordering::AcqRel) + 1 }
}

unsafe extern "system" fn dispatch_release(this: *mut Dispatch) -> u32 {
    // SAFETY: a live object, boxed by `Dispatch::new`, freed with its last reference
    unsafe {
        let n = (*this).refs.fetch_sub(1, Ordering::AcqRel) - 1;
        if n == 0 {
            drop(Box::from_raw(this));
            object_gone();
        }
        n
    }
}

unsafe extern "system" fn type_info_count(_: *mut Dispatch, n: *mut u32) -> HRESULT {
    if n.is_null() {
        return E_POINTER;
    }
    // SAFETY: COM's pointer, valid for the call
    unsafe { *n = 0 };
    S_OK
}

unsafe extern "system" fn type_info(_: *mut Dispatch, _: u32, _: u32, out: *mut *mut c_void) -> HRESULT {
    if !out.is_null() {
        // SAFETY: COM's pointer, valid for the call
        unsafe { *out = std::ptr::null_mut() };
    }
    E_NOTIMPL
}

unsafe extern "system" fn ids_of_names(
    this: *mut Dispatch,
    _: *const GUID,
    names: *const *const u16,
    count: u32,
    _: u32,
    ids: *mut i32,
) -> HRESULT {
    if names.is_null() || ids.is_null() {
        return E_POINTER;
    }
    let mut hr = S_OK;
    // SAFETY: COM's arrays of `count` names (each ended by 0) and ids; `this` a live object
    unsafe {
        let known = (*this).kind.names();
        for k in 0..count as usize {
            let p = *names.add(k);
            let n = (0..).take_while(|&i| *p.add(i) != 0).count();
            let name = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
            // (the first the method; the others, its arguments' names, not known)
            let id = match k {
                0 => known.iter().position(|m| m.eq_ignore_ascii_case(&name)).map(|i| i as i32 + 1),
                _ => None,
            };
            *ids.add(k) = id.unwrap_or(-1);
            if id.is_none() {
                hr = DISP_E_UNKNOWNNAME;
            }
        }
    }
    hr
}

unsafe extern "system" fn invoke(
    this: *mut Dispatch,
    id: i32,
    _: *const GUID,
    _: u32,
    _: u16,
    params: *const DispParams,
    result: *mut Variant,
    excep: *mut ExcepInfo,
    _: *mut u32,
) -> HRESULT {
    // SAFETY: COM's pointers, valid for the call; the arguments in reverse order; `this` a live
    // object
    unsafe {
        if id < 1 || id as usize > (*this).kind.names().len() {
            return DISP_E_MEMBERNOTFOUND;
        }
        let mut args = Vec::new();
        if let Some(p) = params.as_ref() {
            for k in (0..p.count as usize).rev() {
                match text_of(&*p.args.add(k)) {
                    Some(t) => args.push(t),
                    None => return DISP_E_TYPEMISMATCH,
                }
            }
        }
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (*this).call(id, &args)));
        let r = r.unwrap_or_else(|_| Err("the call failed (a panic)".into()));
        match r {
            Ok(out) => {
                let v = match out {
                    Out::Text(t) => Variant { vt: VT_BSTR, reserved: [0; 3], value: bstr(&t) as usize, record: 0 },
                    Out::Object(o, dispatch) => {
                        let vt = if dispatch { VT_DISPATCH } else { VT_UNKNOWN };
                        Variant { vt, reserved: [0; 3], value: o as usize, record: 0 }
                    }
                };
                match result.as_mut() {
                    Some(r) => *r = v,
                    // (given nowhere: let go of)
                    None => release_variant(v),
                }
                S_OK
            }
            Err(e) => {
                if let Some(r) = result.as_mut() {
                    *r = Variant { vt: VT_EMPTY, reserved: [0; 3], value: 0, record: 0 };
                }
                match excep.as_mut() {
                    Some(x) => {
                        *x = ExcepInfo {
                            code: 0,
                            reserved: 0,
                            source: bstr("Aqueous"),
                            description: bstr(&e),
                            help_file: std::ptr::null_mut(),
                            help_context: 0,
                            reserved2: std::ptr::null_mut(),
                            deferred: std::ptr::null_mut(),
                            scode: E_FAIL,
                        };
                        DISP_E_EXCEPTION
                    }
                    None => E_FAIL,
                }
            }
        }
    }
}

/// A result nobody took, let go of.
unsafe fn release_variant(v: Variant) {
    // SAFETY: of the kinds made here: a BSTR, or a COM object of a reference
    unsafe {
        match v.vt {
            VT_BSTR => windows_sys::Win32::Foundation::SysFreeString(v.value as *const u16),
            VT_DISPATCH | VT_UNKNOWN => {
                let vtbl = *(v.value as *const *const usize);
                let release: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(*vtbl.add(2));
                release(v.value as *mut c_void);
            }
            _ => {}
        }
    }
}

/// A file of an archive, read as an `IStream`.
#[repr(C)]
struct Stream {
    vtbl: &'static StreamVtbl,
    refs: AtomicU32,
    archive: Arc<Archive>,
    path: String,
    size: u64,
    mtime: FILETIME,
    at: Mutex<u64>,
}

impl Stream {
    /// A stream of file `path` of `archive` (`entry`, as `stat` tells it), of one reference.
    fn new(archive: Arc<Archive>, path: String, entry: &Value) -> *mut Stream {
        let size = entry["size"].as_u64().unwrap_or(0);
        object_made();
        Box::into_raw(Box::new(Stream {
            vtbl: &STREAM_VTBL,
            refs: AtomicU32::new(1),
            archive,
            path,
            size,
            mtime: filetime(entry),
            at: Mutex::new(0),
        }))
    }

    /// Up to `into.len()` bytes from `at`: how many (fewer only at the end).
    fn read_at(&self, at: u64, into: &mut [u8]) -> Result<usize, String> {
        let mut n = 0;
        while n < into.len() && at + (n as u64) < self.size {
            let want = (into.len() - n).min(CHUNK);
            let d = self.archive.with(|s, h| s.read(h, &self.path, at + n as u64, want))?;
            if d.is_empty() {
                break;
            }
            into[n..n + d.len()].copy_from_slice(&d);
            n += d.len();
        }
        Ok(n)
    }
}

/// The modification time of `entry` (as `list` and `stat` tell it), as a `FILETIME` (zero where
/// none is told).
pub fn filetime(entry: &Value) -> FILETIME {
    let (Some(s), Some(n)) = (entry["mtime"][0].as_i64(), entry["mtime"][1].as_u64()) else {
        return FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    };
    // (100 ns since 1601)
    let t = (s as i128 + 11_644_473_600) * 10_000_000 + n as i128 / 100;
    let t = t.clamp(0, u64::MAX as i128) as u64;
    FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 }
}

type Unk = *mut c_void;

#[repr(C)]
struct StreamVtbl {
    query_interface: unsafe extern "system" fn(*mut Stream, *const GUID, *mut Unk) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut Stream) -> u32,
    release: unsafe extern "system" fn(*mut Stream) -> u32,
    read: unsafe extern "system" fn(*mut Stream, *mut c_void, u32, *mut u32) -> HRESULT,
    write: unsafe extern "system" fn(*mut Stream, *const c_void, u32, *mut u32) -> HRESULT,
    seek: unsafe extern "system" fn(*mut Stream, i64, u32, *mut u64) -> HRESULT,
    set_size: unsafe extern "system" fn(*mut Stream, u64) -> HRESULT,
    copy_to: unsafe extern "system" fn(*mut Stream, Unk, u64, *mut u64, *mut u64) -> HRESULT,
    commit: unsafe extern "system" fn(*mut Stream, u32) -> HRESULT,
    revert: unsafe extern "system" fn(*mut Stream) -> HRESULT,
    lock_region: unsafe extern "system" fn(*mut Stream, u64, u64, u32) -> HRESULT,
    unlock_region: unsafe extern "system" fn(*mut Stream, u64, u64, u32) -> HRESULT,
    stat: unsafe extern "system" fn(*mut Stream, *mut StatStg, u32) -> HRESULT,
    clone: unsafe extern "system" fn(*mut Stream, *mut Unk) -> HRESULT,
}

static STREAM_VTBL: StreamVtbl = StreamVtbl {
    query_interface: stream_qi,
    add_ref: stream_add_ref,
    release: stream_release,
    read: stream_read,
    write: stream_write,
    seek: stream_seek,
    set_size: stream_set_size,
    copy_to: stream_copy_to,
    commit: stream_commit,
    revert: stream_revert,
    lock_region: stream_lock,
    unlock_region: stream_lock,
    stat: stream_stat,
    clone: stream_clone,
};

/// `STATSTG` (windows-sys's is the same; this keeps the features few).
#[repr(C)]
struct StatStg {
    name: *mut u16,
    kind: u32,
    size: u64,
    mtime: FILETIME,
    ctime: FILETIME,
    atime: FILETIME,
    mode: u32,
    locks: u32,
    clsid: GUID,
    state: u32,
    reserved: u32,
}

unsafe extern "system" fn stream_qi(this: *mut Stream, iid: *const GUID, out: *mut Unk) -> HRESULT {
    if out.is_null() || iid.is_null() {
        return E_POINTER;
    }
    // SAFETY: COM's pointers, valid for the call; `this` a live stream
    unsafe {
        let iid = &*iid;
        if same(iid, &IID_IUNKNOWN) || same(iid, &IID_ISEQUENTIALSTREAM) || same(iid, &IID_ISTREAM) {
            stream_add_ref(this);
            *out = this as Unk;
            S_OK
        } else {
            *out = std::ptr::null_mut();
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn stream_add_ref(this: *mut Stream) -> u32 {
    // SAFETY: a live stream
    unsafe { (*this).refs.fetch_add(1, Ordering::AcqRel) + 1 }
}

unsafe extern "system" fn stream_release(this: *mut Stream) -> u32 {
    // SAFETY: a live stream, boxed by `Stream::new`, freed with its last reference
    unsafe {
        let n = (*this).refs.fetch_sub(1, Ordering::AcqRel) - 1;
        if n == 0 {
            drop(Box::from_raw(this));
            object_gone();
        }
        n
    }
}

unsafe extern "system" fn stream_read(this: *mut Stream, into: *mut c_void, n: u32, read: *mut u32) -> HRESULT {
    if into.is_null() && n > 0 {
        return E_POINTER;
    }
    // SAFETY: COM's buffer of `n` bytes; `this` a live stream
    unsafe {
        let s = &*this;
        let buf = std::slice::from_raw_parts_mut(into as *mut u8, n as usize);
        let mut at = s.at.lock().unwrap();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.read_at(*at, buf)));
        let got = match r {
            Ok(Ok(got)) => got,
            _ => {
                if !read.is_null() {
                    *read = 0;
                }
                return STG_E_READFAULT;
            }
        };
        *at += got as u64;
        if !read.is_null() {
            *read = got as u32;
        }
        if got < n as usize { S_FALSE } else { S_OK }
    }
}

unsafe extern "system" fn stream_write(_: *mut Stream, _: *const c_void, _: u32, written: *mut u32) -> HRESULT {
    if !written.is_null() {
        // SAFETY: COM's pointer, valid for the call
        unsafe { *written = 0 };
    }
    STG_E_ACCESSDENIED
}

unsafe extern "system" fn stream_seek(this: *mut Stream, by: i64, from: u32, to: *mut u64) -> HRESULT {
    // SAFETY: a live stream; COM's pointer, valid for the call
    unsafe {
        let s = &*this;
        let mut at = s.at.lock().unwrap();
        let base = match from {
            0 => 0,
            1 => *at as i128,
            2 => s.size as i128,
            _ => return STG_E_INVALIDFUNCTION,
        };
        let new = base + by as i128;
        if new < 0 {
            return STG_E_INVALIDFUNCTION;
        }
        *at = new as u64;
        if !to.is_null() {
            *to = *at;
        }
        S_OK
    }
}

unsafe extern "system" fn stream_set_size(_: *mut Stream, _: u64) -> HRESULT {
    STG_E_ACCESSDENIED
}

unsafe extern "system" fn stream_copy_to(
    this: *mut Stream,
    to: Unk,
    n: u64,
    read: *mut u64,
    written: *mut u64,
) -> HRESULT {
    if to.is_null() {
        return E_POINTER;
    }
    type Write = unsafe extern "system" fn(Unk, *const c_void, u32, *mut u32) -> HRESULT;
    // SAFETY: a live stream; `to` an ISequentialStream (or IStream): Write the fifth of its
    // vtable (after IUnknown's three and Read)
    unsafe {
        let s = &*this;
        let write: Write = std::mem::transmute(*(*(to as *const *const usize)).add(4));
        let mut buf = vec![0u8; CHUNK];
        let (mut r, mut w) = (0u64, 0u64);
        let mut hr = S_OK;
        let mut at = s.at.lock().unwrap();
        while r < n {
            let want = (n - r).min(CHUNK as u64) as usize;
            let got = match s.read_at(*at, &mut buf[..want]) {
                Ok(0) => break,
                Ok(got) => got,
                Err(_) => {
                    hr = STG_E_READFAULT;
                    break;
                }
            };
            *at += got as u64;
            r += got as u64;
            let mut put = 0;
            hr = write(to, buf.as_ptr() as *const c_void, got as u32, &mut put);
            w += put as u64;
            if hr < 0 {
                break;
            }
        }
        if !read.is_null() {
            *read = r;
        }
        if !written.is_null() {
            *written = w;
        }
        if hr < 0 { hr } else { S_OK }
    }
}

unsafe extern "system" fn stream_commit(_: *mut Stream, _: u32) -> HRESULT {
    S_OK
}

unsafe extern "system" fn stream_revert(_: *mut Stream) -> HRESULT {
    E_NOTIMPL
}

unsafe extern "system" fn stream_lock(_: *mut Stream, _: u64, _: u64, _: u32) -> HRESULT {
    STG_E_INVALIDFUNCTION
}

unsafe extern "system" fn stream_stat(this: *mut Stream, stat: *mut StatStg, flag: u32) -> HRESULT {
    if stat.is_null() {
        return E_POINTER;
    }
    // SAFETY: a live stream; COM's structure to fill; the name of COM's memory, the caller's
    unsafe {
        let s = &*this;
        let name = match flag & 1 {
            // STATFLAG_NONAME
            1 => std::ptr::null_mut(),
            _ => {
                let leaf = s.path.rsplit('/').next().unwrap_or("");
                let w: Vec<u16> = leaf.encode_utf16().chain([0]).collect();
                let p = CoTaskMemAlloc(w.len() * 2) as *mut u16;
                if p.is_null() {
                    return E_OUTOFMEMORY;
                }
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                p
            }
        };
        *stat = StatStg {
            name,
            kind: 2, // STGTY_STREAM
            size: s.size,
            mtime: s.mtime,
            ctime: FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 },
            atime: FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 },
            mode: 0, // STGM_READ
            locks: 0,
            clsid: GUID::from_u128(0),
            state: 0,
            reserved: 0,
        };
        S_OK
    }
}

unsafe extern "system" fn stream_clone(this: *mut Stream, out: *mut Unk) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    // SAFETY: a live stream; COM's pointer, valid for the call
    unsafe {
        let s = &*this;
        let at = *s.at.lock().unwrap();
        object_made();
        let c = Box::into_raw(Box::new(Stream {
            vtbl: &STREAM_VTBL,
            refs: AtomicU32::new(1),
            archive: s.archive.clone(),
            path: s.path.clone(),
            size: s.size,
            mtime: s.mtime,
            at: Mutex::new(at),
        }));
        *out = c as Unk;
        S_OK
    }
}
