//! Aqueous in Finder: services on files, folders and archives, and `.aqr` files opened, the work
//! done by `aqr --ipc` (`crates/aqr/src/ipc.rs`) in a process of its own and shown on its file
//! in Finder ([`app`]). Bundled as `Aqueous.app` by `bundle.sh`, `aqr` beside this program in
//! `Contents/MacOS` (else the one on `PATH`; `AQR` names another).

#[cfg(target_os = "macos")]
mod app;
#[cfg(target_os = "macos")]
mod job;

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("aqr-mac: for macOS only");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn main() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    let mtm = MainThreadMarker::new().expect("on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    // (as `LSUIElement` has it, run outside the bundle too)
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let delegate = app::Delegate::new(mtm);
    app.setDelegate(Some(delegate.as_app_delegate()));
    app.run();
}
