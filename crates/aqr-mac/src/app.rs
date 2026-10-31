//! The app: an agent with no window and no Dock icon (`LSUIElement`), its work given by Finder
//! and shown in Finder.
//!
//! - Finder's services (`NSServices` in `Info.plist`, under Quick Actions or Services of a
//!   Control-click) send the paths chosen, all at once, on a pasteboard: "Compress with Aqueous"
//!   on files and folders, "Extract with Aqueous" and "Test with Aqueous" on `.aqr` files.
//!   `.aqr` files opened (a double-click) are extracted.
//! - How far a job is shows on its file in Finder: an `NSProgress` published for it, a bar on
//!   the file's icon as of a file downloading, with a button that stops the job (`aqr`'s
//!   output being written removed). The file is the archive being written (`NAME.PID.part`),
//!   the folder being extracted into, or the archive being tested.
//! - Alerts tell errors (with the last lines `aqr` wrote), what a test checked, and ask about an
//!   output there already.
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
    NSPasteboardTypeFileURL, NSScrollView, NSTextView, NSUpdateDynamicServices,
};
use objc2_foundation::{
    NSArray, NSBundle, NSNotification, NSNumber, NSObject, NSObjectProtocol, NSPoint, NSProgress,
    NSProgressFileOperationKindCopying, NSProgressFileOperationKindDecompressingAfterDownloading, NSProgressKindFile,
    NSRect, NSSize, NSString, NSTimer, NSURL,
};
use serde_json::{Value, json};

use crate::job::{Job, Verb, file_name, tested};

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
                paths.into_iter().partition(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("aqr")));
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
        if paths.is_empty() {
            return;
        }
        self.ivars().worked.set(true);
        match Job::start(verb, paths) {
            Ok(job) => self.ivars().jobs.borrow_mut().push(Running { job, progress: None, always: Vec::new() }),
            Err(e) => {
                alert(self.mtm(), NSAlertStyle::Critical, &format!("Could not {} anything", verb.name()), &e, None)
            }
        }
    }

    /// Looks at the jobs: their progress shown, their questions asked, those done told of.
    fn tick(&self) {
        let mtm = self.mtm();
        // (taken out: an alert's loop may take messages that begin jobs)
        let jobs = std::mem::take(&mut *self.ivars().jobs.borrow_mut());
        let mut left = Vec::new();
        for mut r in jobs {
            let question = r.job.state.lock().unwrap().questions.pop_front();
            if let Some(q) = question {
                let answer = ask(mtm, &mut r, &q);
                r.job.send(&json!({"answer": q["ask"], "value": answer}));
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
            p.setFileOperationKind(Some(match job.verb {
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
    let f = job.share();
    p.setCompletedUnitCount((f * UNITS as f64) as i64);
    let since = job.state.lock().unwrap().since;
    if let Some(t) = since.map(|s| s.elapsed().as_secs_f64())
        && f >= 0.02
        && t >= 3.0
    {
        p.setEstimatedTimeRemaining(Some(&NSNumber::new_f64(t * (1.0 - f) / f)));
    }
}

/// Tells what came of `job`, ended with `status`: an error, or what a test checked.
fn finished(mtm: MainThreadMarker, job: &Job, status: std::process::ExitStatus) {
    let s = job.state.lock().unwrap();
    if s.stopped {
        return;
    }
    let failed = match &s.error {
        Some(e) => Some(e.clone()),
        None if !status.success() => Some(format!("aqr ended with {status}.")),
        None => None,
    };
    let name = if s.begun > 0 { file_name(&s.input) } else { String::new() };
    if let Some(e) = failed {
        let main = match name.as_str() {
            "" => format!("Could not {}", job.verb.name()),
            n => format!("Could not {} {n}", job.verb.name()),
        };
        let log: Vec<&str> = s.log.iter().map(String::as_str).collect();
        let log = log.join("\n");
        alert(mtm, NSAlertStyle::Critical, &main, &e, (!log.is_empty()).then_some(&log));
    } else if job.verb == Verb::Test {
        let lines: Vec<String> = s.ended.iter().map(|(n, e)| tested(n, e, job.count > 1)).collect();
        let main =
            if job.count == 1 { format!("{name} is intact") } else { format!("The {} archives are intact", job.count) };
        alert(mtm, NSAlertStyle::Informational, &main, &lines.join("\n"), None);
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
            a.setMessageText(&NSString::from_str(&format!("{} is there already", file_name(&path))));
            let (info, yes) = if path.is_dir() {
                ("Extract into the folder all the same? Its files of the same names are replaced.", "Extract")
            } else {
                ("Replace the file?", "Replace")
            };
            a.setInformativeText(&NSString::from_str(&format!("{}\n\n{info}", path.display())));
            a.addButtonWithTitle(&NSString::from_str(yes));
            a.addButtonWithTitle(&NSString::from_str("Skip"));
            if r.job.count > 1 {
                a.setShowsSuppressionButton(true);
                if let Some(b) = a.suppressionButton() {
                    b.setTitle(&NSString::from_str("Do the same for the other archives"));
                }
            }
            activate(mtm);
            let answer = Value::Bool(a.runModal() == NSAlertFirstButtonReturn);
            if a.suppressionButton().is_some_and(|b| b.state() != 0) {
                r.always.push((kind.into(), answer.clone()));
            }
            answer
        }
        _ => Value::Null,
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
        let info = "macOS runs it from a copy of its own as long as it is where it was downloaded. Move \
                    Aqueous to the Applications folder in Finder, and open it from there.";
        alert(mtm, NSAlertStyle::Warning, "Aqueous is to be moved first", info, None);
        return;
    }
    let url = bundle.bundleURL();
    // SAFETY: an NSURL, toll-free bridged to CFURL
    let status = unsafe { LSRegisterURL(Retained::as_ptr(&url).cast(), 1) };
    NSUpdateDynamicServices();
    if status != 0 {
        let e = format!("Launch Services did not register {path} (error {status}).");
        alert(mtm, NSAlertStyle::Critical, "Aqueous is not registered", &e, None);
        return;
    }
    let aqr = std::env::current_exe().ok().map(|e| e.with_file_name("aqr")).filter(|p| p.is_file());
    let linked = aqr.as_ref().is_some_and(|a| std::fs::canonicalize(COMMAND).ok() == std::fs::canonicalize(a).ok());
    let a = NSAlert::new(mtm);
    a.setMessageText(&NSString::from_str("Aqueous is ready"));
    let mut info = String::from(
        "In Finder, Control-click files or folders for \"Compress with Aqueous\", and .aqr files for \"Extract \
         with Aqueous\" and \"Test with Aqueous\" (under Quick Actions or Services). Opening an .aqr file \
         extracts it beside it.\n\nWhere they are not there, turn them on in System Settings: Keyboard, \
         Keyboard Shortcuts, Services, Files and Folders.",
    );
    if aqr.is_some() && !linked {
        info += &match there() {
            None => format!("\n\nThe aqr command can be put at {COMMAND} (as an administrator)."),
            Some(t) => format!("\n\n{COMMAND} is {t}, not this app's aqr."),
        };
    }
    a.setInformativeText(&NSString::from_str(&info));
    a.addButtonWithTitle(&NSString::from_str("OK"));
    if let (Some(aqr), false) = (&aqr, linked) {
        a.addButtonWithTitle(&NSString::from_str("Install the aqr Command"));
        activate(mtm);
        if a.runModal() != NSAlertFirstButtonReturn {
            install_command(mtm, aqr);
        }
    } else {
        activate(mtm);
        a.runModal();
    }
}

/// What is at [`COMMAND`], where something is: a link (to where) or a file.
fn there() -> Option<String> {
    let m = std::fs::symlink_metadata(COMMAND).ok()?;
    Some(match std::fs::read_link(COMMAND) {
        Ok(to) if m.file_type().is_symlink() => {
            let gone = if std::fs::metadata(COMMAND).is_err() { ", which is not there" } else { "" };
            format!("a link to {}{gone}", to.display())
        }
        _ if m.is_dir() => "a folder".into(),
        _ => "a file".into(),
    })
}

/// Links [`COMMAND`] to `aqr`, as an administrator (macOS asks for the password); what is there
/// already replaced only where the user agrees.
fn install_command(mtm: MainThreadMarker, aqr: &std::path::Path) {
    if let Some(t) = there() {
        if t == "a folder" {
            let e = format!("{COMMAND} is a folder; it is left as it is.");
            alert(mtm, NSAlertStyle::Critical, "The aqr command is not installed", &e, None);
            return;
        }
        let a = NSAlert::new(mtm);
        a.setAlertStyle(NSAlertStyle::Warning);
        a.setMessageText(&NSString::from_str(&format!("{COMMAND} is there already")));
        let info = format!("It is {t}. Replace it with a link to {}?", aqr.display());
        a.setInformativeText(&NSString::from_str(&info));
        a.addButtonWithTitle(&NSString::from_str("Cancel"));
        a.addButtonWithTitle(&NSString::from_str("Replace"));
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
            let info = format!("{COMMAND} runs {}.", aqr.display());
            alert(mtm, NSAlertStyle::Informational, "The aqr command is installed", &info, None);
        }
        // (cancelled at the password: error -128)
        Ok(o) if String::from_utf8_lossy(&o.stderr).contains("-128") => {}
        Ok(o) => {
            let e = String::from_utf8_lossy(&o.stderr).trim().to_string();
            alert(mtm, NSAlertStyle::Critical, "The aqr command is not installed", &e, None);
        }
        Err(e) => alert(mtm, NSAlertStyle::Critical, "The aqr command is not installed", &e.to_string(), None),
    }
}
