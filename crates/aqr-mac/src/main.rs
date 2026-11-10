//! Aqueous in Finder: services on files, folders and archives, and `.aqr` files opened, the work
//! done by `aqr --ipc` (`crates/aqr/src/ipc.rs`) in a process of its own and shown on its file
//! in Finder ([`app`]). Bundled as `Aqueous.app` by `bundle.sh`, `aqr` beside this program in
//! `Contents/MacOS` (else the one on `PATH`; `AQR` names another).

#[cfg(target_os = "macos")]
mod app;
#[cfg(target_os = "macos")]
mod job;
#[cfg(target_os = "macos")]
mod version;

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
    app::init_language();
    let app = NSApplication::sharedApplication(mtm);
    // (as `LSUIElement` has it, run outside the bundle too)
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let delegate = app::Delegate::new(mtm);
    app.setDelegate(Some(delegate.as_app_delegate()));
    app.run();
}

#[cfg(test)]
mod tests {
    /// The strings of Info.plist that Finder shows (the types' names, the services' items) are
    /// the English of aqr-l10n's `types` and `services`, which its `.strings` translate (keyed
    /// by them).
    #[test]
    fn info_plist_strings_translated() {
        let plist = include_str!("../Info.plist");
        let lines: Vec<&str> = plist.lines().map(str::trim).collect();
        let mut strings = Vec::new();
        for w in lines.windows(2) {
            let key = ["<key>UTTypeDescription</key>", "<key>CFBundleTypeName</key>", "<key>default</key>"];
            if key.contains(&w[0]) {
                strings.push(w[1].strip_prefix("<string>").and_then(|s| s.strip_suffix("</string>")).unwrap());
            }
        }
        assert_eq!(strings.len(), 15 + 15 + 6);
        let en: String = aqr_l10n::LPROJ.iter().filter(|(d, _, _)| *d == "en.lproj").map(|(_, _, s)| *s).collect();
        for s in strings {
            let s = s.replace("&amp;", "&");
            assert!(en.contains(&format!("\"{s}\" = ")), "{s} of Info.plist is not in aqr-l10n");
        }
    }
}
