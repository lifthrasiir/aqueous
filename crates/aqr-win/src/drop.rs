//! The paths chosen in Explorer taken at once, as a drop: each verb of many paths is also a
//! `DropTarget` (`register`), a COM class of this program as a local server
//! (`LocalServer32`: `aqr-win --drop VERB`). Explorer starts it and drops the selection on it,
//! all of it in one `IDataObject`, rather than running the verb's command for each path.
//!
//! The command, one process for each path gathered into one ([`crate::gather`]), stays for the
//! programs that run the verb so; but Explorer asks before it runs a command on a file marked
//! as from the Internet (`Zone.Identifier`) of a type that runs (`.js`, ...), and the gathering
//! does not wait for the answer: the selection was cut in two archives, or more.
//!
//! The class is served on a thread of its own (the multithreaded apartment: no window, no
//! messages), the main thread free for the dialogs as it is otherwise; once a drop came, its
//! paths go to the main thread and the class is revoked.

use std::ffi::OsString;
use std::ffi::c_void;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CLASS_E_NOAGGREGATION, E_FAIL, E_NOINTERFACE, E_NOTIMPL, E_POINTER, S_FALSE, S_OK,
};
use windows_sys::Win32::System::Com::{
    CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, CoInitializeEx, CoRegisterClassObject, CoRevokeClassObject,
    CoUninitialize, FORMATETC, REGCLS_SINGLEUSE, TYMED_HGLOBAL,
};
use windows_sys::Win32::System::Ole::{CF_HDROP, DROPEFFECT_COPY, DROPEFFECT_NONE};
use windows_sys::Win32::UI::Shell::DragQueryFileW;
use windows_sys::core::{GUID, HRESULT};

/// The verbs taking many paths, each a class of its own (aqr-ipc's, which the shell extension's
/// menu drops on too).
pub const VERBS: [&str; 8] = aqr_ipc::shell::DROP_VERBS;

/// The CLSID of `verb`'s class, where it is one of [`VERBS`].
pub fn clsid(verb: &str) -> Option<GUID> {
    aqr_ipc::shell::drop_class(verb).map(GUID::from_u128)
}

/// `{...}`, as the registry writes a CLSID.
pub fn braced(g: &GUID) -> String {
    let d4 = g.data4;
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1, g.data2, g.data3, d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7]
    )
}

/// How long the class waits for its drop: Explorer drops right after it starts the program.
const WAIT: Duration = Duration::from_secs(60);

/// What a drop gave: its paths, or why there are none.
type Paths = Result<Vec<PathBuf>, String>;

/// The paths dropped on `verb`'s class, served from here as Explorer starts this program to
/// drop them; why there are none.
pub fn receive(verb: &str) -> Paths {
    let clsid = clsid(verb).ok_or_else(|| format!("no class of the verb {verb}"))?;
    let (tx, rx) = channel();
    // (the thread outlives this function: the class revoked, Explorer's references let go)
    std::thread::spawn(move || serve(clsid, tx));
    match rx.recv_timeout(WAIT) {
        Ok(Ok(paths)) if paths.is_empty() => Err("nothing dropped".into()),
        Ok(r) => r,
        Err(_) => Err("no drop came".into()),
    }
}

/// Serves `clsid` in this thread until a drop, its paths (or what failed) sent to `tx`.
fn serve(clsid: GUID, tx: Sender<Paths>) {
    // SAFETY: this thread's, of no other COM use
    let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) };
    if hr < 0 {
        let _ = tx.send(Err(format!("CoInitializeEx: {hr:#010x}")));
        return;
    }
    let (dropped, drops) = channel();
    *SINK.lock().unwrap() = Some(dropped);
    let factory =
        Box::into_raw(Box::new(Object { vtbl: &FACTORY_VTBL as *const _ as *const c_void, refs: AtomicU32::new(1) }));
    let mut cookie = 0;
    // SAFETY: a CLSID and a class object of a static vtable, alive until revoked (never freed:
    // a few bytes, once a process)
    let hr = unsafe {
        CoRegisterClassObject(&clsid, factory as *mut c_void, CLSCTX_LOCAL_SERVER, REGCLS_SINGLEUSE as u32, &mut cookie)
    };
    if hr < 0 {
        let _ = tx.send(Err(format!("CoRegisterClassObject: {hr:#010x}")));
        return;
    }
    let paths = drops.recv_timeout(WAIT);
    // SAFETY: registered above
    unsafe { CoRevokeClassObject(cookie) };
    if let Ok(paths) = paths {
        let _ = tx.send(paths);
    }
    // (Explorer's last references let go: its Release comes right after the drop)
    std::thread::sleep(Duration::from_millis(200));
    // SAFETY: initialized above
    unsafe { CoUninitialize() };
}

/// Where the drop sends its paths, to the thread serving the class.
static SINK: Mutex<Option<Sender<Paths>>> = Mutex::new(None);

/// A COM object of no state but its references: the class factory, or the drop target (with
/// `IPersistFile` beside it, the same object through a second vtable, as Explorer may load it
/// with the item first).
#[repr(C)]
struct Object {
    vtbl: *const c_void,
    refs: AtomicU32,
}

/// The drop target: its `IDropTarget` and `IPersistFile` vtables.
#[repr(C)]
struct Target {
    drop: *const c_void,
    persist: *const c_void,
    refs: AtomicU32,
}

const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_ICLASSFACTORY: GUID = GUID::from_u128(0x00000001_0000_0000_c000_000000000046);
const IID_IDROPTARGET: GUID = GUID::from_u128(0x00000122_0000_0000_c000_000000000046);
const IID_IPERSIST: GUID = GUID::from_u128(0x0000010c_0000_0000_c000_000000000046);
const IID_IPERSISTFILE: GUID = GUID::from_u128(0x0000010b_0000_0000_c000_000000000046);

fn same(a: &GUID, b: &GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

#[repr(C)]
struct FactoryVtbl {
    query_interface: unsafe extern "system" fn(*mut Object, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut Object) -> u32,
    release: unsafe extern "system" fn(*mut Object) -> u32,
    create_instance: unsafe extern "system" fn(*mut Object, *mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    lock_server: unsafe extern "system" fn(*mut Object, i32) -> HRESULT,
}

static FACTORY_VTBL: FactoryVtbl = FactoryVtbl {
    query_interface: factory_qi,
    add_ref: factory_add_ref,
    release: factory_release,
    create_instance,
    lock_server,
};

unsafe extern "system" fn factory_qi(this: *mut Object, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() {
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

unsafe extern "system" fn factory_add_ref(this: *mut Object) -> u32 {
    // SAFETY: a live object
    unsafe { (*this).refs.fetch_add(1, Ordering::AcqRel) + 1 }
}

unsafe extern "system" fn factory_release(this: *mut Object) -> u32 {
    // (never freed: the one of the process, its last reference COM's own as it is revoked)
    // SAFETY: a live object
    unsafe { (*this).refs.fetch_sub(1, Ordering::AcqRel).saturating_sub(1) }
}

unsafe extern "system" fn create_instance(
    _this: *mut Object,
    outer: *mut c_void,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    // SAFETY: COM's pointers, valid for the call; the target boxed, freed by its last Release
    unsafe {
        *out = std::ptr::null_mut();
        if !outer.is_null() {
            return CLASS_E_NOAGGREGATION;
        }
        let t = Box::into_raw(Box::new(Target {
            drop: &DROP_VTBL as *const _ as *const c_void,
            persist: &PERSIST_VTBL as *const _ as *const c_void,
            refs: AtomicU32::new(1),
        }));
        let hr = target_qi(t, iid, out);
        target_release(t);
        hr
    }
}

unsafe extern "system" fn lock_server(_this: *mut Object, _lock: i32) -> HRESULT {
    S_OK
}

#[repr(C)]
struct DropVtbl {
    query_interface: unsafe extern "system" fn(*mut Target, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut Target) -> u32,
    release: unsafe extern "system" fn(*mut Target) -> u32,
    drag_enter: unsafe extern "system" fn(*mut Target, *mut c_void, u32, i64, *mut u32) -> HRESULT,
    drag_over: unsafe extern "system" fn(*mut Target, u32, i64, *mut u32) -> HRESULT,
    drag_leave: unsafe extern "system" fn(*mut Target) -> HRESULT,
    drop: unsafe extern "system" fn(*mut Target, *mut c_void, u32, i64, *mut u32) -> HRESULT,
}

static DROP_VTBL: DropVtbl = DropVtbl {
    query_interface: target_qi,
    add_ref: target_add_ref,
    release: target_release,
    drag_enter,
    drag_over,
    drag_leave,
    drop: on_drop,
};

/// The target of its `IPersistFile` pointer (the second field).
unsafe fn of_persist(this: *mut c_void) -> *mut Target {
    // SAFETY: the pointer is the `persist` field of a Target
    unsafe { (this as *mut *const c_void).sub(1) as *mut Target }
}

#[repr(C)]
struct PersistVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_class_id: unsafe extern "system" fn(*mut c_void, *mut GUID) -> HRESULT,
    is_dirty: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    load: unsafe extern "system" fn(*mut c_void, *const u16, u32) -> HRESULT,
    save: unsafe extern "system" fn(*mut c_void, *const u16, i32) -> HRESULT,
    save_completed: unsafe extern "system" fn(*mut c_void, *const u16) -> HRESULT,
    get_cur_file: unsafe extern "system" fn(*mut c_void, *mut *mut u16) -> HRESULT,
}

static PERSIST_VTBL: PersistVtbl = PersistVtbl {
    query_interface: persist_qi,
    add_ref: persist_add_ref,
    release: persist_release,
    get_class_id: persist_class_id,
    is_dirty: persist_is_dirty,
    load: persist_load,
    save: persist_save,
    save_completed: persist_save_completed,
    get_cur_file: persist_cur_file,
};

unsafe extern "system" fn persist_qi(this: *mut c_void, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    // SAFETY: an IPersistFile pointer of a live target
    unsafe { target_qi(of_persist(this), iid, out) }
}

unsafe extern "system" fn persist_add_ref(this: *mut c_void) -> u32 {
    // SAFETY: an IPersistFile pointer of a live target
    unsafe { target_add_ref(of_persist(this)) }
}

unsafe extern "system" fn persist_release(this: *mut c_void) -> u32 {
    // SAFETY: an IPersistFile pointer of a live target
    unsafe { target_release(of_persist(this)) }
}

unsafe extern "system" fn persist_class_id(_: *mut c_void, _: *mut GUID) -> HRESULT {
    E_NOTIMPL
}

unsafe extern "system" fn persist_is_dirty(_: *mut c_void) -> HRESULT {
    S_FALSE
}

/// (the item the menu was of: the drop tells them all)
unsafe extern "system" fn persist_load(_: *mut c_void, _: *const u16, _: u32) -> HRESULT {
    S_OK
}

unsafe extern "system" fn persist_save(_: *mut c_void, _: *const u16, _: i32) -> HRESULT {
    E_NOTIMPL
}

unsafe extern "system" fn persist_save_completed(_: *mut c_void, _: *const u16) -> HRESULT {
    E_NOTIMPL
}

unsafe extern "system" fn persist_cur_file(_: *mut c_void, _: *mut *mut u16) -> HRESULT {
    E_NOTIMPL
}

unsafe extern "system" fn target_qi(this: *mut Target, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    // SAFETY: COM's pointers, valid for the call; `this` a live target
    unsafe {
        let iid = &*iid;
        *out = if same(iid, &IID_IUNKNOWN) || same(iid, &IID_IDROPTARGET) {
            &raw mut (*this).drop as *mut c_void
        } else if same(iid, &IID_IPERSIST) || same(iid, &IID_IPERSISTFILE) {
            &raw mut (*this).persist as *mut c_void
        } else {
            return E_NOINTERFACE;
        };
        target_add_ref(this);
        S_OK
    }
}

unsafe extern "system" fn target_add_ref(this: *mut Target) -> u32 {
    // SAFETY: a live target
    unsafe { (*this).refs.fetch_add(1, Ordering::AcqRel) + 1 }
}

unsafe extern "system" fn target_release(this: *mut Target) -> u32 {
    // SAFETY: a live target, boxed by create_instance, freed with its last reference
    unsafe {
        let n = (*this).refs.fetch_sub(1, Ordering::AcqRel) - 1;
        if n == 0 {
            drop(Box::from_raw(this));
        }
        n
    }
}

unsafe extern "system" fn drag_enter(_: *mut Target, _: *mut c_void, _: u32, _: i64, effect: *mut u32) -> HRESULT {
    // SAFETY: COM's pointer, valid for the call
    unsafe { set_effect(effect, DROPEFFECT_COPY) }
}

unsafe extern "system" fn drag_over(_: *mut Target, _: u32, _: i64, effect: *mut u32) -> HRESULT {
    // SAFETY: COM's pointer, valid for the call
    unsafe { set_effect(effect, DROPEFFECT_COPY) }
}

unsafe extern "system" fn drag_leave(_: *mut Target) -> HRESULT {
    S_OK
}

unsafe fn set_effect(effect: *mut u32, e: u32) -> HRESULT {
    if !effect.is_null() {
        // SAFETY: COM's pointer, valid for the call
        unsafe { *effect = e };
    }
    S_OK
}

unsafe extern "system" fn on_drop(_: *mut Target, data: *mut c_void, _: u32, _: i64, effect: *mut u32) -> HRESULT {
    // SAFETY: COM's IDataObject, valid for the call
    let paths = unsafe { hdrop(data) };
    let ok = paths.is_ok();
    if let Some(tx) = SINK.lock().unwrap().take() {
        let _ = tx.send(paths);
    }
    // SAFETY: COM's pointer, valid for the call
    unsafe { set_effect(effect, if ok { DROPEFFECT_COPY } else { DROPEFFECT_NONE }) };
    if ok { S_OK } else { E_FAIL }
}

#[link(name = "ole32")]
unsafe extern "system" {
    /// (windows-sys's wants its `STGMEDIUM`, of the features of GDI and storage)
    fn ReleaseStgMedium(medium: *mut Medium);
}

/// `STGMEDIUM` of an `HGLOBAL` (windows-sys's own wants features of GDI and storage).
#[repr(C)]
struct Medium {
    tymed: u32,
    hglobal: *mut c_void,
    release: *mut c_void,
}

/// The paths of `data`'s `CF_HDROP`.
unsafe fn hdrop(data: *mut c_void) -> Paths {
    type GetData = unsafe extern "system" fn(*mut c_void, *const FORMATETC, *mut Medium) -> HRESULT;
    if data.is_null() {
        return Err("no data dropped".into());
    }
    let format = FORMATETC {
        cfFormat: CF_HDROP,
        ptd: std::ptr::null_mut(),
        dwAspect: 1, // DVASPECT_CONTENT
        lindex: -1,
        tymed: TYMED_HGLOBAL as u32,
    };
    let mut m = Medium { tymed: 0, hglobal: std::ptr::null_mut(), release: std::ptr::null_mut() };
    // SAFETY: an IDataObject: GetData the fourth of its vtable (after IUnknown's three)
    let hr = unsafe {
        let vtbl = *(data as *const *const usize);
        let get: GetData = std::mem::transmute(*vtbl.add(3));
        get(data, &format, &mut m)
    };
    if hr < 0 {
        return Err(format!("no paths dropped (GetData: {hr:#010x})"));
    }
    let mut paths = Vec::new();
    // SAFETY: an HDROP of the medium, released after; the lengths asked first
    unsafe {
        let n = DragQueryFileW(m.hglobal, u32::MAX, std::ptr::null_mut(), 0);
        for i in 0..n {
            let len = DragQueryFileW(m.hglobal, i, std::ptr::null_mut(), 0);
            let mut w = vec![0u16; len as usize + 1];
            let got = DragQueryFileW(m.hglobal, i, w.as_mut_ptr(), w.len() as u32);
            w.truncate(got as usize);
            paths.push(OsString::from_wide(&w).into());
        }
        ReleaseStgMedium(&mut m);
    }
    Ok(paths)
}
