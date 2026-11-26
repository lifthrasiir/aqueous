//! The commands of an archive's view, which Explorer shows in its command bar
//! (`IExplorerCommandProvider`, of `CreateViewObject`): "Versions", of the archive's journal
//! points as its subcommands, as the background menu has them ([`crate::versions`]): "Latest"
//! and the points newest first, the one shown checked; one chosen browsed to in the same window
//! (by the site the commands are given). Hidden of an archive of no point.

use std::sync::{Arc, Mutex};

use serde_json::Value;
use windows::Win32::Foundation::{E_INVALIDARG, E_NOTIMPL, HWND, S_FALSE, S_OK};
use windows::Win32::System::Com::IBindCtx;
use windows::Win32::UI::Shell::{
    IEnumExplorerCommand, IEnumExplorerCommand_Impl, IExplorerCommand, IExplorerCommand_Impl, IExplorerCommandProvider,
    IExplorerCommandProvider_Impl, IShellItemArray, SHStrDupW,
};
use windows::core::{BOOL, GUID, HRESULT, HSTRING, IUnknown, Interface, PWSTR, Ref, implement};

use crate::client;
use crate::folder::Pidl;

/// The canonical name of "Versions".
pub const VERSIONS_COMMAND: GUID = GUID::from_u128(0x3f0c1a52_8b7e_4d29_9a64_e1c50b2d7f18);

// (`ECF_*`, `ECS_*`)
const HAS_SUBCOMMANDS: u32 = 0x1;
const SEPARATOR_AFTER: u32 = 0x40;
const ENABLED: u32 = 0;
const HIDDEN: u32 = 0x2;
const CHECKED: u32 = 0x8;
const RADIO_CHECK: u32 = 0x10;

/// Where the commands are: the archive shown (as of a point, where it is), its file's ID list,
/// the view's window.
#[derive(Clone)]
struct View {
    key: client::Key,
    file: Arc<Pidl>,
    hwnd: isize,
}

#[implement(IExplorerCommandProvider)]
pub struct Provider {
    view: View,
    _counted: crate::Counted,
}

impl Provider {
    pub fn new(key: client::Key, file: Pidl, hwnd: HWND) -> Provider {
        Provider { view: View { key, file: Arc::new(file), hwnd: hwnd.0 as isize }, _counted: crate::Counted::new() }
    }
}

impl IExplorerCommandProvider_Impl for Provider_Impl {
    fn GetCommands(
        &self,
        site: Ref<IUnknown>,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        let c: IExplorerCommand = Command::new(What::Versions, &self.view, site.cloned()).into();
        let e: IEnumExplorerCommand = Commands::new(vec![c]).into();
        // SAFETY: the caller's pointers
        unsafe { e.query(riid, ppv).ok() }
    }

    fn GetCommand(
        &self,
        id: *const GUID,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> windows::core::Result<()> {
        // SAFETY: the caller's GUID, where there is one
        if id.is_null() || unsafe { *id } != VERSIONS_COMMAND {
            return Err(E_INVALIDARG.into());
        }
        let c: IExplorerCommand = Command::new(What::Versions, &self.view, None).into();
        // SAFETY: the caller's pointers
        unsafe { c.query(riid, ppv).ok() }
    }
}

enum What {
    Versions,
    Latest,
    Point(Value),
}

#[implement(IExplorerCommand)]
struct Command {
    what: What,
    view: View,
    site: Mutex<Option<IUnknown>>,
    _counted: crate::Counted,
}

impl Command {
    fn new(what: What, view: &View, site: Option<IUnknown>) -> Command {
        Command { what, view: view.clone(), site: Mutex::new(site), _counted: crate::Counted::new() }
    }

    /// The archive's points (of all of it: as of a point, it knows of none after).
    fn points(&self) -> windows::core::Result<Vec<Value>> {
        client::journal(&client::Key { path: self.view.key.path.clone(), at: None })
    }
}

fn dup(s: &str) -> windows::core::Result<PWSTR> {
    // SAFETY: a string of its own, copied into COM's memory
    unsafe { SHStrDupW(&HSTRING::from(s)) }
}

impl IExplorerCommand_Impl for Command_Impl {
    fn GetTitle(&self, _: Ref<IShellItemArray>) -> windows::core::Result<PWSTR> {
        match &self.what {
            What::Versions => dup(aqr_l10n::folder::versions()),
            What::Latest => dup(aqr_l10n::folder::latest()),
            What::Point(p) => dup(&crate::folder::version_shown(p)),
        }
    }

    fn GetIcon(&self, _: Ref<IShellItemArray>) -> windows::core::Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }

    fn GetToolTip(&self, _: Ref<IShellItemArray>) -> windows::core::Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }

    fn GetCanonicalName(&self) -> windows::core::Result<GUID> {
        match self.what {
            What::Versions => Ok(VERSIONS_COMMAND),
            _ => Err(E_NOTIMPL.into()),
        }
    }

    fn GetState(&self, _: Ref<IShellItemArray>, _: BOOL) -> windows::core::Result<u32> {
        let at = self.view.key.at;
        Ok(match &self.what {
            What::Versions => match self.points() {
                Ok(p) if !p.is_empty() => ENABLED,
                _ => HIDDEN,
            },
            What::Latest if at.is_none() => RADIO_CHECK | CHECKED,
            What::Point(p) if at.is_some() && p["metadata"].as_u64() == at => RADIO_CHECK | CHECKED,
            _ => ENABLED,
        })
    }

    fn Invoke(&self, _: Ref<IShellItemArray>, _: Ref<IBindCtx>) -> windows::core::Result<()> {
        let p = match &self.what {
            What::Versions => return Ok(()),
            What::Latest => None,
            What::Point(p) => Some(p),
        };
        let t = crate::versions::target(self.view.file.as_ptr(), p);
        let r = crate::versions::browse(t, self.site.lock().unwrap().clone(), HWND(self.view.hwnd as *mut _));
        // SAFETY: of its own
        unsafe { windows::Win32::UI::Shell::ILFree(Some(t)) };
        if let Err(e) = &r {
            crate::log(|| format!("commands: {e}"));
        }
        r
    }

    fn GetFlags(&self) -> windows::core::Result<u32> {
        Ok(match self.what {
            What::Versions => HAS_SUBCOMMANDS,
            What::Latest => SEPARATOR_AFTER,
            What::Point(_) => 0,
        })
    }

    fn EnumSubCommands(&self) -> windows::core::Result<IEnumExplorerCommand> {
        if !matches!(self.what, What::Versions) {
            return Err(E_NOTIMPL.into());
        }
        let site = self.site.lock().unwrap().clone();
        let mut all: Vec<IExplorerCommand> = vec![Command::new(What::Latest, &self.view, site.clone()).into()];
        for p in self.points()?.into_iter().rev() {
            all.push(Command::new(What::Point(p), &self.view, site.clone()).into());
        }
        Ok(Commands::new(all).into())
    }
}

/// Commands, as an enumeration.
#[implement(IEnumExplorerCommand)]
struct Commands {
    all: Vec<IExplorerCommand>,
    at: Mutex<usize>,
    _counted: crate::Counted,
}

impl Commands {
    fn new(all: Vec<IExplorerCommand>) -> Commands {
        Commands { all, at: Mutex::new(0), _counted: crate::Counted::new() }
    }
}

impl IEnumExplorerCommand_Impl for Commands_Impl {
    fn Next(&self, n: u32, out: *mut Option<IExplorerCommand>, fetched: *mut u32) -> HRESULT {
        if out.is_null() {
            return windows::Win32::Foundation::E_POINTER;
        }
        let mut at = self.at.lock().unwrap();
        let mut k = 0;
        while k < n as usize && *at < self.all.len() {
            // SAFETY: the caller's array of `n`
            unsafe { out.add(k).write(Some(self.all[*at].clone())) };
            *at += 1;
            k += 1;
        }
        if !fetched.is_null() {
            // SAFETY: the caller's pointer
            unsafe { *fetched = k as u32 };
        }
        if k == n as usize { S_OK } else { S_FALSE }
    }

    fn Skip(&self, n: u32) -> windows::core::Result<()> {
        let mut at = self.at.lock().unwrap();
        *at = (*at + n as usize).min(self.all.len());
        Ok(())
    }

    fn Reset(&self) -> windows::core::Result<()> {
        *self.at.lock().unwrap() = 0;
        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IEnumExplorerCommand> {
        let c = Commands::new(self.all.clone());
        *c.at.lock().unwrap() = *self.at.lock().unwrap();
        Ok(c.into())
    }
}
