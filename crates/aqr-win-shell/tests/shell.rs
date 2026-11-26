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

use aqr_shell::drop::TEST_DROP_HANDLER;
use aqr_shell::folder::{TEST_FOLDER, TEST_SERVER};
use aqr_shell::history::TEST_VERSIONS_PAGE;
use windows::Win32::Foundation::WPARAM;
use windows::Win32::Foundation::{HWND, LPARAM, POINTL, PROPERTYKEY, SIZE};
use windows::Win32::System::Com::IPersistFile;
use windows::Win32::System::Com::{
    CLSCTX_ALL, CoCreateInstance, FORMATETC, IDataObject, IStream, TYMED_HGLOBAL, TYMED_ISTREAM,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::DROPEFFECT;
use windows::Win32::System::Ole::{DROPEFFECT_COPY, IDropTarget};
use windows::Win32::System::Ole::{OleGetClipboard, OleInitialize, OleUninitialize};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RegDeleteTreeW, RegSetKeyValueW};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Controls::{
    HPROPSHEETPAGE, PROPSHEETHEADERW_V2, PROPSHEETHEADERW_V2_2, PSH_MODELESS, PSH_NOAPPLYNOW, PropertySheetW,
};
use windows::Win32::UI::Shell::BHID_SFUIObject;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::Common::SHELLDETAILS;
use windows::Win32::UI::Shell::IPersistFolder;
use windows::Win32::UI::Shell::{
    ASSOCF_NONE, ASSOCSTR_SHELLEXTENSION, CMINVOKECOMMANDINFO, IContextMenu, IQueryAssociations,
    IShellItemImageFactory, IThumbnailProvider, SIIGBF_THUMBNAILONLY,
};
use windows::Win32::UI::Shell::{BHID_DataObject, IShellExtInit, IShellPropSheetExt, SHCreateItemFromRelativeName};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, BHID_SFObject, BHID_Stream, IEnumShellItems, IShellFolder2, IShellItem, IShellItem2, IShellView,
    SHCreateItemFromParsingName, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_NORMALDISPLAY,
};
use windows::Win32::UI::Shell::{CFSTR_FILECONTENTS, CFSTR_FILEDESCRIPTORW, FILEDESCRIPTORW};
use windows::Win32::UI::Shell::{FILEOPERATION_FLAGS, FileOperation, IFileOperation};
use windows::Win32::UI::Shell::{IEnumExplorerCommand, IExplorerCommand, IExplorerCommandProvider};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, DestroyMenu, DestroyWindow, DispatchMessageW, GET_MENU_DEFAULT_ITEM_FLAGS, GetMenuDefaultItem,
    GetMenuItemCount, GetMenuItemID, GetMenuState, GetMenuStringW, GetSubMenu, IsDialogMessageW, MF_BYPOSITION,
    MF_CHECKED, MSG, PM_REMOVE, PeekMessageW, SendDlgItemMessageW, SendMessageW, WM_COMMAND,
};
use windows::core::{GUID, HSTRING, Interface};
use windows::core::{PSTR, PWSTR};

/// The directory of the binaries built (of this test's, `deps` in it).
fn built() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().to_owned();
    for f in ["aqr.exe", "aqr-win.exe"] {
        assert!(
            dir.join(f).is_file(),
            "{}: not built (cargo rt -p aqr -p aqr-win -p aqr-win-shell)",
            dir.join(f).display()
        );
    }
    dir
}

/// A process killed as it is dropped (a test failed: not left running).
struct Killed(std::process::Child);

impl Drop for Killed {
    fn drop(&mut self) {
        _ = self.0.kill();
        _ = self.0.wait();
    }
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
        let attributes: u32 = 0x2000_0000 | 0x40_0000 | 0x100;
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
        // (the Versions tab's class)
        let page = format!(r"CLSID\{}", braced(&TEST_VERSIONS_PAGE));
        set(&format!(r"{page}\InProcServer32"), None, &dll.display().to_string());
        set(&format!(r"{page}\InProcServer32"), Some("ThreadingModel"), "Apartment");
        // (the drop handler's)
        let handler = format!(r"CLSID\{}", braced(&TEST_DROP_HANDLER));
        set(&format!(r"{handler}\InProcServer32"), None, &dll.display().to_string());
        set(&format!(r"{handler}\InProcServer32"), Some("ThreadingModel"), "Apartment");
        Registered(vec![".aqrtest".into(), "Aqueous.aqrtest".into(), class, page, handler])
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
    // (beside big.bin: a file named after it, and a folder)
    std::fs::create_dir_all(d.join("t/sub/big_files")).unwrap();
    std::fs::write(d.join("t/sub/big.bin.srt"), b"srt").unwrap();
    std::fs::write(d.join("t/sub/big_files/p.txt"), b"page").unwrap();
    let o = Command::new(bin.join("aqr.exe")).current_dir(&d).args(["-q", "-o", "t.aqrtest", "t"]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // (a picture of 2x2 pixels, a BMP)
    std::fs::create_dir_all(d.join("img")).unwrap();
    let mut bmp = Vec::new();
    bmp.extend(b"BM");
    bmp.extend(70u32.to_le_bytes());
    bmp.extend([0; 4]);
    bmp.extend(54u32.to_le_bytes());
    bmp.extend(40u32.to_le_bytes());
    bmp.extend(2i32.to_le_bytes());
    bmp.extend(2i32.to_le_bytes());
    bmp.extend(1u16.to_le_bytes());
    bmp.extend(24u16.to_le_bytes());
    bmp.extend([0; 24]);
    bmp.extend([0, 0, 255, 0, 255, 0, 0, 0, 255, 0, 0, 255, 255, 255, 0, 0]);
    std::fs::write(d.join("img/p.bmp"), &bmp).unwrap();
    std::fs::write(d.join("img/n.txt"), b"text").unwrap();
    let mut big_bmp = Vec::new();
    let row = 64 * 3;
    big_bmp.extend(b"BM");
    big_bmp.extend((54 + row * 64u32).to_le_bytes());
    big_bmp.extend([0; 4]);
    big_bmp.extend(54u32.to_le_bytes());
    big_bmp.extend(40u32.to_le_bytes());
    big_bmp.extend(64i32.to_le_bytes());
    big_bmp.extend(64i32.to_le_bytes());
    big_bmp.extend(1u16.to_le_bytes());
    big_bmp.extend(24u16.to_le_bytes());
    big_bmp.extend([0; 24]);
    for y in 0..64u32 {
        for x in 0..64u32 {
            big_bmp.extend([(x * 4) as u8, (y * 4) as u8, 128]);
        }
    }
    std::fs::write(d.join("img/q.bmp"), &big_bmp).unwrap();
    let o =
        Command::new(bin.join("aqr.exe")).current_dir(&d).args(["-q", "-o", "img.aqrtest", "img"]).output().unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // (an archive of two journal points, the second of a file added)
    std::fs::create_dir_all(d.join("j")).unwrap();
    std::fs::write(d.join("j/one.txt"), b"one").unwrap();
    let o = Command::new(bin.join("aqr.exe"))
        .current_dir(&d)
        .args(["-q", "-j", "first", "-o", "j.aqrtest", "j"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    std::fs::write(d.join("two.txt"), b"two").unwrap();
    let o = Command::new(bin.join("aqr.exe"))
        .current_dir(&d)
        .args(["-q", "-a", "-j", "second", "j.aqrtest", "two.txt"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = Command::new(bin.join("aqr.exe"))
        .current_dir(&d)
        .env("AQR_PASS", "sesame")
        .args(["-q", "--passphrase-from=env:AQR_PASS", "-o", "e.aqrtest", "t"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // (the library of this build: `cargo test` writes it in `deps`, beside this test; the one in
    // the directory above only as `cargo build` does, and it may be of another build)
    let exe = std::env::current_exe().unwrap();
    let dll = exe.with_file_name("aqr_shell.dll");
    assert!(dll.is_file(), "{}: not built", dll.display());
    let _registered = Registered::new(&dll);
    // (a class of this run's alone, which the library is told: a server left of a run failed
    // takes nothing of this one)
    let class = GUID::from_u128(TEST_SERVER.to_u128() ^ (std::process::id() as u128) << 64);
    // SAFETY: the test's own environment, before the library reads it
    unsafe { std::env::set_var("AQR_SHELL_TEST_SERVER", braced(&class)) };
    let mut server = Killed(
        Command::new(bin.join("aqr-win.exe"))
            .args(["--serve", &braced(&class)])
            .env("AQR", bin.join("aqr.exe"))
            .env("AQR_SERVE_IDLE", "0.2,1")
            .env("AQR_SERVE_TEST_PASSPHRASE", "sesame")
            // (not this test's output: a server left of a test failed holds it open no longer)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .env("LOCALAPPDATA", d.join("local"))
            .spawn()
            .unwrap(),
    );
    // (once it has registered its class)
    std::thread::sleep(Duration::from_millis(500));
    let archive = d.join("t.aqrtest");
    // (what the library tells: the archive changed, below)
    let log = d.join("shell.log");
    // SAFETY: the test's own environment, before the library reads it
    unsafe { std::env::set_var("AQR_SHELL_LOG", &log) };
    // (what "Open" opens, told rather than run)
    let opened = d.join("opened.txt");
    // SAFETY: as above
    unsafe { std::env::set_var("AQR_SHELL_TEST_OPENED", &opened) };
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
        assert_eq!(
            entries(&sub),
            [
                ("big.bin".to_owned(), false, Some(2_500_000)),
                ("big.bin.srt".to_owned(), false, Some(3)),
                ("big_files".to_owned(), true, None),
                ("deep".to_owned(), true, None)
            ]
        );
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
            // the columns of what -t -v tells (each named; past them, none): of a file, its data
            // and other blocks, its ratio and segment, its flags
            for col in 0..18 {
                assert!(!details(std::ptr::null(), col).is_empty(), "{col}");
            }
            assert!(f.GetDetailsOf(std::ptr::null(), 18, &mut SHELLDETAILS::default()).is_err());
            let verbose: Vec<String> = (4..13).map(|col| details(items[a], col)).collect();
            assert!(verbose[0].contains("KB") && verbose[1].contains("KB"), "{verbose:?}");
            assert!(verbose[2].ends_with('%') && verbose[3] == "1", "{verbose:?}");
            assert_eq!(verbose[8].len(), 7, "{verbose:?}");
            // (and as properties: the size of the data blocks, by its key)
            let data = PROPERTYKEY { fmtid: GUID::from_u128(0xd973fccd_add6_4146_a7d7_5d7c05ef84f0), pid: 2 };
            let v = f.GetDetailsEx(items[a], &data).unwrap();
            assert_eq!(*(&v as *const _ as *const u16), 21, "VT_UI8");
            // (a folder first, by any column)
            for col in 0..4 {
                let n = f.CompareIDs(LPARAM(col), items[a], items[1 - a]);
                assert_eq!(n.0 as i16, 1, "{col}");
            }
            // copied out (as a paste does: the copy engine, of the folder's data object)
            let out = d_out();
            let ids: Vec<*const ITEMIDLIST> = items.iter().map(|&p| p as *const _).collect();
            let data: IDataObject = f.GetUIObjectOf(HWND::default(), &ids, None).unwrap();
            // for other programs: the files' descriptors (of all under the folders chosen too)
            // and their contents by index
            let format = |name| FORMATETC {
                cfFormat: RegisterClipboardFormatW(name) as u16,
                ptd: std::ptr::null_mut(),
                dwAspect: 1,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            };
            let m = data.GetData(&format(CFSTR_FILEDESCRIPTORW)).unwrap();
            let names: Vec<(String, u32, u64)> = {
                let p = GlobalLock(m.u.hGlobal) as *const u8;
                let n = (p as *const u32).read_unaligned() as usize;
                let size = std::mem::size_of::<FILEDESCRIPTORW>();
                let all = (0..n)
                    .map(|k| {
                        let f = (p.add(4 + k * size) as *const FILEDESCRIPTORW).read_unaligned();
                        let name = f.cFileName;
                        let name = String::from_utf16_lossy(&name[..name.iter().position(|&c| c == 0).unwrap()]);
                        (name, f.dwFileAttributes & 0x10, (f.nFileSizeHigh as u64) << 32 | f.nFileSizeLow as u64)
                    })
                    .collect();
                _ = GlobalUnlock(m.u.hGlobal);
                all
            };
            let mut sorted = names.clone();
            sorted.sort();
            let want = [
                ("a.txt", 0, 1000),
                ("sub", 0x10, 0),
                (r"sub\big.bin", 0, 2_500_000),
                (r"sub\big.bin.srt", 0, 3),
                (r"sub\big_files", 0x10, 0),
                (r"sub\big_files\p.txt", 0, 4),
                (r"sub\deep", 0x10, 0),
                (r"sub\deep\c.txt", 0, 4),
            ];
            assert_eq!(sorted, want.map(|(n, a, s)| (n.to_owned(), a, s)));
            let k = names.iter().position(|n| n.0 == r"sub\big.bin").unwrap();
            let mut fc = format(CFSTR_FILECONTENTS);
            (fc.lindex, fc.tymed) = (k as i32, TYMED_ISTREAM.0 as u32);
            let m = data.GetData(&fc).unwrap();
            assert!(read(m.u.pstm.as_ref().unwrap()) == big);
            // dropped on a folder (as dragging there does, and pasting: the drop target of the
            // folder's), copied by what it takes of the data object; perhaps on a thread of its own
            let dropped = d_out().with_file_name("dropped");
            std::fs::create_dir_all(&dropped).unwrap();
            let target: IDropTarget = item(&dropped).BindToHandler(None, &BHID_SFUIObject).unwrap();
            let mut effect = DROPEFFECT_COPY;
            target.DragEnter(&data, MODIFIERKEYS_FLAGS(1), POINTL::default(), &mut effect).unwrap();
            let mut effect = DROPEFFECT_COPY;
            target.Drop(&data, MODIFIERKEYS_FLAGS(1), POINTL::default(), &mut effect).unwrap();
            let start = Instant::now();
            while std::fs::read(dropped.join(r"sub\deep\c.txt")).ok().as_deref() != Some(b"deep") {
                assert!(start.elapsed() < Duration::from_secs(20), "not dropped");
                std::thread::sleep(Duration::from_millis(100));
            }
            assert!(std::fs::read(dropped.join(r"sub\big.bin")).unwrap() == big);
            assert_eq!(std::fs::read(dropped.join("a.txt")).unwrap(), b"some text\n".repeat(100));
            // by the copy engine itself, of the data object (Windows 8 on: Windows 7's takes items
            // of ID lists only of a folder of ITransferSource, and is given the descriptors alone)
            if aqr_shell::windows_8_on() {
                let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL).unwrap();
                // FOF_NO_UI
                op.SetOperationFlags(FILEOPERATION_FLAGS(0x4 | 0x10 | 0x200 | 0x400)).unwrap();
                op.CopyItems(&data, &item(&out)).unwrap();
                op.PerformOperations().unwrap();
                assert!(!op.GetAnyOperationsAborted().unwrap().as_bool());
                assert_eq!(std::fs::read(out.join("a.txt")).unwrap(), b"some text\n".repeat(100));
                assert!(std::fs::read(out.join(r"sub\big.bin")).unwrap() == big);
                assert_eq!(std::fs::read(out.join(r"sub\deep\c.txt")).unwrap(), b"deep");
            }
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
            assert!(verbs.iter().any(|(_, v)| v == "openwithfolder"), "{verbs:?}");
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
            // (the clipboard may be held by another program a moment)
            let clip = (0..50)
                .find_map(|_| {
                    OleGetClipboard().ok().or_else(|| {
                        std::thread::sleep(Duration::from_millis(100));
                        None
                    })
                })
                .expect("the clipboard");
            // (as a paste does: dropped on the folder)
            let target: IDropTarget = item(&pasted).BindToHandler(None, &BHID_SFUIObject).unwrap();
            let mut effect = DROPEFFECT_COPY;
            target.DragEnter(&clip, MODIFIERKEYS_FLAGS(1), POINTL::default(), &mut effect).unwrap();
            let mut effect = DROPEFFECT_COPY;
            target.Drop(&clip, MODIFIERKEYS_FLAGS(1), POINTL::default(), &mut effect).unwrap();
            let start = Instant::now();
            while std::fs::read(pasted.join("a.txt")).ok() != Some(b"some text\n".repeat(100)) {
                assert!(start.elapsed() < Duration::from_secs(20), "not pasted");
                std::thread::sleep(Duration::from_millis(100));
            }
            items.into_iter().for_each(|p| windows::Win32::UI::Shell::ILFree(Some(p)));
        }
        // "Open with the files beside it": the file and those of its folder (and under it)
        // written out, the file opened
        // SAFETY: COM calls of the shell's objects
        unsafe {
            let sub: IShellFolder2 = item(&archive.join("sub")).BindToHandler(None, &BHID_SFObject).unwrap();
            let mut got = [std::ptr::null_mut()];
            sub.ParseDisplayName(
                HWND::default(),
                None,
                windows::core::w!("big.bin"),
                None,
                &mut got[0],
                std::ptr::null_mut(),
            )
            .unwrap();
            let m: IContextMenu = sub.GetUIObjectOf(HWND::default(), &[got[0] as *const _], None).unwrap();
            let menu = CreatePopupMenu().unwrap();
            assert!(m.QueryContextMenu(menu, 0, 1, 0x7fff, 0).is_ok());
            let info = CMINVOKECOMMANDINFO {
                cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
                lpVerb: windows::core::s!("openwithfolder"),
                nShow: 1,
                ..Default::default()
            };
            m.InvokeCommand(&info).unwrap();
            // (and its properties: all that -t -v tells, its formula too; logged by the library
            // of the tests rather than shown)
            let info = CMINVOKECOMMANDINFO {
                cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
                lpVerb: windows::core::s!("properties"),
                nShow: 1,
                ..Default::default()
            };
            m.InvokeCommand(&info).unwrap();
            let listed = Command::new(bin.join("aqr.exe")).args(["-t", "--json"]).arg(&archive).output().unwrap();
            let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
            let formula = listed["entries"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["path"] == "sub/big.bin")
                .and_then(|e| e["formula"].as_str())
                .unwrap()
                .to_owned();
            let start = Instant::now();
            let props = loop {
                let t = std::fs::read_to_string(&log).unwrap_or_default();
                if let Some(k) = t.find("properties of big.bin:") {
                    break t[k..].to_owned();
                }
                assert!(start.elapsed() < Duration::from_secs(10), "no properties");
                std::thread::sleep(Duration::from_millis(50));
            };
            assert!(props.contains(&formula) && props.contains("2500000"), "{props}");
            DestroyMenu(menu).unwrap();
            windows::Win32::UI::Shell::ILFree(Some(got[0]));
            let start = Instant::now();
            let told = loop {
                if let Ok(t) = std::fs::read_to_string(&opened) {
                    break t;
                }
                assert!(start.elapsed() < Duration::from_secs(10), "nothing opened");
                std::thread::sleep(Duration::from_millis(50));
            };
            let file = PathBuf::from(told.trim());
            assert!(file.starts_with(d.join(r"local\Aqueous\Open")) && file.ends_with(r"sub\big.bin"), "{told}");
            assert!(std::fs::read(&file).unwrap() == big);
            // (beside it, and in its folder named after it; not the other folder, nor above)
            assert_eq!(std::fs::read(file.with_file_name("big.bin.srt")).unwrap(), b"srt");
            assert_eq!(std::fs::read(file.with_file_name(r"big_files\p.txt")).unwrap(), b"page");
            assert!(!file.with_file_name("deep").exists());
            assert!(!file.parent().unwrap().with_file_name("a.txt").exists());
        }
        // encrypted: of the passphrase the server's dialog is given (the test's)
        let e = item(&d.join("e.aqrtest"));
        assert_eq!(entries(&e), [("a.txt".to_owned(), false, Some(1000)), ("sub".to_owned(), true, None)]);
        // SAFETY: COM calls of the shell's item
        let s: IStream = unsafe { item(&d.join(r"e.aqrtest\sub\big.bin")).BindToHandler(None, &BHID_Stream).unwrap() };
        assert!(read(&s) == big);
        drop((e, s));
        // the archive changed (its time, here: a file mapped is not replaced): opened anew, as
        // the folder is enumerated again
        // SAFETY: COM calls of the shell's objects
        unsafe {
            // (a folder kept, as a window's view keeps it)
            let kept: IShellFolder2 = root.BindToHandler(None, &BHID_SFObject).unwrap();
            let count = |f: &IShellFolder2| {
                let mut e = None;
                assert!(f.EnumObjects(HWND::default(), 0x20 | 0x40, &mut e).is_ok());
                let (e, mut n) = (e.unwrap(), 0);
                loop {
                    let mut got = [std::ptr::null_mut()];
                    if e.Next(&mut got, None).is_err() || got[0].is_null() {
                        return n;
                    }
                    windows::Win32::UI::Shell::ILFree(Some(got[0]));
                    n += 1;
                }
            };
            assert_eq!(count(&kept), 2);
            let f = std::fs::File::options().write(true).open(&archive).unwrap();
            f.set_modified(std::time::SystemTime::now() + Duration::from_secs(5)).unwrap();
            drop(f);
            assert_eq!(count(&kept), 2);
            let told = std::fs::read_to_string(&log).unwrap_or_default();
            // (changed again while the folder is kept, not enumerated: watched, let go of)
            let f = std::fs::File::options().write(true).open(&archive).unwrap();
            f.set_modified(std::time::SystemTime::now() + Duration::from_secs(9)).unwrap();
            drop(f);
            let start = Instant::now();
            while !std::fs::read_to_string(&log).unwrap_or_default().contains("watched: ") {
                assert!(start.elapsed() < Duration::from_secs(10), "not watched");
                std::thread::sleep(Duration::from_millis(100));
            }
            assert_eq!(count(&kept), 2);
            assert!(
                told.contains("changed: "),
                "{}",
                told.lines().filter(|l| !l.contains("GetDetailsEx")).collect::<Vec<_>>().join(
                    "
"
                )
            );
        }
        // an archive first listed by a window's thread, which then ends (the window closed),
        // while a folder of it is kept by another: listed again by that one (what the thread
        // gone held, gone with its apartment: E_INVALIDARG once)
        std::fs::copy(&archive, d.join("u.aqrtest")).unwrap();
        let u = d.join("u.aqrtest");
        // SAFETY: COM calls of the shell's objects
        unsafe {
            let kept: IShellFolder2 = item(&u).BindToHandler(None, &BHID_SFObject).unwrap();
            let u2 = u.clone();
            std::thread::spawn(move || {
                OleInitialize(None).unwrap();
                {
                    let f: IShellFolder2 = item(&u2).BindToHandler(None, &BHID_SFObject).unwrap();
                    let mut e = None;
                    assert!(f.EnumObjects(HWND::default(), 0x20 | 0x40, &mut e).is_ok());
                }
                OleUninitialize();
            })
            .join()
            .unwrap();
            let mut e = None;
            let hr = kept.EnumObjects(HWND::default(), 0x20 | 0x40, &mut e);
            assert!(hr.is_ok(), "{hr:?}");
            let mut got = [std::ptr::null_mut()];
            assert!(e.unwrap().Next(&mut got, None).is_ok() && !got[0].is_null());
            windows::Win32::UI::Shell::ILFree(Some(got[0]));
        }
        // a picture's thumbnail, by its type's provider; and the associations of its type (which
        // the preview pane finds its handler by)
        // SAFETY: COM calls of the shell's objects
        unsafe {
            let f: IShellFolder2 = item(&d.join("img.aqrtest")).BindToHandler(None, &BHID_SFObject).unwrap();
            let mut p = [std::ptr::null_mut()];
            f.ParseDisplayName(
                HWND::default(),
                None,
                windows::core::w!("p.bmp"),
                None,
                &mut p[0],
                std::ptr::null_mut(),
            )
            .unwrap();
            let t: IThumbnailProvider = f.GetUIObjectOf(HWND::default(), &[p[0] as *const _], None).unwrap();
            let (mut bmp, mut alpha) = (Default::default(), Default::default());
            t.GetThumbnail(32, &mut bmp, &mut alpha).unwrap();
            assert!(!bmp.is_invalid());
            _ = windows::Win32::Graphics::Gdi::DeleteObject(bmp.into());
            // (text's preview handler, which every Windows has; pictures are previewed by
            // Explorer itself)
            let mut n = [std::ptr::null_mut()];
            f.ParseDisplayName(
                HWND::default(),
                None,
                windows::core::w!("n.txt"),
                None,
                &mut n[0],
                std::ptr::null_mut(),
            )
            .unwrap();
            let a: IQueryAssociations = f.GetUIObjectOf(HWND::default(), &[n[0] as *const _], None).unwrap();
            let mut buf = [0u16; 64];
            let mut len = buf.len() as u32;
            let preview = windows::core::w!("{8895b1c6-b41f-4c1c-a562-0d564250836f}");
            a.GetString(ASSOCF_NONE, ASSOCSTR_SHELLEXTENSION, preview, Some(PWSTR(buf.as_mut_ptr())), &mut len)
                .unwrap();
            windows::Win32::UI::Shell::ILFree(Some(n[0]));
            windows::Win32::UI::Shell::ILFree(Some(p[0]));
        }
        // as Explorer asks for a picture of an item, at each size of its views: the item's image
        // factory (the thumbnail cache, the handler bound as it binds it)
        // SAFETY: COM calls of the shell's item
        unsafe {
            let q = item(&d.join(r"img.aqrtest\q.bmp"));
            let f: IShellItemImageFactory = q.cast().unwrap();
            let got: Vec<(i32, bool)> = [16, 32, 48, 96, 256]
                .into_iter()
                .map(|n| {
                    let r = f.GetImage(SIZE { cx: n, cy: n }, SIIGBF_THUMBNAILONLY);
                    if let Ok(b) = &r {
                        _ = windows::Win32::Graphics::Gdi::DeleteObject((*b).into());
                    }
                    (n, r.is_ok())
                })
                .collect();
            let told = std::fs::read_to_string(&log).unwrap_or_default();
            let asked: Vec<&str> = told.lines().filter(|l| l.contains("asked")).collect();
            assert!(got.iter().all(|g| g.1), "{got:?}\n{}", asked.join("\n"));
        }
        // the menu of the view's background: "Versions", the journal points newest first after
        // "Latest", the one shown checked; one chosen browsed to (told, in tests), the archive as
        // of it of the files there then
        let j = d.join("j.aqrtest");
        // (the menu of the folder at `path`: its choices' texts and checks, and the place its
        // choice of offset `k` browses to)
        let versions = |path: &Path, k: Option<usize>| -> (Vec<(String, bool)>, Option<String>) {
            // SAFETY: COM calls of the shell's objects
            unsafe {
                let f: IShellFolder2 = item(path).BindToHandler(None, &BHID_SFObject).unwrap();
                let m: IContextMenu = f.CreateViewObject(HWND::default()).unwrap();
                let menu = CreatePopupMenu().unwrap();
                let hr = m.QueryContextMenu(menu, 0, 1, 0x7fff, 0);
                // ("Paste", of all of the archive; "Latest" and the two points)
                assert_eq!(hr.0, 4, "{hr:?}");
                let sub = GetSubMenu(menu, GetMenuItemCount(Some(menu)) - 1);
                let shown = (0..GetMenuItemCount(Some(sub)))
                    .map(|i| {
                        let mut b = [0u16; 128];
                        let n = GetMenuStringW(sub, i as u32, Some(&mut b), MF_BYPOSITION);
                        let checked = GetMenuState(sub, i as u32, MF_BYPOSITION) & MF_CHECKED.0 != 0;
                        (String::from_utf16_lossy(&b[..n as usize]), checked)
                    })
                    .collect();
                DestroyMenu(menu).unwrap();
                let browsed = k.map(|k| {
                    _ = std::fs::remove_file(&opened);
                    let info = CMINVOKECOMMANDINFO {
                        cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
                        lpVerb: windows::core::PCSTR(k as *const u8),
                        nShow: 1,
                        ..Default::default()
                    };
                    m.InvokeCommand(&info).unwrap();
                    std::fs::read_to_string(&opened).unwrap().trim().to_owned()
                });
                (shown, browsed)
            }
        };
        let (shown, browsed) = versions(&j, Some(2));
        let texts: Vec<&str> = shown.iter().map(|s| s.0.as_str()).collect();
        assert_eq!(texts.len(), 4, "{shown:?}");
        assert!(texts[2].starts_with("second (") && texts[3].starts_with("first ("), "{shown:?}");
        assert_eq!(shown.iter().map(|s| s.1).collect::<Vec<_>>(), [true, false, false, false]);
        let browsed = browsed.unwrap();
        let at = browsed.strip_prefix("browse ").unwrap().to_owned();
        let at = at.as_str();
        assert!(at.starts_with(&format!(r"{}\@", j.display())), "{browsed}");
        assert_eq!(entries(&item(&j)).len(), 2);
        assert_eq!(entries(&item(Path::new(at))), [("one.txt".to_owned(), false, Some(3))]);
        // SAFETY: COM calls of the shell's items
        unsafe {
            let v = item(Path::new(at));
            let n = v.GetDisplayName(SIGDN_NORMALDISPLAY).unwrap();
            assert!(n.to_string().unwrap().starts_with("first ("));
            windows::Win32::System::Com::CoTaskMemFree(Some(n.0 as *const _));
            let s: IStream = item(&Path::new(at).join("one.txt")).BindToHandler(None, &BHID_Stream).unwrap();
            assert_eq!(read(&s), b"one");
        }
        // (of the point: it checked; "Latest" browses back to all of the archive)
        let (shown, browsed) = versions(Path::new(at), Some(1));
        let latest = shown[0].0.clone();
        assert_eq!(shown.iter().map(|s| s.1).collect::<Vec<_>>(), [false, false, false, true]);
        assert_eq!(browsed.unwrap(), format!("browse {}", j.display()));
        // the view's commands (Explorer's command bar): "Versions", its subcommands as the menu's
        // SAFETY: COM calls of the shell's objects
        unsafe {
            let title = |c: &IExplorerCommand| {
                let t = c.GetTitle(None).unwrap();
                let s = t.to_string().unwrap();
                windows::Win32::System::Com::CoTaskMemFree(Some(t.0 as *const _));
                s
            };
            let f: IShellFolder2 = item(&j).BindToHandler(None, &BHID_SFObject).unwrap();
            let p: IExplorerCommandProvider = f.CreateViewObject(HWND::default()).unwrap();
            let e: IEnumExplorerCommand = p.GetCommands(None).unwrap();
            let mut got = [None];
            let mut n = 0;
            assert!(e.Next(&mut got, Some(&mut n)).is_ok() && n == 1);
            let v = got[0].take().unwrap();
            // (its titles in the user's language: "Latest" as the menu's)
            assert!(!title(&v).is_empty());
            assert_eq!((v.GetFlags().unwrap() & 1, v.GetState(None, true).unwrap()), (1, 0));
            let subs = v.EnumSubCommands().unwrap();
            let mut all = Vec::new();
            loop {
                let mut got = [None];
                if subs.Next(&mut got, None) != windows::Win32::Foundation::S_OK {
                    break;
                }
                all.push(got[0].take().unwrap());
            }
            let titles: Vec<String> = all.iter().map(title).collect();
            assert!(titles.len() == 3 && titles[0] == latest && titles[2].starts_with("first ("), "{titles:?}");
            let states: Vec<u32> = all.iter().map(|c| c.GetState(None, true).unwrap()).collect();
            assert_eq!(states, [0x18, 0, 0]);
            _ = std::fs::remove_file(&opened);
            all[2].Invoke(None, None).unwrap();
            assert_eq!(std::fs::read_to_string(&opened).unwrap().trim(), format!("browse {at}"));
        }
        // files dropped: on the view (its folder), on a folder in it, on the archive file (its
        // type's drop handler), and pasted; added by aqr-win (told), always copied, asked first
        // where a name is there already; nothing of the archive as of a journal point
        std::fs::write(d.join("new.txt"), b"new").unwrap();
        std::fs::write(d.join("a.txt"), b"other").unwrap();
        // SAFETY: COM calls of the shell's objects
        unsafe {
            let data =
                |name: &str| -> IDataObject { item(&d.join(name)).BindToHandler(None, &BHID_DataObject).unwrap() };
            let dropped = |t: &IDropTarget, name: &str| -> (u32, String) {
                _ = std::fs::remove_file(&opened);
                let data = data(name);
                let mut effect = DROPEFFECT(DROPEFFECT_COPY.0 | 2); // DROPEFFECT_MOVE
                t.DragEnter(&data, MODIFIERKEYS_FLAGS(0), POINTL::default(), &mut effect).unwrap();
                assert_eq!(effect, DROPEFFECT_COPY);
                let mut effect = DROPEFFECT(DROPEFFECT_COPY.0 | 2);
                t.Drop(&data, MODIFIERKEYS_FLAGS(0), POINTL::default(), &mut effect).unwrap();
                (effect.0, std::fs::read_to_string(&opened).unwrap_or_default().trim().to_owned())
            };
            let new = d.join("new.txt");
            let f: IShellFolder2 = item(&archive).BindToHandler(None, &BHID_SFObject).unwrap();
            let view: IDropTarget = f.CreateViewObject(HWND::default()).unwrap();
            assert_eq!(dropped(&view, "new.txt"), (1, format!("run add {}  {}", archive.display(), new.display())));
            let mut sub = [std::ptr::null_mut()];
            f.ParseDisplayName(
                HWND::default(),
                None,
                windows::core::w!("sub"),
                None,
                &mut sub[0],
                std::ptr::null_mut(),
            )
            .unwrap();
            let on_sub: IDropTarget = f.GetUIObjectOf(HWND::default(), &[sub[0] as *const _], None).unwrap();
            windows::Win32::UI::Shell::ILFree(Some(sub[0]));
            assert_eq!(dropped(&on_sub, "new.txt").1, format!("run add {} sub {}", archive.display(), new.display()));
            // (a.txt is there: asked, logged in tests)
            let (_, told) = dropped(&view, "a.txt");
            assert_eq!(told, format!("run add {}  {}", archive.display(), d.join("a.txt").display()));
            assert!(std::fs::read_to_string(&log).unwrap().contains("drop: asked:"));
            let h: IPersistFile = CoCreateInstance(&TEST_DROP_HANDLER, None, CLSCTX_ALL).unwrap();
            h.Load(&HSTRING::from(archive.as_os_str()), Default::default()).unwrap();
            let on_file: IDropTarget = h.cast().unwrap();
            assert_eq!(dropped(&on_file, "new.txt").1, format!("run add {}  {}", archive.display(), new.display()));
            // (of a journal point's: none)
            let v: IShellFolder2 = item(Path::new(at)).BindToHandler(None, &BHID_SFObject).unwrap();
            assert!(v.CreateViewObject::<IDropTarget>(HWND::default()).is_err());
            // pasted: the background's "paste", as Ctrl+V invokes it
            _ = std::fs::remove_file(&opened);
            windows::Win32::System::Ole::OleSetClipboard(&data("new.txt")).unwrap();
            let m: IContextMenu = f.CreateViewObject(HWND::default()).unwrap();
            let menu = CreatePopupMenu().unwrap();
            assert!(m.QueryContextMenu(menu, 0, 1, 0x7fff, 0).is_ok());
            DestroyMenu(menu).unwrap();
            let info = CMINVOKECOMMANDINFO {
                cbSize: std::mem::size_of::<CMINVOKECOMMANDINFO>() as u32,
                lpVerb: windows::core::s!("paste"),
                nShow: 1,
                ..Default::default()
            };
            m.InvokeCommand(&info).unwrap();
            _ = windows::Win32::System::Ole::OleSetClipboard(None);
            let told = std::fs::read_to_string(&opened).unwrap();
            assert_eq!(told.trim(), format!("run add {}  {}", archive.display(), new.display()));
        }
        // archives of other formats: the folder of a ZIP's and a tar.gz's file (as Explorer makes
        // it where Aqueous opens the type: not registered here, not to take `.zip` of the user's),
        // their entries and a file read, only the columns their formats have, nothing added
        for (name, columns) in [("o.zip", 10), ("o.tar.gz", 9)] {
            let o = Command::new(bin.join("aqr.exe")).current_dir(&d).args(["-q", "-o", name, "t"]).output().unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            // SAFETY: COM calls of the shell's objects
            unsafe {
                let p: IPersistFolder = CoCreateInstance(&TEST_FOLDER, None, CLSCTX_ALL).unwrap();
                let at = windows::Win32::UI::Shell::ILCreateFromPathW(&HSTRING::from(d.join(name).as_os_str()));
                p.Initialize(at).unwrap();
                windows::Win32::UI::Shell::ILFree(Some(at));
                let f: IShellFolder2 = p.cast().unwrap();
                let n = (0..40).take_while(|&k| {
                    let mut sd = SHELLDETAILS::default();
                    f.GetDetailsOf(std::ptr::null(), k, &mut sd).is_ok()
                });
                assert_eq!(n.count(), columns, "{name}");
                let mut e = None;
                assert!(f.EnumObjects(HWND::default(), 0x20 | 0x40, &mut e).is_ok());
                let mut got = [std::ptr::null_mut()];
                let mut fetched = 0;
                assert!(e.unwrap().Next(&mut got, Some(&mut fetched)).is_ok() && fetched == 1);
                windows::Win32::UI::Shell::ILFree(Some(got[0]));
                let mut a = [std::ptr::null_mut()];
                f.ParseDisplayName(
                    HWND::default(),
                    None,
                    windows::core::w!("t/a.txt"),
                    None,
                    &mut a[0],
                    std::ptr::null_mut(),
                )
                .unwrap();
                let st: IStream = f.BindToObject(a[0], None).unwrap();
                assert_eq!(read(&st), b"some text\n".repeat(100), "{name}");
                windows::Win32::UI::Shell::ILFree(Some(a[0]));
                assert!(f.CreateViewObject::<IDropTarget>(HWND::default()).is_err(), "{name}");
            }
        }
        // the Versions tab of the archive file's Properties: its points newest first, read as it
        // is shown; Open (told: the archive as of the point), Extract (aqr-win's, told) and Name
        // (aqr-win's, told, the list read again after)
        // SAFETY: COM calls of the shell's objects, the sheet's window of this thread
        unsafe {
            let init: IShellExtInit = CoCreateInstance(&TEST_VERSIONS_PAGE, None, CLSCTX_ALL).unwrap();
            let parent = item(&d);
            let file: IShellItem = SHCreateItemFromRelativeName(&parent, &HSTRING::from("j.aqrtest"), None).unwrap();
            let data: IDataObject = file.BindToHandler(None, &BHID_DataObject).unwrap();
            init.Initialize(None, &data, Default::default()).unwrap();
            let ext: IShellPropSheetExt = init.cast().unwrap();
            let mut pages: Vec<HPROPSHEETPAGE> = Vec::new();
            unsafe extern "system" fn add(p: HPROPSHEETPAGE, l: LPARAM) -> windows::core::BOOL {
                // SAFETY: the test's list
                unsafe { (*(l.0 as *mut Vec<HPROPSHEETPAGE>)).push(p) };
                true.into()
            }
            ext.AddPages(Some(add), LPARAM(&mut pages as *mut _ as isize)).unwrap();
            assert_eq!(pages.len(), 1);
            let mut h = PROPSHEETHEADERW_V2 {
                dwSize: std::mem::size_of::<PROPSHEETHEADERW_V2>() as u32,
                dwFlags: PSH_MODELESS | PSH_NOAPPLYNOW,
                nPages: 1,
                Anonymous3: PROPSHEETHEADERW_V2_2 { phpage: pages.as_mut_ptr() },
                ..Default::default()
            };
            let sheet = HWND(PropertySheetW(&mut h) as *mut _);
            assert!(!sheet.0.is_null());
            let page = HWND(SendMessageW(sheet, 0x0400 + 118, None, None).0 as *mut _); // PSM_GETCURRENTPAGEHWND
            let rows = || -> Vec<String> {
                let n = SendDlgItemMessageW(page, 100, 0x018B, WPARAM(0), LPARAM(0)).0; // LB_GETCOUNT
                (0..n)
                    .map(|k| {
                        let mut b = [0u16; 256];
                        let m =
                            SendDlgItemMessageW(page, 100, 0x0189, WPARAM(k as usize), LPARAM(b.as_mut_ptr() as isize))
                                .0; // LB_GETTEXT
                        String::from_utf16_lossy(&b[..m as usize])
                    })
                    .collect()
            };
            let got = rows();
            assert!(got.len() == 2 && got[0].starts_with("#2 ") && got[0].ends_with("second"), "{got:?}");
            assert!(got[1].starts_with("#1 ") && got[1].ends_with("first"), "{got:?}");
            _ = std::fs::remove_file(&opened);
            // (the second chosen, the first row: then the first, by a double-click)
            SendMessageW(page, WM_COMMAND, Some(WPARAM(101)), None);
            SendDlgItemMessageW(page, 100, 0x0186, WPARAM(1), LPARAM(0)); // LB_SETCURSEL
            SendMessageW(page, WM_COMMAND, Some(WPARAM(100 | 2 << 16)), None); // LBN_DBLCLK
            SendMessageW(page, WM_COMMAND, Some(WPARAM(102)), None);
            SendMessageW(page, WM_COMMAND, Some(WPARAM(103)), None);
            let told: Vec<String> = std::fs::read_to_string(&opened).unwrap().lines().map(str::to_owned).collect();
            assert_eq!(told.len(), 4, "{told:?}");
            assert!(told[0].starts_with(&format!("open {}\\@", j.display())) && told[0] != told[1], "{told:?}");
            assert_eq!(told[1], format!("open {at}"), "{told:?}");
            assert_eq!(told[2], format!("run extract-version {} #1", j.display()));
            assert_eq!(told[3], format!("run name-version {}", j.display()));
            // (named: the list read again, as the page is told)
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if !IsDialogMessageW(sheet, &msg).as_bool() {
                    DispatchMessageW(&msg);
                }
            }
            assert_eq!(rows().len(), 2);
            DestroyWindow(sheet).unwrap();
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
    while server.0.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(10) {
            _ = server.0.kill();
            panic!("the server stayed: an object of it not let go of");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // (a file may be held a moment longer, by the system's thumbnails or indexing: tried a while,
    // then left to the temporary directory)
    for _ in 0..50 {
        if std::fs::remove_dir_all(&d).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
