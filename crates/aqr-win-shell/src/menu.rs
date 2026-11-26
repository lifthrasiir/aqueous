//! The context menu of entries of an archive: the shell's own (`SHCreateDefaultContextMenu`:
//! Copy, Properties, and of folders the verbs of folders), with two of ours first of a file
//! chosen alone: "Open" (the default) and "Open with the files beside it" (a `.cue` and its
//! `.bin`, a page and its images: the files of its folder and of its folders named after it,
//! [`files_of`], written out too; over [`ASK_OVER`] bytes, once the user says so). The files are
//! written out by the server (`Materialize`) on a thread of their own, a window of how far it is
//! shown where it takes over [`QUIET`] (cancelled, the one being written stopped, nothing
//! opened), and the file opened as the system opens a file of its type.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Win32::Foundation::{E_NOTIMPL, HWND, LPARAM, S_FALSE, WPARAM};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IDataObject,
};
use windows::Win32::UI::Shell::{
    DFM_GETDEFSTATICID, DFM_GETVERBA, DFM_GETVERBW, DFM_INVOKECOMMAND, DFM_INVOKECOMMANDEX, DFM_MAPCOMMANDNAME,
    DFM_MERGECONTEXTMENU, IContextMenuCB, IContextMenuCB_Impl, IProgressDialog, IShellFolder, PROGDLG_AUTOTIME,
    PROGDLG_NOMINIMIZE, QCMINFO, SHELLEXECUTEINFOW, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    IDYES, InsertMenuW, MB_ICONERROR, MB_ICONQUESTION, MB_OK, MB_YESNO, MF_BYPOSITION, MF_STRING, MessageBoxW,
    SW_SHOWNORMAL, SetMenuDefaultItem,
};
use windows::core::{GUID, HSTRING, PCWSTR, Ref, implement};

use crate::client;

/// How long opening goes on before a window tells how far it is (as aqr-win's do).
const QUIET: Duration = Duration::from_millis(400);

/// `CLSID_ProgressDialog`.
const PROGRESS_DIALOG: GUID = GUID::from_u128(0xf8383852_fcd3_11d1_a6b9_006097df5bd4);

/// The callback of the menu: of a file chosen alone, its archive and path (our commands added).
#[implement(IContextMenuCB)]
pub struct Menu {
    pub open: Option<(Arc<client::Use>, String)>,
    pub _counted: crate::Counted,
}

/// Our commands' offsets and verbs.
const OPEN: usize = 0;
const OPEN_WITH_FOLDER: usize = 1;
const VERBS: [&str; 2] = ["open", "openwithfolder"];

impl IContextMenuCB_Impl for Menu_Impl {
    fn CallBack(
        &self,
        _: Ref<IShellFolder>,
        hwnd: HWND,
        _: Ref<IDataObject>,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> windows::core::Result<()> {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.call(hwnd, msg, wparam, lparam)));
        r.unwrap_or_else(|_| Err(windows::Win32::Foundation::E_UNEXPECTED.into()))
    }
}

impl Menu {
    fn call(&self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> windows::core::Result<()> {
        let msg = msg as i32;
        let ours = |id: usize| self.open.is_some() && id < VERBS.len();
        match msg {
            m if m == DFM_MERGECONTEXTMENU.0 => {
                if self.open.is_some() {
                    // SAFETY: the shell's QCMINFO, of its menu
                    unsafe {
                        let q = &mut *(lparam.0 as *mut QCMINFO);
                        let labels = [aqr_l10n::folder::open(), aqr_l10n::folder::open_with_folder()];
                        for (k, label) in labels.into_iter().enumerate() {
                            let flags = MF_BYPOSITION | MF_STRING;
                            InsertMenuW(q.hmenu, q.indexMenu, flags, q.idCmdFirst as usize + k, &HSTRING::from(label))?;
                            q.indexMenu += 1;
                        }
                        _ = SetMenuDefaultItem(q.hmenu, q.idCmdFirst + OPEN as u32, 0);
                        q.idCmdFirst += VERBS.len() as u32;
                    }
                }
                Ok(())
            }
            m if (m == DFM_INVOKECOMMAND.0 || m == DFM_INVOKECOMMANDEX.0) && ours(wparam.0) => {
                let (archive, path) = self.open.clone().expect("ours");
                open(archive, path, wparam.0 == OPEN_WITH_FOLDER, hwnd);
                Ok(())
            }
            // (a verb invoked by its name: the offset of our command of it)
            m if m == DFM_MAPCOMMANDNAME.0 && self.open.is_some() => {
                // SAFETY: the shell's verb, ended by 0, and the place of the offset
                unsafe {
                    let verb = PCWSTR(lparam.0 as *const u16).to_string().unwrap_or_default();
                    let k = VERBS
                        .iter()
                        .position(|v| v.eq_ignore_ascii_case(&verb))
                        .ok_or(windows::core::Error::from(E_NOTIMPL))?;
                    *(wparam.0 as *mut i32) = k as i32;
                }
                Ok(())
            }
            m if m == DFM_GETVERBW.0 || m == DFM_GETVERBA.0 => {
                let id = wparam.0 & 0xffff;
                if !ours(id) {
                    return Err(E_NOTIMPL.into());
                }
                let verb = format!("{}\0", VERBS[id]);
                let n = (wparam.0 >> 16) & 0xffff;
                // SAFETY: the shell's buffer of `n` characters
                unsafe {
                    match m == DFM_GETVERBW.0 {
                        true => {
                            let b = std::slice::from_raw_parts_mut(lparam.0 as *mut u16, n);
                            verb.encode_utf16().zip(b.iter_mut()).for_each(|(c, b)| *b = c);
                        }
                        false => {
                            let b = std::slice::from_raw_parts_mut(lparam.0 as *mut u8, n);
                            verb.bytes().zip(b.iter_mut()).for_each(|(c, b)| *b = c);
                        }
                    }
                }
                Ok(())
            }
            // (the shell's commands, its own: copy, properties, ...)
            m if m == DFM_INVOKECOMMAND.0 || m == DFM_INVOKECOMMANDEX.0 => Err(S_FALSE.into()),
            m if m == DFM_GETDEFSTATICID.0 => Err(E_NOTIMPL.into()),
            _ => Err(E_NOTIMPL.into()),
        }
    }
}

/// The files written out to open file `path`, with their sizes, the file itself first: where
/// `folder`, the files of its folder too (not under it) and those under its folders named after
/// it (`page.html`'s `page_files`, `page.files`: its name before the first `.`, then what is not
/// a letter nor a digit).
fn files_of(archive: &Path, path: &str, folder: bool) -> windows::core::Result<Vec<(String, u64)>> {
    let size = client::stat(archive, path)?["size"].as_u64().unwrap_or(0);
    let mut out = vec![(path.to_owned(), size)];
    if !folder {
        return Ok(out);
    }
    let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
    let stem = name.split('.').next().unwrap_or(name);
    let named = |d: &str| {
        d.strip_prefix(stem).is_some_and(|rest| rest.chars().next().is_some_and(|c| !c.is_alphanumeric()))
            && !stem.is_empty()
    };
    let mut dirs = Vec::new();
    for e in client::list(archive, dir)? {
        let n = e["name"].as_str().unwrap_or("");
        let p = crate::folder::joined(dir, n);
        match e["kind"].as_str() {
            Some("dir") if named(n) => dirs.push(p),
            Some("file") if p != path => out.push((p, e["size"].as_u64().unwrap_or(0))),
            _ => {}
        }
    }
    while let Some(d) = dirs.pop() {
        for e in client::list(archive, &d)? {
            let p = crate::folder::joined(&d, e["name"].as_str().unwrap_or(""));
            match e["kind"].as_str() {
                Some("dir") => dirs.push(p),
                Some("file") => out.push((p, e["size"].as_u64().unwrap_or(0))),
                _ => {}
            }
        }
    }
    Ok(out)
}

/// Over so many bytes in all, the files beside a file are written out only once the user says so.
const ASK_OVER: u64 = 256 << 20;

/// File `path` of `archive` (with those beside it, where `folder`) written out and opened, on a
/// thread of its own; a failure told in a box over `hwnd`.
fn open(archive: Arc<client::Use>, path: String, folder: bool, hwnd: HWND) {
    let hwnd = hwnd.0 as isize;
    let counted = crate::Counted::new();
    std::thread::spawn(move || {
        let _counted = counted;
        let hwnd = HWND(hwnd as *mut _);
        // SAFETY: this thread's COM
        if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_err() {
            return;
        }
        let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
        let r = written(&archive, &path, &name, folder, hwnd).and_then(|file| match file {
            Some(file) => execute(&file, hwnd),
            None => Ok(()),
        });
        if let Err(e) = r {
            crate::log(|| format!("open {path}: {e}"));
            if crate::TESTING.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let text = HSTRING::from(format!("{}\n\n{}", aqr_l10n::folder::not_opened(&name), e.message()));
            // SAFETY: strings of their own
            unsafe { MessageBoxW(Some(hwnd), &text, &HSTRING::from("Aqueous"), MB_OK | MB_ICONERROR) };
        }
        drop(archive);
        // SAFETY: initialized above, its objects let go of
        unsafe { CoUninitialize() };
    });
}

/// The files to open `path` written out (by a thread of their own), a window over `hwnd` told
/// how far where it takes long: the file's path, or none where the user cancelled.
fn written(
    archive: &Arc<client::Use>,
    path: &str,
    name: &str,
    folder: bool,
    hwnd: HWND,
) -> windows::core::Result<Option<PathBuf>> {
    let files = files_of(archive.path(), path, folder)?;
    let all: u64 = files.iter().map(|f| f.1).sum();
    if folder && all > ASK_OVER {
        let text = aqr_l10n::folder::ask_many(name, files.len(), aqr_l10n::size(all as f64));
        // SAFETY: strings of their own
        let r = unsafe {
            MessageBoxW(Some(hwnd), &HSTRING::from(text), &HSTRING::from("Aqueous"), MB_YESNO | MB_ICONQUESTION)
        };
        if r != IDYES {
            return Ok(None);
        }
    }
    // (the bytes of the files written whole, and the one being written; whether cancelled)
    let state: Arc<Mutex<(u64, String, bool)>> = Arc::new(Mutex::new((0, path.to_owned(), false)));
    let total = Arc::new(Mutex::new(all));
    let (to, done) = channel();
    {
        let (archive, state) = (archive.clone(), state.clone());
        std::thread::spawn(move || {
            // SAFETY: this thread's COM
            if unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_err() {
                return;
            }
            let r = (|| {
                let mut first = None;
                for (p, size) in files {
                    {
                        let mut s = state.lock().unwrap();
                        if s.2 {
                            // (cancelled: none written on)
                            return Err(windows::Win32::Foundation::E_ABORT.into());
                        }
                        s.1 = p.clone();
                    }
                    let file = client::materialize(archive.path(), &p)?;
                    first.get_or_insert(file);
                    state.lock().unwrap().0 += size;
                }
                Ok(first.expect("the file itself"))
            })();
            _ = to.send(r);
            // SAFETY: initialized above
            unsafe { CoUninitialize() };
        });
    }
    match done.recv_timeout(QUIET) {
        Ok(r) => return r.map(Some),
        Err(RecvTimeoutError::Disconnected) => return Err(windows::Win32::Foundation::E_FAIL.into()),
        Err(RecvTimeoutError::Timeout) => {}
    }
    // SAFETY: the shell's class, on this thread's COM
    let dialog: IProgressDialog = unsafe { CoCreateInstance(&PROGRESS_DIALOG, None, CLSCTX_INPROC_SERVER)? };
    // SAFETY: strings of their own
    unsafe {
        dialog.SetTitle(&HSTRING::from("Aqueous"))?;
        dialog.SetLine(1, &HSTRING::from(aqr_l10n::folder::opening(name)), false, None)?;
        dialog.StartProgressDialog(Some(hwnd), None, PROGDLG_AUTOTIME | PROGDLG_NOMINIMIZE, None)?;
    }
    let r = loop {
        match done.recv_timeout(Duration::from_millis(250)) {
            Ok(r) => break r.map(Some),
            Err(RecvTimeoutError::Disconnected) => break Err(windows::Win32::Foundation::E_FAIL.into()),
            Err(RecvTimeoutError::Timeout) => {}
        }
        // (of all: the files whole, and of the one at work, what is written or decompressed)
        let (whole, at, _) = state.lock().unwrap().clone();
        let total = (*total.lock().unwrap()).max(1);
        let (d, t) = client::progress(archive.path(), &at).unwrap_or((0, 0));
        let size = client::stat(archive.path(), &at).ok().and_then(|e| e["size"].as_u64()).unwrap_or(0);
        let part = if t > 0 { (size as u128 * d as u128 / t as u128) as u64 } else { 0 };
        // SAFETY: the dialog's calls
        unsafe {
            _ = dialog.SetProgress64(whole + part.min(size), total);
            if dialog.HasUserCancelled().as_bool() {
                // (the file being written stopped, and none after it: nothing opened)
                state.lock().unwrap().2 = true;
                _ = client::cancel(archive.path(), &at);
                break Ok(None);
            }
        }
    };
    // SAFETY: the dialog's call
    unsafe { _ = dialog.StopProgressDialog() };
    r
}

/// `file` opened as the system opens a file of its type.
fn execute(file: &Path, hwnd: HWND) -> windows::core::Result<()> {
    // (for tests: the file told, not run)
    if let Some(told) = std::env::var_os("AQR_SHELL_TEST_OPENED") {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(told)
            .map_err(|e| windows::core::Error::new(windows::Win32::Foundation::E_FAIL, e.to_string()))?;
        _ = writeln!(f, "{}", file.display());
        return Ok(());
    }
    let f = HSTRING::from(file.as_os_str());
    let dir = HSTRING::from(file.parent().map(PathBuf::from).unwrap_or_default().as_os_str());
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        hwnd,
        lpFile: PCWSTR(f.as_ptr()),
        lpDirectory: PCWSTR(dir.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    // SAFETY: a structure of strings of its own
    unsafe { ShellExecuteExW(&mut info) }
}
