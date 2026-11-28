//! The menu "Aqueous" of files and folders (`IExplorerCommand` of subcommands, the verb
//! `Aqueous.more`'s `ExplorerCommandHandler` of `*` and `Directory`): beside "Compress with
//! Aqueous", "Extract here" and "Extract to…", which stay verbs of their own, what is used less.
//!
//! - Of archives (their extensions: aqr-ipc's `shell::ARCHIVES`) chosen: "Test"; "Mount as a
//!   drive" of one, disabled where aqr-mount or WinFsp is not installed
//!   ([`crate::mount::available`]: shown all the same, so that it is seen to need them);
//!   "Extract a previous version…" and "Name this version…" of one `.aqr` file;
//! - then, of all, after a separator: "Compress with passphrase…", "Compress into .zip" and
//!   ".7z", and after another, "Compress with options…".
//!
//! The menu does not know what is chosen as it is listed (`EnumSubCommands` is told nothing),
//! so each command tells whether it is shown of the items it is asked of (`GetState`, at once:
//! of their extensions, and a file and a key of Mount). Invoked, a verb of many paths drops them
//! on aqr-win's class of it (`aqr-win --drop VERB`, as Explorer drops them on a verb's
//! `DropTarget`: all at once, no command line to hold them), the others run aqr-win on the one.

use std::path::Path;

use windows::Win32::Foundation::{E_NOTIMPL, POINTL};
use windows::Win32::System::Com::{CLSCTX_LOCAL_SERVER, CoCreateInstance, IBindCtx, IDataObject};
use windows::Win32::System::Ole::{DROPEFFECT_COPY, IDropTarget};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{
    BHID_DataObject, IEnumExplorerCommand, IExplorerCommand, IExplorerCommand_Impl, IShellItemArray, SHStrDupW,
    SIGDN_FILESYSPATH,
};
use windows::core::{BOOL, GUID, HSTRING, PWSTR, Ref, implement};

/// The class of the menu.
pub const MENU: GUID = GUID::from_u128(0x5b17e3a9_d64c_4f20_8e71_a92c0b6d4f38);

// (`ECF_*`, `ECS_*`)
const HAS_SUBCOMMANDS: u32 = 0x1;
const SEPARATOR_BEFORE: u32 = 0x20;
const ENABLED: u32 = 0;
const DISABLED: u32 = 0x1;
const HIDDEN: u32 = 0x2;

/// The commands of the menu, in its order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Item {
    Menu,
    Test,
    Mount,
    ExtractVersion,
    NameVersion,
    CompressPassphrase,
    CompressZip,
    Compress7z,
    CompressOptions,
}

const ITEMS: [Item; 8] = [
    Item::Test,
    Item::Mount,
    Item::ExtractVersion,
    Item::NameVersion,
    Item::CompressPassphrase,
    Item::CompressZip,
    Item::Compress7z,
    Item::CompressOptions,
];

impl Item {
    /// aqr-win's verb.
    fn verb(self) -> &'static str {
        match self {
            Item::Menu => "",
            Item::Test => "test",
            Item::Mount => "mount",
            Item::ExtractVersion => "extract-version",
            Item::NameVersion => "name-version",
            Item::CompressPassphrase => "compress-passphrase",
            Item::CompressZip => "compress-zip",
            Item::Compress7z => "compress-7z",
            Item::CompressOptions => "compress-options",
        }
    }

    fn title(self) -> &'static str {
        use aqr_l10n::shell;
        match self {
            Item::Menu => "Aqueous",
            Item::Test => shell::test(),
            Item::Mount => shell::mount(),
            Item::ExtractVersion => shell::extract_version(),
            Item::NameVersion => shell::name_version(),
            Item::CompressPassphrase => shell::compress_passphrase(),
            Item::CompressZip => shell::compress_zip(),
            Item::Compress7z => shell::compress_7z(),
            Item::CompressOptions => shell::compress_options(),
        }
    }

    /// Its canonical name: the menu's class, the others' its with their index in the last byte.
    pub fn canonical(self) -> GUID {
        let i = ITEMS.iter().position(|&i| i == self).map_or(0, |i| i + 1);
        GUID::from_u128(MENU.to_u128() ^ i as u128)
    }

    /// Its state of `paths`, the items chosen: shown or not, and Mount disabled where it cannot
    /// mount.
    fn state(self, paths: &[String]) -> u32 {
        match self.shown(paths) {
            false => HIDDEN,
            true if self == Item::Mount && !crate::mount::available() => DISABLED,
            true => ENABLED,
        }
    }

    /// Whether it is shown of `paths`, the items chosen.
    fn shown(self, paths: &[String]) -> bool {
        let archives = !paths.is_empty() && paths.iter().all(|p| aqr_ipc::shell::is_archive(Path::new(p)));
        let one = paths.len() == 1;
        match self {
            Item::Menu => true,
            Item::Test => archives,
            Item::Mount => archives && one,
            Item::ExtractVersion | Item::NameVersion => one && aqr_ipc::shell::is_aqueous(Path::new(&paths[0])),
            Item::CompressPassphrase | Item::CompressZip | Item::Compress7z | Item::CompressOptions => {
                !paths.is_empty()
            }
        }
    }
}

#[implement(IExplorerCommand)]
pub struct Command {
    item: Item,
    _counted: crate::Counted,
}

impl Command {
    pub fn new(item: Item) -> Command {
        Command { item, _counted: crate::Counted::new() }
    }
}

fn dup(s: &str) -> windows::core::Result<PWSTR> {
    // SAFETY: a string of its own, copied into COM's memory
    unsafe { SHStrDupW(&HSTRING::from(s)) }
}

/// The paths of the items chosen (of the file system: none of an item of another).
fn paths(items: &IShellItemArray) -> windows::core::Result<Vec<String>> {
    // SAFETY: the shell's array, read
    unsafe {
        let mut out = Vec::new();
        for i in 0..items.GetCount()? {
            let Ok(p) = items.GetItemAt(i)?.GetDisplayName(SIGDN_FILESYSPATH) else { return Ok(Vec::new()) };
            out.push(p.to_string().unwrap_or_default());
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
        }
        Ok(out)
    }
}

/// The items dropped on aqr-win's class of `verb`, as Explorer drops them on a verb's
/// `DropTarget`: the class's program started (`--drop VERB`), given them all at once.
fn drop_on(verb: &str, items: &IShellItemArray) -> windows::core::Result<()> {
    let clsid = GUID::from_u128(aqr_ipc::shell::drop_class(verb).ok_or(E_NOTIMPL)?);
    // SAFETY: the shell's array as a data object, dropped on the class's object
    unsafe {
        let data: IDataObject = items.BindToHandler(None, &BHID_DataObject)?;
        let target: IDropTarget = CoCreateInstance(&clsid, None, CLSCTX_LOCAL_SERVER)?;
        let mut effect = DROPEFFECT_COPY;
        target.DragEnter(&data, MODIFIERKEYS_FLAGS(0), POINTL::default(), &mut effect)?;
        let mut effect = DROPEFFECT_COPY;
        target.Drop(&data, MODIFIERKEYS_FLAGS(0), POINTL::default(), &mut effect)
    }
}

impl IExplorerCommand_Impl for Command_Impl {
    fn GetTitle(&self, _: Ref<IShellItemArray>) -> windows::core::Result<PWSTR> {
        dup(self.item.title())
    }

    /// aqr-win's icon (resource 1) of the menu; none of its commands.
    fn GetIcon(&self, _: Ref<IShellItemArray>) -> windows::core::Result<PWSTR> {
        match (self.item, crate::library_dir()) {
            (Item::Menu, Some(d)) => dup(&format!("{},-1", d.join("aqr-win.exe").display())),
            _ => Err(E_NOTIMPL.into()),
        }
    }

    fn GetToolTip(&self, _: Ref<IShellItemArray>) -> windows::core::Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }

    fn GetCanonicalName(&self) -> windows::core::Result<GUID> {
        Ok(self.item.canonical())
    }

    fn GetState(&self, items: Ref<IShellItemArray>, _: BOOL) -> windows::core::Result<u32> {
        let r = std::panic::catch_unwind(|| {
            let paths = items.as_ref().and_then(|i| paths(i).ok()).unwrap_or_default();
            self.item.state(&paths)
        });
        Ok(r.unwrap_or(HIDDEN))
    }

    fn Invoke(&self, items: Ref<IShellItemArray>, _: Ref<IBindCtx>) -> windows::core::Result<()> {
        let items = items.ok()?;
        let verb = self.item.verb();
        crate::log(|| format!("more: {verb}"));
        if aqr_ipc::shell::drop_class(verb).is_some() {
            return drop_on(verb, items).inspect_err(|e| crate::log(|| format!("more: {verb}: {e}")));
        }
        let Some(exe) = crate::library_dir().map(|d| d.join("aqr-win.exe")) else { return Err(E_NOTIMPL.into()) };
        for p in paths(items)?.into_iter().take(1) {
            // (run, not waited for: the dialogs are aqr-win's)
            if let Err(e) = std::process::Command::new(&exe).arg(verb).arg(&p).spawn() {
                crate::log(|| format!("more: {}: {e}", exe.display()));
            }
        }
        Ok(())
    }

    fn GetFlags(&self) -> windows::core::Result<u32> {
        Ok(match self.item {
            Item::Menu => HAS_SUBCOMMANDS,
            // (the commands of all after those of archives; the dialog of the options last)
            Item::CompressPassphrase | Item::CompressZip | Item::CompressOptions => SEPARATOR_BEFORE,
            _ => 0,
        })
    }

    fn EnumSubCommands(&self) -> windows::core::Result<IEnumExplorerCommand> {
        if self.item != Item::Menu {
            return Err(E_NOTIMPL.into());
        }
        let all: Vec<IExplorerCommand> = ITEMS.iter().map(|&i| Command::new(i).into()).collect();
        Ok(crate::commands::Commands::new(all).into())
    }
}

/// The menu, as an interface.
pub fn menu() -> IExplorerCommand {
    Command::new(Item::Menu).into()
}
