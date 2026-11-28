//! The menu "Aqueous" and "Eject" as Explorer has them: the library loaded (`aqr_shell.dll`, of
//! the classes `more::MENU` and `mount::EJECT_COMMAND`), the commands asked their state of shell
//! items and invoked. The menu's commands of archives are shown of archives alone ("Mount as a
//! drive" disabled unless aqr-mount.exe is beside the library and WinFsp is installed, the
//! versions' of a `.aqr` file), those of compressing of all; Eject of aqr-mount's drives alone, and invoked it
//! ejects (by aqr-win beside the library). Nothing is registered.
//!
//! It needs aqr.exe, aqr-win.exe and aqr-mount.exe built beside the library (`cargo rt -p aqr -p
//! aqr-win -p aqr-mount-win -p aqr-win-shell --test mount`); what needs WinFsp is skipped where
//! it is not installed.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use aqr_shell::more::{Item, MENU};
use aqr_shell::mount::EJECT_COMMAND;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, IClassFactory};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Shell::{
    IExplorerCommand, IShellItem, IShellItemArray, SHCreateItemFromParsingName, SHCreateShellItemArrayFromShellItem,
};
use windows::core::{GUID, HRESULT, HSTRING, Interface, s};

const ENABLED: u32 = 0;
const DISABLED: u32 = 0x1;
const HIDDEN: u32 = 0x2;

fn built() -> PathBuf {
    std::env::current_exe().unwrap().parent().unwrap().parent().unwrap().to_owned()
}

/// The command of `clsid`, of the library built.
fn command(clsid: GUID) -> IExplorerCommand {
    type GetClassObject = unsafe extern "system" fn(*const GUID, *const GUID, *mut *mut core::ffi::c_void) -> HRESULT;
    let dll = built().join("aqr_shell.dll");
    assert!(dll.is_file(), "{}: not built", dll.display());
    // SAFETY: the library built, its entry point of COM's signature, its class factory
    unsafe {
        let m: HMODULE = LoadLibraryW(&HSTRING::from(dll.as_os_str())).unwrap();
        let f: GetClassObject = std::mem::transmute(GetProcAddress(m, s!("DllGetClassObject")).unwrap());
        let mut p = std::ptr::null_mut();
        f(&clsid, &IClassFactory::IID, &mut p).ok().unwrap();
        let factory = IClassFactory::from_raw(p);
        factory.CreateInstance::<_, IExplorerCommand>(None).unwrap()
    }
}

fn items(path: &str) -> IShellItemArray {
    // SAFETY: a path parsed, its item as an array
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path), None).unwrap();
        SHCreateShellItemArrayFromShellItem(&item).unwrap()
    }
}

fn state(c: &IExplorerCommand, path: &str, slow: bool) -> windows::core::Result<u32> {
    // SAFETY: the command and an array of an item
    unsafe { c.GetState(&items(path), slow) }
}

fn winfsp() -> bool {
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    // SAFETY: names ending in NUL
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            windows::core::w!(r"SOFTWARE\WOW6432Node\WinFsp"),
            windows::core::w!("InstallDir"),
            RRF_RT_REG_SZ,
            None,
            None,
            None,
        )
    }
    .is_ok()
}

fn archive(d: &Path) -> PathBuf {
    _ = std::fs::remove_dir_all(d);
    std::fs::create_dir_all(d.join("t")).unwrap();
    std::fs::write(d.join("t/a.txt"), b"alpha").unwrap();
    let aqr = built().join("aqr.exe");
    let o = Command::new(&aqr).current_dir(d).args(["-q", "-o", "t.aqr", "t"]).output().unwrap();
    assert!(o.status.success(), "{}: {}", aqr.display(), String::from_utf8_lossy(&o.stderr));
    d.join("t.aqr")
}

#[test]
fn commands() {
    // SAFETY: COM for this thread, as Explorer's are
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap() };
    let d = std::env::temp_dir().join(format!("aqr-shell-mount-{}", std::process::id()));
    let t = archive(&d);
    let (menu, eject) = (command(MENU), command(EJECT_COMMAND));
    // (the menu's commands by their canonical names)
    let subs: Vec<IExplorerCommand> = {
        // SAFETY: the menu's enumeration, read to its end
        let e = unsafe { menu.EnumSubCommands().unwrap() };
        let mut all = Vec::new();
        loop {
            let mut one = [None];
            let mut n = 0;
            // SAFETY: an array of one, and its count
            if unsafe { e.Next(&mut one, Some(&mut n)) }.is_err() || n == 0 {
                break;
            }
            all.push(one[0].take().unwrap());
        }
        all
    };
    let sub = |item: Item| {
        // SAFETY: the commands' names
        subs.iter().find(|c| unsafe { c.GetCanonicalName() }.ok() == Some(item.canonical())).unwrap()
    };
    assert_eq!(subs.len(), 8);
    // SAFETY: the commands' titles, of COM's memory
    unsafe {
        for c in subs.iter().chain([&menu, &eject]) {
            let title = c.GetTitle(None).unwrap();
            assert!(!title.to_string().unwrap().is_empty());
            windows::Win32::System::Com::CoTaskMemFree(Some(title.0 as *const _));
        }
    }

    // of the archive: all (Mount disabled unless aqr-mount is beside the library and WinFsp is
    // installed); of a file of another type, those of compressing alone
    let available = built().join("aqr-mount.exe").is_file() && winfsp();
    let archive = t.to_str().unwrap();
    let other = d.join("t").join("a.txt");
    let other = other.to_str().unwrap();
    assert_eq!(state(sub(Item::Mount), archive, false).unwrap(), if available { ENABLED } else { DISABLED });
    for item in [Item::Test, Item::ExtractVersion, Item::NameVersion, Item::CompressZip, Item::CompressOptions] {
        assert_eq!(state(sub(item), archive, false).unwrap(), ENABLED, "{item:?}");
    }
    for item in [Item::Test, Item::Mount, Item::ExtractVersion, Item::NameVersion] {
        assert_eq!(state(sub(item), other, false).unwrap(), HIDDEN, "{item:?}");
    }
    for item in [Item::CompressPassphrase, Item::CompressZip, Item::Compress7z, Item::CompressOptions] {
        assert_eq!(state(sub(item), other, false).unwrap(), ENABLED, "{item:?}");
    }
    // Eject: not of a drive of another file system
    let system = std::env::var("SystemDrive").unwrap_or("C:".into());
    assert_eq!(state(&eject, &format!(r"{system}\"), true).unwrap(), HIDDEN);
    if !available {
        eprintln!("skipped: aqr-mount.exe not beside the library, or no WinFsp");
        std::fs::remove_dir_all(&d).unwrap();
        return;
    }

    // a drive of aqr-mount's: Eject shown, and invoked, ejected
    let mut child = Command::new(built().join("aqr-mount.exe"))
        .arg("--json")
        .arg(&t)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(child.stdout.take().unwrap()), &mut line).unwrap();
    let told: serde_json::Value = serde_json::from_str(&line).unwrap();
    let drive = told["mounted"].as_str().unwrap_or_else(|| panic!("{line}")).to_owned();
    let root = format!(r"{drive}\");
    // (told at once where the shell has the volume's file system; else put off, then told)
    match state(&eject, &root, false) {
        Ok(s) => assert_eq!(s, ENABLED),
        Err(e) => assert_eq!(e.code(), HRESULT(0x8000_000A_u32 as i32)),
    }
    assert_eq!(state(&eject, &root, true).unwrap(), ENABLED);
    // SAFETY: the command invoked on the drive's item
    unsafe { eject.Invoke(&items(&root), None).unwrap() };
    let since = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if since.elapsed() > Duration::from_secs(20) {
            _ = child.kill();
            panic!("{drive}: not ejected");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(child.wait().unwrap().success());
    assert!(!Path::new(&root).exists());
    std::fs::remove_dir_all(&d).unwrap();
}
