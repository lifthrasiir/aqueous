//! `aqr-win --serve` driven as the shell extension drives it, through COM from another process:
//! the class (of a CLSID of the test's, registered by the server as it runs, so that nothing
//! is written to the registry) opening an archive, its entries listed, a file read through an
//! `IStream`, an error told by `EXCEPINFO`, and the server ending once idle.
//!
//! It needs `aqr.exe` beside `aqr-win.exe` (`cargo rt -p aqr -p aqr-win --test serve --
//! --ignored`): the tests of this package alone do not build it.
#![cfg(windows)]

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;
use windows_sys::Win32::Foundation::{SysAllocStringLen, SysFreeString, SysStringLen};
use windows_sys::Win32::System::Com::{CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
use windows_sys::core::{GUID, HRESULT};

const CLSID: GUID = GUID::from_u128(0x5e1f0b6a_92c4_4d1e_a7f3_0c2b9d8e7a61);
const IID_IDISPATCH: GUID = GUID::from_u128(0x00020400_0000_0000_c000_000000000046);
const IID_ISTREAM: GUID = GUID::from_u128(0x0000000c_0000_0000_c000_000000000046);

fn aqr() -> PathBuf {
    let aqr = Path::new(env!("CARGO_BIN_EXE_aqr-win")).with_file_name("aqr.exe");
    assert!(aqr.is_file(), "{}: not built (cargo rt -p aqr -p aqr-win)", aqr.display());
    aqr
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Variant {
    vt: u16,
    reserved: [u16; 3],
    value: usize,
    record: usize,
}

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

/// Entry `k` of the vtable of COM object `o`.
unsafe fn method<F: Copy>(o: *mut c_void, k: usize) -> F {
    // SAFETY: a COM object, of a vtable of at least `k + 1` entries of the type asked
    unsafe { std::mem::transmute_copy(&*(*(o as *const *const usize)).add(k)) }
}

unsafe fn release(o: *mut c_void) {
    // SAFETY: a COM object: Release the third of its vtable
    unsafe { method::<unsafe extern "system" fn(*mut c_void) -> u32>(o, 2)(o) };
}

fn bstr(s: &str) -> *mut u16 {
    let w: Vec<u16> = s.encode_utf16().collect();
    // SAFETY: `w.len()` units of `w`
    unsafe { SysAllocStringLen(w.as_ptr(), w.len() as u32) as *mut u16 }
}

unsafe fn text(b: *const u16) -> String {
    // SAFETY: a BSTR
    unsafe { String::from_utf16_lossy(std::slice::from_raw_parts(b, SysStringLen(b) as usize)) }
}

/// What method `name` of late-bound object `o` gives of the strings `args`: a string, or an
/// object; or the description of its error.
enum Got {
    Text(String),
    Object(*mut c_void),
}

unsafe fn call(o: *mut c_void, name: &str, args: &[&str]) -> Result<Got, String> {
    type Ids = unsafe extern "system" fn(*mut c_void, *const GUID, *const *const u16, u32, u32, *mut i32) -> HRESULT;
    type Invoke = unsafe extern "system" fn(
        *mut c_void,
        i32,
        *const GUID,
        u32,
        u16,
        *const DispParams,
        *mut Variant,
        *mut ExcepInfo,
        *mut u32,
    ) -> HRESULT;
    // SAFETY: an IDispatch: GetIDsOfNames the sixth of its vtable, Invoke the seventh
    unsafe {
        let w: Vec<u16> = name.encode_utf16().chain([0]).collect();
        let mut id = 0;
        let hr = method::<Ids>(o, 5)(o, &GUID::from_u128(0), &w.as_ptr(), 1, 0, &mut id);
        if hr < 0 {
            return Err(format!("GetIDsOfNames({name}): {hr:#010x}"));
        }
        // (in reverse order)
        let vs: Vec<Variant> = args
            .iter()
            .rev()
            .map(|a| Variant { vt: 8, reserved: [0; 3], value: bstr(a) as usize, record: 0 })
            .collect();
        let p = DispParams { args: vs.as_ptr(), named: std::ptr::null(), count: vs.len() as u32, named_count: 0 };
        let mut r = Variant { vt: 0, reserved: [0; 3], value: 0, record: 0 };
        let mut x: ExcepInfo = std::mem::zeroed();
        let hr = method::<Invoke>(o, 6)(o, id, &GUID::from_u128(0), 0, 1, &p, &mut r, &mut x, std::ptr::null_mut());
        vs.iter().for_each(|v| SysFreeString(v.value as *const u16));
        if hr < 0 {
            let m = if x.description.is_null() { format!("{hr:#010x}") } else { text(x.description) };
            for b in [x.source, x.description, x.help_file] {
                SysFreeString(b);
            }
            return Err(m);
        }
        match r.vt {
            8 => {
                let t = text(r.value as *const u16);
                SysFreeString(r.value as *const u16);
                Ok(Got::Text(t))
            }
            9 | 13 => Ok(Got::Object(r.value as *mut c_void)),
            vt => Err(format!("a result of type {vt}")),
        }
    }
}

unsafe fn json(o: *mut c_void, name: &str, args: &[&str]) -> Value {
    // SAFETY: as `call`
    match unsafe { call(o, name, args) } {
        Ok(Got::Text(t)) => serde_json::from_str(&t).unwrap(),
        Ok(Got::Object(_)) => panic!("{name}: an object"),
        Err(e) => panic!("{name}: {e}"),
    }
}

unsafe fn object(o: *mut c_void, name: &str, args: &[&str]) -> *mut c_void {
    // SAFETY: as `call`
    match unsafe { call(o, name, args) } {
        Ok(Got::Object(o)) => o,
        Ok(Got::Text(t)) => panic!("{name}: {t}"),
        Err(e) => panic!("{name}: {e}"),
    }
}

/// All of the stream of object `o` (an `IUnknown`, let go of), read `n` bytes at a time.
unsafe fn read_all(o: *mut c_void, n: usize) -> Vec<u8> {
    type Qi = unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT;
    type Read = unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut u32) -> HRESULT;
    let mut s = std::ptr::null_mut();
    // SAFETY: a COM object: QueryInterface the first of its vtable
    unsafe {
        assert!(method::<Qi>(o, 0)(o, &IID_ISTREAM, &mut s) >= 0, "no IStream");
        release(o);
    }
    let (mut all, mut buf) = (Vec::new(), vec![0u8; n]);
    loop {
        let mut got = 0;
        // SAFETY: an IStream: Read the fourth of its vtable
        let hr = unsafe { method::<Read>(s, 3)(s, buf.as_mut_ptr() as *mut c_void, n as u32, &mut got) };
        assert!(hr >= 0, "Read: {hr:#010x}");
        all.extend_from_slice(&buf[..got as usize]);
        if got == 0 {
            // SAFETY: of a reference of its own
            unsafe { release(s) };
            return all;
        }
    }
}

#[test]
#[ignore = "needs aqr.exe beside aqr-win.exe"]
fn served() {
    let aqr = aqr();
    let d = std::env::temp_dir().join(format!("aqr-win-serve-{}", std::process::id()));
    _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("t/sub")).unwrap();
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(d.join("t/a.txt"), b"some text\n".repeat(100)).unwrap();
    std::fs::write(d.join("t/sub/big.bin"), &big).unwrap();
    let o = Command::new(&aqr).current_dir(&d).args(["-q", "-o", "t.aqr", "t"]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    let mut server = Command::new(env!("CARGO_BIN_EXE_aqr-win"))
        .args(["--serve", "{5E1F0B6A-92C4-4D1E-A7F3-0C2B9D8E7A61}"])
        .env("AQR", &aqr)
        .env("AQR_SERVE_IDLE", "0.2,2")
        .spawn()
        .unwrap();
    // SAFETY: this thread's COM, and the objects of the server through it
    unsafe {
        assert!(CoInitializeEx(std::ptr::null(), COINIT_MULTITHREADED as u32) >= 0);
        // (once it has registered its class)
        let start = Instant::now();
        let mut s = std::ptr::null_mut();
        while CoCreateInstance(&CLSID, std::ptr::null_mut(), CLSCTX_LOCAL_SERVER, &IID_IDISPATCH, &mut s) < 0 {
            assert!(start.elapsed() < Duration::from_secs(10), "the class never came");
            std::thread::sleep(Duration::from_millis(50));
        }
        let path = d.join("t.aqr");
        let a = object(s, "Open", &[path.to_str().unwrap()]);
        assert_eq!(json(a, "Info", &[])["single"], false);
        let root = json(a, "List", &[""]);
        let mut names: Vec<&str> = root.as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
        names.sort();
        assert_eq!(names, ["a.txt", "sub"]);
        assert_eq!(json(a, "Stat", &["sub/big.bin"])["size"], 3_000_000);
        let st = object(a, "OpenStream", &["sub/big.bin"]);
        assert!(read_all(st, 700_000) == big);
        let st = object(a, "OpenStream", &["a.txt"]);
        assert_eq!(read_all(st, 4096), b"some text\n".repeat(100));
        release(st);
        match call(a, "OpenStream", &["nope"]) {
            Err(e) => assert!(e.contains("not in the archive"), "{e}"),
            Ok(_) => panic!("a stream of nothing"),
        }
        match call(s, "Open", &[d.join("missing.aqr").to_str().unwrap()]) {
            Err(e) => assert!(e.contains("missing.aqr"), "{e}"),
            Ok(_) => panic!("an archive of nothing"),
        }
        release(a);
        release(s);
    }
    // (idle: the server ends)
    let start = Instant::now();
    while server.try_wait().unwrap().is_none() {
        assert!(start.elapsed() < Duration::from_secs(10), "the server stayed");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(server.wait().unwrap().success());
    std::fs::remove_dir_all(&d).unwrap();
}
