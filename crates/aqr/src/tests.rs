use super::*;

/// A panic of the work ends the ticking too, and is passed up: the bar drawn (or a frontend
/// told) as a file panicked had aqr wait for ever.
#[test]
fn ticked_panic_ends() {
    let (told, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let r = std::panic::catch_unwind(|| ticked(true, &|| {}, || panic!("the work panics")));
        told.send(r.is_err()).unwrap();
    });
    let panicked = heard.recv_timeout(std::time::Duration::from_secs(20)).expect("a wait for ever");
    assert!(panicked);
}

/// Every developer option of the crates linked: of a well-formed name, declared once, its help
/// and its namespace's told.
#[test]
fn zopts() {
    let all = aqr_common::zopt::all();
    for o in &all {
        let name = match o.members() {
            // (a family's `PREFIX:WHAT`, and its members)
            Some(m) => {
                // (a module's name may hold a `/`: `exe-recons/x86`)
                let ok = |m: &str| aqr_common::zopt::well_formed(&format!("x:{}", m.replace('/', "-")));
                m.iter().for_each(|m| assert!(ok(m), "{}: {m}", o.name()));
                o.name().split(':').next().unwrap()
            }
            None => o.name(),
        };
        assert!(aqr_common::zopt::well_formed(name), "-Z {}: not a well-formed name", o.name());
        assert!(!o.help().is_empty() && !o.help().ends_with('.'), "-Z {}: help", o.name());
    }
    for w in all.windows(2) {
        assert_ne!(w[0].name(), w[1].name(), "declared twice");
    }
    assert!(aqr_common::zopt::help("").unwrap().contains("[no-]sandbox"));
    for (ns, ..) in aqr_common::zopt::namespaces() {
        aqr_common::zopt::help(ns).unwrap();
    }
}

/// The options `--help` tells of, as given (`-x`, `--long`).
pub(crate) fn help_options() -> Vec<String> {
    let mut out = Vec::new();
    for line in HELP.lines() {
        // (the lines of options: indented by 2 to 6, the option first, its words to two spaces)
        let indent = line.len() - line.trim_start().len();
        if !(2..=6).contains(&indent) || !line.trim_start().starts_with('-') {
            continue;
        }
        let column = line.trim_start().split("  ").next().unwrap();
        for word in column.split([' ', ',']) {
            let name = word.split('[').next().unwrap();
            if name.len() < 2 || !name.starts_with('-') || name == "..." {
                continue;
            }
            out.push(name.to_owned());
        }
    }
    out
}

/// Every option `--help` tells of is in the manual page (`aqr.1`), as mdoc's `Fl`.
#[test]
fn man_page_has_every_option() {
    let man = include_str!("../aqr.1");
    let missing: Vec<String> =
        help_options().into_iter().filter(|o| !man.contains(&format!("Fl {}", &o[1..]))).collect();
    assert!(missing.is_empty(), "not in aqr.1: {missing:?}");
}

/// `-M`: by default 2 GB but at most half the system's; `avail` most of what is available,
/// but not too little; a share of the system's; a size; none.
#[test]
fn memory() {
    const G: u64 = 1 << 30;
    let (sys, unknown) = ((Some(16 * G), Some(10 * G)), (None, None));
    assert_eq!(budget(None, sys), Ok(aqr_common::budget::DEFAULT));
    assert_eq!(budget(None, (Some(2 * G), Some(G))), Ok(G));
    assert_eq!(budget(None, unknown), Ok(aqr_common::budget::DEFAULT));
    assert_eq!(budget(Some("avail"), sys), Ok(8 * G));
    assert_eq!(budget(Some("avail"), (Some(16 * G), Some(G / 2))), Ok(G));
    assert_eq!(budget(Some("avail"), (Some(2 * G), Some(0))), Ok(G / 2));
    assert_eq!(budget(Some("avail"), unknown), Ok(aqr_common::budget::DEFAULT));
    assert_eq!(budget(Some("25%"), sys), Ok(4 * G));
    assert!(budget(Some("25%"), unknown).is_err());
    assert!(budget(Some("0%"), sys).is_err() && budget(Some("101%"), sys).is_err());
    assert_eq!(budget(Some("3G"), sys), Ok(3 * G));
    assert_eq!(budget(Some("0"), sys), Ok(u64::MAX));
    assert!(budget(Some("lots"), sys).is_err());
}

/// An archive is compressed with a spill beside it, but an encrypted one with none: nothing
/// of its contents written out as it is.
#[test]
fn no_spill_sealed() {
    let out = std::env::temp_dir().join("x.aqr");
    assert!(spill_of(&out, false).unwrap().is_some_and(|p| p.ends_with(".x.aqr")));
    assert_eq!(spill_of(&out, true), Ok(None));
}
