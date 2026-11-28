//! "Eject" (of `Drive`): a command of the menu (`IExplorerCommand`, the verb's
//! `ExplorerCommandHandler`), shown of aqr-mount's drives alone and run by aqr-win (`eject DRIVE`:
//! its `mount`), which ejects through aqr-mount's control (crates/aqr-mount-win). And whether
//! "Mount as a drive" of the menu "Aqueous" ([`crate::more`]) is shown: [`available`].
//!
//! Whether Eject is shown is told at once, as Explorer asks it building the menu: of drives
//! whose file system is `aqr` (aqr-mount's volumes), by the shell item's
//! `System.Volume.FileSystem`, which the shell has; where it has it not, by the volume itself
//! (`GetVolumeInformationW`), which may be slow (a network drive's): then put off as Explorer
//! allows (`E_PENDING` where it is not to be slow, asked again on a thread of its own).
//!
//! Invoked, it runs aqr-win and returns: the work and its dialogs are aqr-win's.

use std::path::PathBuf;

use windows::Win32::Foundation::{E_NOTIMPL, PROPERTYKEY};
use windows::Win32::System::Com::IBindCtx;
use windows::Win32::UI::Shell::{
    IEnumExplorerCommand, IExplorerCommand, IExplorerCommand_Impl, IShellItem2, IShellItemArray, SHStrDupW,
    SIGDN_FILESYSPATH,
};
use windows::core::{BOOL, GUID, HSTRING, Interface, PWSTR, Ref, implement};

/// The class of "Eject".
pub const EJECT_COMMAND: GUID = GUID::from_u128(0x2e94c7a6_5f1b_4d38_a0c2_b7d6e18f4b93);

// (`ECS_*`)
const ENABLED: u32 = 0;
const HIDDEN: u32 = 0x2;

/// `E_PENDING`: told later, asked again where it may be slow.
const E_PENDING: windows::core::HRESULT = windows::core::HRESULT(0x8000_000A_u32 as i32);

/// `PKEY_Volume_FileSystem` (of `propkey.h`).
const VOLUME_FILE_SYSTEM: PROPERTYKEY =
    PROPERTYKEY { fmtid: GUID::from_u128(0x9b174b35_40ff_11d2_a27e_00c04fc30871), pid: 4 };

#[implement(IExplorerCommand)]
pub struct Command {
    _counted: crate::Counted,
}

/// aqr-win and aqr-mount, beside this library.
fn beside(name: &str) -> Option<PathBuf> {
    crate::library_dir().map(|d| d.join(name))
}

/// Whether archives may be mounted: aqr-mount.exe beside this library (aqr-win's folder, where
/// `aqr-mount --install` copies it) and WinFsp installed. A file and a key: no process asked.
pub fn available() -> bool {
    beside("aqr-mount.exe").is_some_and(|p| p.is_file()) && winfsp()
}

/// Whether WinFsp is installed: its installer's `InstallDir` (of the 32-bit key on x64).
fn winfsp() -> bool {
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    [windows::core::w!(r"SOFTWARE\WOW6432Node\WinFsp"), windows::core::w!(r"SOFTWARE\WinFsp")].iter().any(|&key| {
        // SAFETY: names ending in NUL; the size of the value alone asked
        unsafe {
            RegGetValueW(HKEY_LOCAL_MACHINE, key, windows::core::w!("InstallDir"), RRF_RT_REG_SZ, None, None, None)
        }
        .is_ok()
    })
}

/// The file system of the volume at `root` (`Q:\`), asked of the volume.
fn volume_file_system(root: &str) -> Option<String> {
    use windows::Win32::Storage::FileSystem::GetVolumeInformationW;
    let mut name = [0u16; 64];
    // SAFETY: a root ending in NUL, a buffer for the name
    unsafe { GetVolumeInformationW(&HSTRING::from(root), None, None, None, None, Some(&mut name)) }.ok()?;
    let n = name.iter().position(|&c| c == 0).unwrap_or(0);
    Some(String::from_utf16_lossy(&name[..n]))
}

/// The paths of the items chosen.
fn paths(items: &IShellItemArray) -> windows::core::Result<Vec<String>> {
    // SAFETY: the shell's array, read
    unsafe {
        let mut out = Vec::new();
        for i in 0..items.GetCount()? {
            let p = items.GetItemAt(i)?.GetDisplayName(SIGDN_FILESYSPATH)?;
            out.push(p.to_string().unwrap_or_default());
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
        }
        Ok(out)
    }
}

/// Whether the drive item `i` is of aqr-mount's: put off (`E_PENDING`) where only the volume
/// can tell and that is not to be slow.
fn of_aqr(items: &IShellItemArray, i: u32, slow: bool) -> windows::core::Result<bool> {
    // SAFETY: the shell's array and items, read
    unsafe {
        let item = items.GetItemAt(i)?;
        if let Ok(fs) = item.cast::<IShellItem2>().and_then(|i| i.GetString(&VOLUME_FILE_SYSTEM)) {
            let told = fs.to_string().unwrap_or_default();
            windows::Win32::System::Com::CoTaskMemFree(Some(fs.0 as *const _));
            return Ok(told == "aqr");
        }
        if !slow {
            return Err(E_PENDING.into());
        }
        let p = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let root = p.to_string().unwrap_or_default();
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
        Ok(volume_file_system(&root).as_deref() == Some("aqr"))
    }
}

fn dup(s: &str) -> windows::core::Result<PWSTR> {
    // SAFETY: a string of its own, copied into COM's memory
    unsafe { SHStrDupW(&HSTRING::from(s)) }
}

impl IExplorerCommand_Impl for Command_Impl {
    fn GetTitle(&self, _: Ref<IShellItemArray>) -> windows::core::Result<PWSTR> {
        dup(aqr_l10n::shell::eject())
    }

    fn GetIcon(&self, _: Ref<IShellItemArray>) -> windows::core::Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }

    fn GetToolTip(&self, _: Ref<IShellItemArray>) -> windows::core::Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }

    fn GetCanonicalName(&self) -> windows::core::Result<GUID> {
        Ok(EJECT_COMMAND)
    }

    fn GetState(&self, items: Ref<IShellItemArray>, slow: BOOL) -> windows::core::Result<u32> {
        let r = std::panic::catch_unwind(|| -> windows::core::Result<u32> {
            let Some(items) = items.as_ref() else { return Ok(HIDDEN) };
            // SAFETY: the shell's array
            let n = unsafe { items.GetCount()? };
            for i in 0..n {
                if !of_aqr(items, i, slow.as_bool())? {
                    return Ok(HIDDEN);
                }
            }
            Ok(if n > 0 { ENABLED } else { HIDDEN })
        });
        r.unwrap_or(Ok(HIDDEN))
    }

    fn Invoke(&self, items: Ref<IShellItemArray>, _: Ref<IBindCtx>) -> windows::core::Result<()> {
        let items = items.ok()?;
        let Some(exe) = beside("aqr-win.exe") else { return Err(E_NOTIMPL.into()) };
        let verb = "eject";
        for p in paths(items)? {
            crate::log(|| format!("mount: {verb} {p}"));
            // (run, not waited for: the dialogs are aqr-win's)
            if let Err(e) = std::process::Command::new(&exe).arg(verb).arg(&p).spawn() {
                crate::log(|| format!("mount: {}: {e}", exe.display()));
            }
        }
        Ok(())
    }

    fn GetFlags(&self) -> windows::core::Result<u32> {
        Ok(0)
    }

    fn EnumSubCommands(&self) -> windows::core::Result<IEnumExplorerCommand> {
        Err(E_NOTIMPL.into())
    }
}

/// "Eject", as an interface.
pub fn eject() -> IExplorerCommand {
    Command { _counted: crate::Counted::new() }.into()
}
