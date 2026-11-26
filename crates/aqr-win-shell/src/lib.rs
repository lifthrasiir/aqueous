//! Aqueous archives browsed in Windows Explorer as folders (`docs/win-shell-ext-design.md`): a
//! shell namespace extension, `aqr_shell.dll`, of the class of `.aqr` files
//! ([`folder::FOLDER`], a junction as Windows' ZIP folders are). It is loaded into Explorer
//! and into every program that opens a file dialog, so it is thin: what it lists and reads it
//! asks `aqr-win --serve` ([`client`]), a COM local server over `aqr --serve`, and a panic in
//! it is caught at every entry point (a failure, not another program's crash).
#![cfg(windows)]
// (the COM methods take their callers' pointers as their interfaces have them, of signatures the
// `windows` crate gives)
#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod client;
mod columns;
mod data;
pub mod folder;
mod handlers;
mod menu;
pub mod pidl;

use std::sync::atomic::{AtomicUsize, Ordering};

use windows::Win32::Foundation::{CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_POINTER, S_FALSE, S_OK};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::core::{BOOL, GUID, HRESULT, IUnknown, Interface, Ref, implement};

/// Where what the library does is told, a line at a time, where `AQR_SHELL_LOG` names a file
/// (for debugging: nothing is written otherwise).
static LOG: std::sync::LazyLock<Option<std::sync::Mutex<std::fs::File>>> = std::sync::LazyLock::new(|| {
    let path = std::env::var_os("AQR_SHELL_LOG")?;
    std::fs::OpenOptions::new().create(true).append(true).open(path).ok().map(std::sync::Mutex::new)
});

/// Tells `what`, where there is a log ([`LOG`]).
pub fn log(what: impl FnOnce() -> String) {
    if let Some(f) = LOG.as_ref() {
        use std::io::Write;
        _ = writeln!(f.lock().unwrap(), "{:?} {}", std::thread::current().id(), what());
    }
}

/// Of the class of the tests: nothing shown (boxes) nor told to the shell (changes, which would
/// have Explorer, the user's, load the library of a test), but logged.
pub static TESTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The objects of this library alive, and the locks of its class factories: none, it may be
/// unloaded.
static OBJECTS: AtomicUsize = AtomicUsize::new(0);

/// One of [`OBJECTS`], for as long as it is kept.
pub struct Counted(());

impl Counted {
    pub fn new() -> Counted {
        OBJECTS.fetch_add(1, Ordering::SeqCst);
        Counted(())
    }
}

impl Default for Counted {
    fn default() -> Self {
        Counted::new()
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        OBJECTS.fetch_sub(1, Ordering::SeqCst);
    }
}

#[implement(IClassFactory)]
struct Factory {
    clsid: GUID,
    _counted: Counted,
}

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        if ppv.is_null() || riid.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: the caller's pointer
        unsafe { *ppv = std::ptr::null_mut() };
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let r = std::panic::catch_unwind(|| {
            let f: windows::Win32::UI::Shell::IShellFolder2 = folder::Folder::new(self.clsid).into();
            // SAFETY: the caller's pointers, given a reference
            unsafe { f.query(riid, ppv) }
        });
        r.unwrap_or(windows::Win32::Foundation::E_UNEXPECTED).ok()
    }

    fn LockServer(&self, lock: BOOL) -> windows::core::Result<()> {
        match lock.as_bool() {
            true => OBJECTS.fetch_add(1, Ordering::SeqCst),
            false => OBJECTS.fetch_sub(1, Ordering::SeqCst),
        };
        Ok(())
    }
}

/// The class object of `clsid` (of [`folder::FOLDER`], or of [`folder::TEST_FOLDER`], served by
/// the test's server).
///
/// # Safety
///
/// COM's pointers, valid for the call.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    clsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut core::ffi::c_void,
) -> HRESULT {
    if clsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    // SAFETY: COM's pointers
    unsafe {
        *ppv = std::ptr::null_mut();
        let clsid = *clsid;
        log(|| format!("DllGetClassObject {clsid:?}"));
        static LANGUAGE: std::sync::Once = std::sync::Once::new();
        LANGUAGE.call_once(|| _ = aqr_l10n::init(&aqr_l10n::system_languages()));
        match clsid {
            folder::FOLDER => {}
            folder::TEST_FOLDER => {
                // (of the test's run, where it tells one)
                let class = std::env::var("AQR_SHELL_TEST_SERVER")
                    .ok()
                    .and_then(|c| GUID::try_from(c.trim_matches(['{', '}'])).ok())
                    .unwrap_or(folder::TEST_SERVER);
                client::set_server(class);
                TESTING.store(true, Ordering::Relaxed);
            }
            _ => return CLASS_E_CLASSNOTAVAILABLE,
        }
        let f: IClassFactory = Factory { clsid, _counted: Counted::new() }.into();
        f.query(riid, ppv)
    }
}

/// Whether the library may be unloaded: of no object alive.
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if OBJECTS.load(Ordering::SeqCst) == 0 { S_OK } else { S_FALSE }
}
