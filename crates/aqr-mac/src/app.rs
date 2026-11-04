//! The app: an agent with no window and no Dock icon (`LSUIElement`), its work given by Finder
//! and shown in Finder.
//!
//! - Finder's services (`NSServices` in `Info.plist`, under Quick Actions or Services of a
//!   Control-click) send the paths chosen, all at once, on a pasteboard: "Compress with Aqueous"
//!   on files and folders ("Compress with Aqueous (Passphrase)…" encrypts: `aqr -P`), "Extract
//!   with Aqueous" and "Test with Aqueous" on `.aqr` files and the other archives `aqr` extracts.
//!   Those opened (a double-click; [`EXTRACTED`]) are extracted: beside them, a single entry at
//!   the top as it is, else into their names without the extension (`aqr --each --auto-dir`).
//!   Other formats than `.aqr` are document types of the rank `Alternate` (`Info.plist`): opened
//!   by Aqueous where no other app does, else where the user chooses it.
//! - How far a job is shows on its file in Finder: an `NSProgress` published for it, a bar on
//!   the file's icon as of a file downloading, with a button that stops the job (`aqr`'s
//!   output being written removed). The file is the archive being written (`NAME.PID.part`),
//!   what is extracted (the archive until that is decided), or the archive being tested.
//! - Alerts tell errors (with the last lines `aqr` wrote), what a test checked, and ask about an
//!   output there already and for passphrases (a secure field, two to choose one; asked again
//!   where it was wrong, and `aqr`'s error is not told where the user gives up).
//! - Run by itself (not for a file or a service), it registers itself with Launch Services and
//!   the services, says so, and offers the `aqr` command on `PATH`; then it quits, as it does
//!   once its jobs are done.

use std::cell::{Cell, OnceCell, RefCell};
use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::Instant;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication, NSApplicationDelegate, NSPasteboard,
    NSPasteboardTypeFileURL, NSScrollView, NSSecureTextField, NSTextView, NSUpdateDynamicServices, NSView,
};
use objc2_foundation::{
    NSArray, NSBundle, NSNotification, NSNumber, NSObject, NSObjectProtocol, NSPoint, NSProgress,
    NSProgressFileOperationKindCopying, NSProgressFileOperationKindDecompressingAfterDownloading, NSProgressKindFile,
    NSRect, NSSize, NSString, NSTimer, NSURL,
};
use serde_json::Value;

use aqr_l10n::{common, mac, services};

use crate::job::{Job, Verb, failure, file_name, intact, tested};

/// How often the jobs are looked at, in seconds.
const TICK: f64 = 0.25;

/// The time the app waits, run for a service, for its message before it quits.
const IDLE: f64 = 10.0;

/// The time the app waits, run as by itself, for a file or a service before it welcomes: a
/// launch for a service is told as a launch of its own too (`NSApplicationLaunchIsDefaultLaunchKey`
/// true), its message coming after.
const WELCOME: f64 = 1.0;

/// The progress bar's units.
const UNITS: i64 = 1000;

/// The extensions of the files opened that are extracted (`Info.plist`'s document types; other
/// files opened are compressed).
const EXTRACTED: [&str; 15] =
    ["aqr", "zip", "7z", "rar", "alz", "egg", "tar", "tgz", "tbz2", "txz", "gz", "bz2", "xz", "lzma", "zst"];

/// A job, as the app keeps it.
struct Running {
    job: Arc<Job>,
    /// The progress published, and for which file.
    progress: Option<(PathBuf, Retained<NSProgress>)>,
    /// The answers given to all of a kind.
    always: Vec<(String, Value)>,
}

#[derive(Default)]
pub struct Ivars {
    jobs: RefCell<Vec<Running>>,
    /// Whether a job was ever begun, whether the app was launched as by itself (a welcome due),
    /// and when.
    worked: Cell<bool>,
    by_itself: Cell<bool>,
    launched: OnceCell<Instant>,
    timer: OnceCell<Retained<NSTimer>>,
}

define_class!(
    // SAFETY: NSObject has no requirements of a subclass; `Delegate` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct Delegate;

    // SAFETY: no requirements
    unsafe impl NSObjectProtocol for Delegate {}

    // SAFETY: no requirements; the signatures are the protocol's
    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationWillFinishLaunching:))]
        fn will_finish_launching(&self, _: &NSNotification) {
            _ = self.ivars().launched.set(Instant::now());
            let app = NSApplication::sharedApplication(self.mtm());
            // SAFETY: an object answering the messages of `NSServices` (below)
            unsafe { app.setServicesProvider(Some(self)) };
        }

        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, n: &NSNotification) {
            let this: Retained<Delegate> = self.retain();
            let tick = RcBlock::new(move |_: NonNull<NSTimer>| this.tick());
            // SAFETY: the block lives as long as the timer
            let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(TICK, true, &tick) };
            _ = self.ivars().timer.set(timer);
            self.ivars().by_itself.set(default_launch(n));
        }

        #[unsafe(method(application:openURLs:))]
        fn open_urls(&self, _: &NSApplication, urls: &NSArray<NSURL>) {
            let paths: Vec<PathBuf> = urls.iter().filter_map(|u| u.to_file_path()).collect();
            let (archives, others): (Vec<_>, Vec<_>) =
                paths.into_iter().partition(|p| {
                    p.extension().is_some_and(|e| EXTRACTED.iter().any(|x| e.eq_ignore_ascii_case(x)))
                });
            if !archives.is_empty() {
                self.start(Verb::Extract, archives);
            }
            if !others.is_empty() {
                self.start(Verb::Compress, others);
            }
        }
    }

    // The services of `Info.plist` (`NSMessage`).
    impl Delegate {
        #[unsafe(method(compressFiles:userData:error:))]
        fn compress_files(&self, pb: &NSPasteboard, _: Option<&NSString>, _: *mut *mut NSString) {
            self.start(Verb::Compress, paths(pb));
        }

        #[unsafe(method(compressFilesWithPassphrase:userData:error:))]
        fn compress_files_with_passphrase(&self, pb: &NSPasteboard, _: Option<&NSString>, _: *mut *mut NSString) {
            self.start_with(Verb::Compress, paths(pb), true);
        }

        #[unsafe(method(extractFiles:userData:error:))]
        fn extract_files(&self, pb: &NSPasteboard, _: Option<&NSString>, _: *mut *mut NSString) {
            self.start(Verb::Extract, paths(pb));
        }

        #[unsafe(method(testFiles:userData:error:))]
        fn test_files(&self, pb: &NSPasteboard, _: Option<&NSString>, _: *mut *mut NSString) {
            self.start(Verb::Test, paths(pb));
        }
    }
);

/// Chooses the language of the messages: the first of the user's (this app's own in System
/// Settings, else the system's) that the catalogs have, as `NSBundle` prefers localizations.
pub fn init_language() {
    let available: Vec<Retained<NSString>> = aqr_l10n::LANGS.iter().map(|t| NSString::from_str(t)).collect();
    let preferred = NSBundle::preferredLocalizationsFromArray(&NSArray::from_retained_slice(&available));
    let preferred: Vec<String> = preferred.iter().map(|s| s.to_string()).collect();
    aqr_l10n::init(&preferred);
}

impl Delegate {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars::default());
        // SAFETY: NSObject's `init`
        unsafe { msg_send![super(this), init] }
    }

    pub fn as_app_delegate(&self) -> &ProtocolObject<dyn NSApplicationDelegate> {
        ProtocolObject::from_ref(self)
    }

    fn start(&self, verb: Verb, paths: Vec<PathBuf>) {
        self.start_with(verb, paths, false);
    }

    /// Begins a job, its archives encrypted (`aqr -P`) where `encrypt`.
    fn start_with(&self, verb: Verb, paths: Vec<PathBuf>, encrypt: bool) {
        if paths.is_empty() {
            return;
        }
        self.ivars().worked.set(true);
        match Job::start(verb, paths, encrypt) {
            Ok(job) => self.ivars().jobs.borrow_mut().push(Running { job, progress: None, always: Vec::new() }),
            Err(e) => alert(self.mtm(), NSAlertStyle::Critical, &verb.failed(""), &e, None),
        }
    }

    /// Looks at the jobs: their progress shown, their questions asked, those done told of.
    fn tick(&self) {
        let mtm = self.mtm();
        // (taken out: an alert's loop may take messages that begin jobs)
        let jobs = std::mem::take(&mut *self.ivars().jobs.borrow_mut());
        let mut left = Vec::new();
        for mut r in jobs {
            let question = r.job.ipc.state.lock().unwrap().questions.pop_front();
            if let Some(q) = question {
                let answer = ask(mtm, &mut r, &q);
                if q["kind"] == "passphrase" && answer.is_null() {
                    // (given up: aqr's error for it is not told)
                    r.job.ipc.state.lock().unwrap().cancelled = true;
                }
                r.job.ipc.answer(&q, answer);
            }
            if let Some(status) = r.job.done() {
                if let Some((_, p)) = r.progress.take() {
                    p.unpublish();
                }
                finished(mtm, &r.job, status);
            } else {
                show(&mut r);
                left.push(r);
            }
        }
        let mut jobs = self.ivars().jobs.borrow_mut();
        left.append(&mut jobs);
        *jobs = left;
        let launched = self.ivars().launched.get().map_or(0.0, |t| t.elapsed().as_secs_f64());
        if self.ivars().by_itself.get() && !self.ivars().worked.get() && launched > WELCOME {
            drop(jobs);
            self.ivars().by_itself.set(false);
            welcome(mtm);
            NSApplication::sharedApplication(mtm).terminate(None);
            return;
        }
        if jobs.is_empty() && (self.ivars().worked.get() || launched > IDLE) {
            drop(jobs);
            NSApplication::sharedApplication(mtm).terminate(None);
        }
    }
}

/// The paths of the files on pasteboard `pb` (Finder's file reference URLs made paths).
fn paths(pb: &NSPasteboard) -> Vec<PathBuf> {
    let Some(items) = pb.pasteboardItems() else { return Vec::new() };
    let mut paths = Vec::new();
    for item in items.iter() {
        // SAFETY: a static of AppKit's
        let Some(s) = item.stringForType(unsafe { NSPasteboardTypeFileURL }) else { continue };
        let url = NSURL::URLWithString(&s).and_then(|u| u.filePathURL());
        paths.extend(url.and_then(|u| u.to_file_path()));
    }
    paths
}

/// Shows how far job `r` is on its file, published once the file is there (again for each
/// archive).
fn show(r: &mut Running) {
    let job = &r.job;
    let Some(file) = job.shown() else { return };
    if r.progress.as_ref().is_some_and(|(f, _)| *f != file)
        && let Some((_, p)) = r.progress.take()
    {
        p.unpublish();
    }
    if r.progress.is_none() {
        if !file.exists() {
            return;
        }
        let Some(url) = NSURL::from_file_path(&file) else { return };
        // SAFETY: a progress of no parent; the statics of Foundation's; a block of no
        // requirements but `Job::stop`'s, of any thread
        let p = unsafe {
            let p = NSProgress::initWithParent_userInfo(NSProgress::alloc(), None, None);
            p.setKind(Some(NSProgressKindFile));
            p.setFileOperationKind(Some(match job.ipc.verb {
                Verb::Compress => NSProgressFileOperationKindCopying,
                Verb::Extract | Verb::Test => NSProgressFileOperationKindDecompressingAfterDownloading,
            }));
            p.setFileURL(Some(&url));
            p.setTotalUnitCount(UNITS);
            p.setCancellable(true);
            p.setPausable(false);
            let j = job.clone();
            p.setCancellationHandler(Some(&RcBlock::new(move || j.stop())));
            p.publish();
            p
        };
        r.progress = Some((file, p));
    }
    let (_, p) = r.progress.as_ref().unwrap();
    let f = job.ipc.share();
    p.setCompletedUnitCount((f * UNITS as f64) as i64);
    let since = job.ipc.state.lock().unwrap().since;
    if let Some(t) = since.map(|s| s.elapsed().as_secs_f64())
        && f >= 0.02
        && t >= 3.0
    {
        p.setEstimatedTimeRemaining(Some(&NSNumber::new_f64(t * (1.0 - f) / f)));
    }
}

/// Tells what came of `job`, ended with `status`: an error, or what a test checked.
fn finished(mtm: MainThreadMarker, job: &Job, status: std::process::ExitStatus) {
    let s = job.ipc.state.lock().unwrap();
    if s.stopped || s.cancelled {
        return;
    }
    let name = s.input().map(file_name).unwrap_or_default();
    if let Some(e) = failure(&s, Some(status)) {
        let main = job.ipc.verb.failed(&name);
        let log: Vec<&str> = s.log.iter().map(String::as_str).collect();
        let log = log.join("\n");
        alert(mtm, NSAlertStyle::Critical, &main, &e, (!log.is_empty()).then_some(&log));
    } else if job.ipc.verb == Verb::Test {
        let lines: Vec<String> = s.ended.iter().map(|(n, e)| tested(n, e, job.ipc.count > 1)).collect();
        alert(mtm, NSAlertStyle::Informational, &intact(&name, job.ipc.count), &lines.join("\n"), None);
    }
}

/// The answer to question `q` of job `r` (or given before for all).
fn ask(mtm: MainThreadMarker, r: &mut Running, q: &Value) -> Value {
    let kind = q["kind"].as_str().unwrap_or("");
    if let Some((_, a)) = r.always.iter().find(|(k, _)| k == kind) {
        return a.clone();
    }
    match kind {
        "overwrite" => {
            let path = PathBuf::from(q["path"].as_str().unwrap_or(""));
            let a = NSAlert::new(mtm);
            a.setAlertStyle(NSAlertStyle::Warning);
            a.setMessageText(&NSString::from_str(&common::is_there_already(file_name(&path))));
            let (info, yes) = if path.is_dir() {
                (mac::extract_into(), mac::extract())
            } else {
                (mac::replace_file(), mac::replace())
            };
            a.setInformativeText(&NSString::from_str(&format!("{}\n\n{info}", path.display())));
            a.addButtonWithTitle(&NSString::from_str(yes));
            a.addButtonWithTitle(&NSString::from_str(mac::skip()));
            if r.job.ipc.count > 1 {
                a.setShowsSuppressionButton(true);
                if let Some(b) = a.suppressionButton() {
                    b.setTitle(&NSString::from_str(common::same_for_others()));
                }
            }
            activate(mtm);
            let answer = Value::Bool(a.runModal() == NSAlertFirstButtonReturn);
            if a.suppressionButton().is_some_and(|b| b.state() != 0) {
                r.always.push((kind.into(), answer.clone()));
            }
            answer
        }
        "passphrase" => passphrase(mtm, q).map_or(Value::Null, Value::String),
        _ => Value::Null,
    }
}

/// The passphrase asked for by question `q`, typed twice where it says `confirm`; `None` where
/// the user gives up.
fn passphrase(mtm: MainThreadMarker, q: &Value) -> Option<String> {
    let confirm = q["confirm"].as_bool().unwrap_or(false);
    let compress = q["mode"] == "compress";
    let path = q["path"].as_str().map(PathBuf::from);
    let mut note = if q["again"].as_bool().unwrap_or(false) { Some(common::wrong_passphrase()) } else { None };
    loop {
        let a = NSAlert::new(mtm);
        a.setAlertStyle(NSAlertStyle::Informational);
        let main = match (&path, compress) {
            (Some(p), true) => mac::choose_name(file_name(p)),
            (Some(p), false) => mac::encrypted_name(file_name(p)),
            (None, true) => mac::choose().to_string(),
            (None, false) => mac::encrypted().to_string(),
        };
        a.setMessageText(&NSString::from_str(&main));
        let mut info = String::from(if compress { mac::needed() } else { mac::enter() });
        if let Some(n) = note {
            info = format!("{n}\n\n{info}");
        }
        a.setInformativeText(&NSString::from_str(&info));
        let fields: Vec<_> = (0..if confirm { 2 } else { 1 })
            .map(|i| {
                let y = if confirm { 28.0 * (1 - i) as f64 } else { 0.0 };
                let frame = NSRect::new(NSPoint::new(0.0, y), NSSize::new(260.0, 24.0));
                let f = NSSecureTextField::initWithFrame(NSSecureTextField::alloc(mtm), frame);
                f.setPlaceholderString(Some(&NSString::from_str(if i == 0 {
                    mac::passphrase()
                } else {
                    mac::passphrase_again()
                })));
                f
            })
            .collect();
        let height = if confirm { 52.0 } else { 24.0 };
        let view =
            NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(260.0, height)));
        for f in &fields {
            view.addSubview(f);
        }
        a.setAccessoryView(Some(&view));
        a.addButtonWithTitle(&NSString::from_str(common::ok()));
        a.addButtonWithTitle(&NSString::from_str(common::cancel()));
        a.layout();
        a.window().setInitialFirstResponder(Some(&fields[0]));
        activate(mtm);
        if a.runModal() != NSAlertFirstButtonReturn {
            return None;
        }
        let value = fields[0].stringValue().to_string();
        if !confirm || value == fields[1].stringValue().to_string() {
            return Some(value);
        }
        note = Some(common::passphrases_differ());
    }
}

fn activate(mtm: MainThreadMarker) {
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
}

/// Shows an alert, `more` in a box of text below.
fn alert(mtm: MainThreadMarker, style: NSAlertStyle, main: &str, info: &str, more: Option<&str>) {
    let a = NSAlert::new(mtm);
    a.setAlertStyle(style);
    a.setMessageText(&NSString::from_str(main));
    a.setInformativeText(&NSString::from_str(info));
    if let Some(m) = more {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(480.0, 160.0));
        let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
        scroll.setHasVerticalScroller(true);
        let text = NSTextView::initWithFrame(NSTextView::alloc(mtm), frame);
        text.setString(&NSString::from_str(m));
        text.setEditable(false);
        scroll.setDocumentView(Some(&text));
        a.setAccessoryView(Some(&scroll));
    }
    activate(mtm);
    a.runModal();
}

/// Whether the app was run by itself, not to open a file or for a service.
fn default_launch(n: &NSNotification) -> bool {
    let Some(info) = n.userInfo() else { return false };
    // SAFETY: a static of AppKit's
    let key: &AnyObject = unsafe { objc2_app_kit::NSApplicationLaunchIsDefaultLaunchKey };
    info.objectForKey(key).and_then(|v| v.downcast::<NSNumber>().ok()).is_some_and(|v| v.boolValue())
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    /// Launch Services' (a `CFURLRef`, which an `NSURL` is).
    fn LSRegisterURL(url: *const c_void, update: u8) -> i32;
}

/// Where the `aqr` command goes.
const COMMAND: &str = "/usr/local/bin/aqr";

/// Registers the app (`.aqr` files and the services), and says so; or says where it is to be
/// first, where it runs translocated (from a download not moved, a copy of it at a place of the
/// system's own, read only).
fn welcome(mtm: MainThreadMarker) {
    let bundle = NSBundle::mainBundle();
    let path = bundle.bundlePath().to_string();
    if path.contains("/AppTranslocation/") {
        alert(mtm, NSAlertStyle::Warning, mac::move_first(), mac::move_info(), None);
        return;
    }
    let url = bundle.bundleURL();
    // SAFETY: an NSURL, toll-free bridged to CFURL
    let status = unsafe { LSRegisterURL(Retained::as_ptr(&url).cast(), 1) };
    NSUpdateDynamicServices();
    if status != 0 {
        let e = mac::launch_services(&path, status);
        alert(mtm, NSAlertStyle::Critical, mac::not_registered(), &e, None);
        return;
    }
    let aqr = std::env::current_exe().ok().map(|e| e.with_file_name("aqr")).filter(|p| p.is_file());
    let linked = aqr.as_ref().is_some_and(|a| std::fs::canonicalize(COMMAND).ok() == std::fs::canonicalize(a).ok());
    let a = NSAlert::new(mtm);
    a.setMessageText(&NSString::from_str(mac::ready()));
    let mut info =
        mac::ready_info(services::compress(), services::compress_passphrase(), services::extract(), services::test());
    if aqr.is_some() && !linked {
        info += "\n\n";
        info += &match there() {
            None => mac::command_offer(COMMAND),
            Some(t) => mac::command_other(COMMAND, t.what()),
        };
    }
    a.setInformativeText(&NSString::from_str(&info));
    a.addButtonWithTitle(&NSString::from_str(common::ok()));
    if let (Some(aqr), false) = (&aqr, linked) {
        a.addButtonWithTitle(&NSString::from_str(mac::install_command()));
        activate(mtm);
        if a.runModal() != NSAlertFirstButtonReturn {
            install_command(mtm, aqr);
        }
    } else {
        activate(mtm);
        a.runModal();
    }
}

/// What is at [`COMMAND`].
enum There {
    /// A link to that path, and whether something is there.
    Link(PathBuf, bool),
    Folder,
    File,
}

impl There {
    /// What it is, as told.
    fn what(&self) -> String {
        match self {
            There::Link(to, true) => mac::link(to.display()),
            There::Link(to, false) => mac::link_gone(to.display()),
            There::Folder => mac::folder().into(),
            There::File => mac::file().into(),
        }
    }
}

/// What is at [`COMMAND`], where something is.
fn there() -> Option<There> {
    let m = std::fs::symlink_metadata(COMMAND).ok()?;
    Some(match std::fs::read_link(COMMAND) {
        Ok(to) if m.file_type().is_symlink() => There::Link(to, std::fs::metadata(COMMAND).is_ok()),
        _ if m.is_dir() => There::Folder,
        _ => There::File,
    })
}

/// Links [`COMMAND`] to `aqr`, as an administrator (macOS asks for the password); what is there
/// already replaced only where the user agrees.
fn install_command(mtm: MainThreadMarker, aqr: &std::path::Path) {
    if let Some(t) = there() {
        if let There::Folder = t {
            let e = mac::command_folder(COMMAND);
            alert(mtm, NSAlertStyle::Critical, mac::command_not_installed(), &e, None);
            return;
        }
        let a = NSAlert::new(mtm);
        a.setAlertStyle(NSAlertStyle::Warning);
        a.setMessageText(&NSString::from_str(&common::is_there_already(COMMAND)));
        let info = mac::command_replace(t.what(), aqr.display());
        a.setInformativeText(&NSString::from_str(&info));
        a.addButtonWithTitle(&NSString::from_str(common::cancel()));
        a.addButtonWithTitle(&NSString::from_str(mac::replace()));
        activate(mtm);
        if a.runModal() == NSAlertFirstButtonReturn {
            return;
        }
    }
    let script = format!(
        "on run argv\ndo shell script \"mkdir -p /usr/local/bin && ln -sf \" & quoted form of item 1 of argv & \" \
         {COMMAND}\" with administrator privileges\nend run"
    );
    let out = std::process::Command::new("/usr/bin/osascript").arg("-e").arg(script).arg(aqr).output();
    match out {
        Ok(o) if o.status.success() => {
            let info = mac::command_runs(COMMAND, aqr.display());
            alert(mtm, NSAlertStyle::Informational, mac::command_installed(), &info, None);
        }
        // (cancelled at the password: error -128)
        Ok(o) if String::from_utf8_lossy(&o.stderr).contains("-128") => {}
        Ok(o) => {
            let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
            alert(mtm, NSAlertStyle::Critical, mac::command_not_installed(), &e, None);
        }
        Err(e) => alert(mtm, NSAlertStyle::Critical, mac::command_not_installed(), &e.to_string(), None),
    }
}
