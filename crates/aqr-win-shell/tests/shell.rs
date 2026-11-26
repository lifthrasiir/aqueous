//! The folders of archives as the shell browses them, in this process: `.aqrtest` files (a
//! class of the tests', [`TEST_FOLDER`], registered for the user while the test runs) parsed by
//! path, their entries enumerated as shell items with their sizes, folders in them, and a file
//! read through `BHID_Stream` (as a program given a shell item by a file dialog reads it). The
//! server is `aqr-win --serve` of [`TEST_SERVER`], started here.
//!
//! It needs aqr.exe and aqr-win.exe built beside the library (`cargo rt -p aqr -p aqr-win -p
//! aqr-win-shell --test shell -- --ignored`), and writes keys under `HKCU\Software\Classes`
//! (`.aqrtest`, `Aqueous.aqrtest`, the class), taken away after.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use aqr_shell::folder::{TEST_FOLDER, TEST_SERVER};
use windows::Win32::Foundation::{HWND, LPARAM, PROPERTYKEY};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, IDataObject, IStream};
use windows::Win32::System::Ole::{OleGetClipboard, OleInitialize, OleUninitialize};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RegDeleteTreeW, RegSetKeyValueW};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::Common::SHELLDETAILS;
use windows::Win32::UI::Shell::{
    BHID_EnumItems, BHID_SFObject, BHID_Stream, IEnumShellItems, IShellFolder2, IShellItem, IShellItem2, IShellView,
    SHCreateItemFromParsingName, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_NORMALDISPLAY,
};
use windows::Win32::UI::Shell::{CMINVOKECOMMANDINFO, IContextMenu};
use windows::Win32::UI::Shell::{FILEOPERATION_FLAGS, FileOperation, IFileOperation};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, DestroyMenu, GET_MENU_DEFAULT_ITEM_FLAGS, GetMenuDefaultItem, GetMenuItemCount, GetMenuItemID,
};
use windows::core::PSTR;
use windows::core::{GUID, HSTRING, Interface};

/// The directory of the binaries built (of this test's, `deps` in it).
fn built() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().to_owned();
    for f in ["aqr.exe", "aqr-win.exe", "aqr_shell.dll"] {
        assert!(
            dir.join(f).is_file(),
            "{}: not built (cargo rt -p aqr -p aqr-win -p aqr-win-shell)",
            dir.join(f).display()
        );
    }
    dir
}

fn braced(g: &GUID) -> String {
    format!("{{{g:?}}}")
}

fn set(key: &str, name: Option<&str>, value: &str) {
    let w: Vec<u16> = value.encode_utf16().chain([0]).collect();
    let name = name.map(HSTRING::from);
    // SAFETY: a key of the user's, and a string ended by 0
    let r = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(format!(r"Software\Classes\{key}")),
            name.as_ref().map_or(windows::core::PCWSTR::null(), |n| windows::core::PCWSTR(n.as_ptr())),
            REG_SZ.0,
            Some(w.as_ptr() as *const _),
            (w.len() * 2) as u32,
        )
    };
    assert!(r.is_ok(), "{key}: {r:?}");
}

/// The keys of the test's class, taken away as it is dropped.
struct Registered(Vec<String>);

impl Registered {
    fn new(dll: &Path) -> Registered {
        let class = format!(r"CLSID\{}", braced(&TEST_FOLDER));
        set(".aqrtest", None, "Aqueous.aqrtest");
        set(r"Aqueous.aqrtest\CLSID", None, &braced(&TEST_FOLDER));
        set(&class, None, "Aqueous test archive");
        set(&format!(r"{class}\InProcServer32"), None, &dll.display().to_string());
        set(&format!(r"{class}\InProcServer32"), Some("ThreadingModel"), "Apartment");
        // (CATID_BrowsableShellExt)
        set(&format!(r"{class}\Implemented Categories\{{00021490-0000-0000-C000-000000000046}}"), None, "");
        let attributes: u32 = 0x2000_0000 | 0x40_0000;
        // SAFETY: a key of the user's, and a DWORD
        let r = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(format!(r"Software\Classes\{class}\ShellFolder")),
                &HSTRING::from("Attributes"),
                REG_DWORD.0,
                Some(&attributes as *const u32 as *const _),
                4,
            )
        };
        assert!(r.is_ok());
        Registered(vec![".aqrtest".into(), "Aqueous.aqrtest".into(), class])
    }
}

impl Drop for Registered {
    fn drop(&mut self) {
        for k in &self.0 {
            // SAFETY: a key of the user's
            _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(format!(r"Software\Classes\{k}"))) };
        }
    }
}

fn item(path: &Path) -> IShellItem {
    // SAFETY: a path, parsed by the shell
    unsafe { SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None) }
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The entries of folder `f`: their names, whether folders, and sizes.
fn entries(f: &IShellItem) -> Vec<(String, bool, Option<u64>)> {
    const SIZE: PROPERTYKEY = PROPERTYKEY { fmtid: GUID::from_u128(0xb725f130_47ef_101a_a5f1_02608c9eebac), pid: 12 };
    let mut out = Vec::new();
    // SAFETY: COM calls of the shell's items
    unsafe {
        let e: IEnumShellItems = f.BindToHandler(None, &BHID_EnumItems).unwrap();
        loop {
            let mut got = [None];
            let mut n = 0;
            if e.Next(&mut got, Some(&mut n)).is_err() || n == 0 {
                break;
            }
            let i = got[0].take().unwrap();
            let name = i.GetDisplayName(SIGDN_NORMALDISPLAY).unwrap();
            let folder = i.GetAttributes(windows::Win32::System::SystemServices::SFGAO_FOLDER).unwrap().0 != 0;
            let size = i.cast::<IShellItem2>().unwrap().GetUInt64(&SIZE).ok();
            out.push((name.to_string().unwrap(), folder, size));
            windows::Win32::System::Com::CoTaskMemFree(Some(name.0 as *const _));
        }
    }
    out.sort();
    out
}

fn read(s: &IStream) -> Vec<u8> {
    let (mut all, mut buf) = (Vec::new(), vec![0u8; 100_000]);
    loop {
        let mut n = 0;
        // SAFETY: a buffer of its length
        let hr = unsafe { s.Read(buf.as_mut_ptr() as *mut _, buf.len() as u32, Some(&mut n)) };
        assert!(hr.is_ok(), "{hr:?}");
        if n == 0 {
            return all;
        }
        all.extend_from_slice(&buf[..n as usize]);
    }
}

#[test]
#[ignore = "needs aqr.exe and aqr-win.exe beside the library, and registers a class for the user"]
fn browsed() {
    let bin = built();
    let d = std::env::temp_dir().join(format!("aqr-shell-{}", std::process::id()));
    _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("t/sub/deep")).unwrap();
    let big: Vec<u8> = (0..2_500_000u32).map(|i| (i * 13 % 251) as u8).collect();
    std::fs::write(d.join("t/a.txt"), b"some text\n".repeat(100)).unwrap();
    std::fs::write(d.join("t/sub/big.bin"), &big).unwrap();
    std::fs::write(d.join("t/sub/deep/c.txt"), b"deep").unwrap();
    let o = Command::new(bin.join("aqr.exe")).current_dir(&d).args(["-q", "-o", "t.aqrtest", "t"]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let _registered = Registered::new(&bin.join("aqr_shell.dll"));
    let mut server = Command::new(bin.join("aqr-win.exe"))
        .args(["--serve", &braced(&TEST_SERVER)])
        .env("AQR", bin.join("aqr.exe"))
        .env("AQR_SERVE_IDLE", "0.2,1")
        .spawn()
        .unwrap();
    // (once it has registered its class)
    std::thread::sleep(Duration::from_millis(500));
    let archive = d.join("t.aqrtest");
    let d_out = || {
        let o = d.join("out");
        std::fs::create_dir_all(&o).unwrap();
        o
    };
    // SAFETY: this thread's COM
    unsafe { OleInitialize(None).unwrap() };
    {
        let root = item(&archive);
        assert_eq!(entries(&root), [("a.txt".to_owned(), false, Some(1000)), ("sub".to_owned(), true, None)]);
        let sub = item(&archive.join("sub"));
        assert_eq!(entries(&sub), [("big.bin".to_owned(), false, Some(2_500_000)), ("deep".to_owned(), true, None)]);
        let c = item(&archive.join(r"sub\deep\c.txt"));
        // SAFETY: COM calls of the shell's item
        unsafe {
            let p = c.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING).unwrap();
            assert_eq!(p.to_string().unwrap(), archive.join(r"sub\deep\c.txt").display().to_string());
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
            let s: IStream = c.BindToHandler(None, &BHID_Stream).unwrap();
            assert_eq!(read(&s), b"deep");
            let s: IStream = item(&archive.join(r"sub\big.bin")).BindToHandler(None, &BHID_Stream).unwrap();
            assert!(read(&s) == big);
        }
        // the view (DefView), and the columns
        // SAFETY: COM calls of the shell's objects
        unsafe {
            let f: IShellFolder2 = root.BindToHandler(None, &BHID_SFObject).unwrap();
            let v: IShellView = f.CreateViewObject(HWND::default()).unwrap();
            drop(v);
            let mut items = Vec::new();
            let mut e = None;
            assert!(f.EnumObjects(HWND::default(), 0x20 | 0x40, &mut e).is_ok());
            let e = e.unwrap();
            loop {
                let mut got = [std::ptr::null_mut()];
                let mut n = 0;
                if e.Next(&mut got, Some(&mut n)).is_err() || n == 0 {
                    break;
                }
                items.push(got[0]);
            }
            assert_eq!(items.len(), 2);
            let mut d = SHELLDETAILS::default();
            let mut details = |item, col| {
                f.GetDetailsOf(item, col, &mut d).unwrap();
                // (SHELLDETAILS is packed)
                let p = std::ptr::addr_of!(d.str).read_unaligned().Anonymous.pOleStr;
                let s = p.to_string().unwrap();
                windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
                s
            };
            let a = if details(items[0], 0) == "a.txt" { 0 } else { 1 };
            let shown: Vec<String> = (0..4).map(|col| details(items[a], col)).collect();
            assert_eq!(shown[0], "a.txt");
            // (a date and a time; 1000 bytes in KB, of the user's locale)
            assert!(shown[1].chars().filter(char::is_ascii_digit).count() >= 8, "{shown:?}");
            assert!(shown[1].chars().all(|c| !('\u{200e}'..='\u{200f}').contains(&c)), "{shown:?}");
            assert!(shown[3].contains('1') && shown[3].contains("KB"), "{shown:?}");
            // (a folder first, by any column)
            for col in 0..4 {
                let n = f.CompareIDs(LPARAM(col), items[a], items[1 - a]);
                assert_eq!(n.0 as i16, 1, "{col}");
            }
            // copied out (as a paste does: the copy engine, of the folder's data object)
            let out = d_out();
            let ids: Vec<*const ITEMIDLIST> = items.iter().map(|&p| p as *const _).collect();
            let data: IDataObject = f.GetUIObjectOf(HWND::default(), &ids, None).unwrap();
            let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL).unwrap();
            // FOF_NO_UI
            op.SetOperationFlags(FILEOPERATION_FLAGS(0x4 | 0x10 | 0x200 | 0x400)).unwrap();
            op.CopyItems(&data, &item(&out)).unwrap();
            op.PerformOperations().unwrap();
            assert!(!op.GetAnyOperationsAborted().unwrap().as_bool());
            assert_eq!(std::fs::read(out.join("a.txt")).unwrap(), b"some text\n".repeat(100));
            assert!(std::fs::read(out.join(r"sub\big.bin")).unwrap() == big);
            assert_eq!(std::fs::read(out.join(r"sub\deep\c.txt")).unwrap(), b"deep");
            // the menu of a file: "open" ours, the default; "copy" the shell's, which puts it on
            // the clipboard, to be pasted
            let file = [items[a] as *const ITEMIDLIST];
            let m: IContextMenu = f.GetUIObjectOf(HWND::default(), &file, None).unwrap();
            let menu = CreatePopupMenu().unwrap();
            assert!(m.QueryContextMenu(menu, 0, 1, 0x7fff, 0).is_ok());
            let verbs: Vec<(u32, String)> = (0..GetMenuItemCount(Some(menu)))
                .map(|k| GetMenuItemID(menu, k))
                .filter(|&id| id != u32::MAX && id >= 1)
                .filter_map(|id| {
                    let mut b = [0u16; 64];
                    m.GetCommandString((id - 1) as usize, 4, None, PSTR(b.as_mut_ptr() as *mut u8), 64).ok()?; // GCS_VERBW
                    Some((id, String::from_utf16_lossy(&b[..b.iter().position(|&c| c == 0)?])))
                })
                .collect();
            let default = GetMenuDefaultItem(menu, 0, GET_MENU_DEFAULT_ITEM_FLAGS(0));
            assert!(verbs.iter().any(|(id, v)| v == "open" && *id == default), "{verbs:?} {default}");
            assert!(verbs.iter().any(|(_, v)| v == "copy"), "{verbs:?}");
            DestroyMenu(menu).unwrap();
            let copy = windows::core::s!("copy");
            let info = CMINVOKECOMMANDINFO {
                cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
                lpVerb: copy,
                nShow: 1,
                ..Default::default()
            };
            m.InvokeCommand(&info).unwrap();
            let pasted = d_out().with_file_name("pasted");
            std::fs::create_dir_all(&pasted).unwrap();
            let clip = OleGetClipboard().unwrap();
            let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL).unwrap();
            op.SetOperationFlags(FILEOPERATION_FLAGS(0x4 | 0x10 | 0x200 | 0x400)).unwrap();
            op.CopyItems(&clip, &item(&pasted)).unwrap();
            op.PerformOperations().unwrap();
            assert_eq!(std::fs::read(pasted.join("a.txt")).unwrap(), b"some text\n".repeat(100));
            items.into_iter().for_each(|p| windows::Win32::UI::Shell::ILFree(Some(p)));
        }
        // (not there: the shell's parse fails)
        // SAFETY: a path, parsed by the shell
        assert!(
            unsafe {
                SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(archive.join("nope").as_os_str()), None)
            }
            .is_err()
        );
    }
    // SAFETY: initialized above, its objects let go of
    unsafe {
        // (the clipboard's data object, ours, let go of)
        _ = windows::Win32::System::Ole::OleSetClipboard(None);
        OleUninitialize();
    }
    // (its objects let go of: the server ends, idle)
    let start = Instant::now();
    while server.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(10) {
            _ = server.kill();
            panic!("the server stayed: an object of it not let go of");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    std::fs::remove_dir_all(&d).unwrap();
}
