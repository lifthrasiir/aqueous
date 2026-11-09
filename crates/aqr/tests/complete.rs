//! `aqr --setup-autocomplete=SHELL`: the script written and a line reading it added to the
//! startup file once, in a home of the test's own; the scripts of the shells found here
//! checked by them, bash's completing; the command taken alone.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-complete-{name}-{}", std::process::id()));
    _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// aqr run on `args` with `home` its home (nothing of the user's own): whether it succeeded,
/// its standard error.
fn aqr(home: &Path, args: &[&str]) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_aqr"))
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("LOCALAPPDATA", home.join("AppData/Local"))
        .env_remove("XDG_DATA_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("ZDOTDIR")
        .env("SHELL", "/bin/zsh")
        .args(args)
        .output()
        .unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stderr).into_owned())
}

/// Whether program `p` runs here.
#[cfg(unix)]
fn found(p: &str) -> bool {
    Command::new(p).arg("--version").output().is_ok_and(|o| o.status.success())
}

#[cfg(unix)]
#[test]
fn setup() {
    let home = scratch("setup");
    for shell in ["bash", "zsh"] {
        let (ok, err) = aqr(&home, &[&format!("--setup-autocomplete={shell}")]);
        assert!(ok, "{err}");
        // (again: the line not added twice)
        let (ok, err) = aqr(&home, &["--setup-autocomplete", shell]);
        assert!(ok && err.contains("reads it already"), "{err}");
        let script = home.join(format!(".local/share/aqr/completion.{shell}"));
        let rc = fs::read_to_string(home.join(format!(".{shell}rc"))).unwrap();
        assert_eq!(rc.matches(&*script.to_string_lossy()).count(), 2, "{rc}");
        assert_eq!(rc.lines().count(), 1, "{rc}");
        if found(shell) {
            let o = Command::new(shell).arg("-n").arg(&script).output().unwrap();
            assert!(o.status.success(), "{shell} -n: {}", String::from_utf8_lossy(&o.stderr));
        }
    }
    // $SHELL's by default
    fs::remove_file(home.join(".zshrc")).unwrap();
    let (ok, err) = aqr(&home, &["--setup-autocomplete"]);
    assert!(ok && home.join(".zshrc").is_file(), "{err}");
    let (ok, err) = aqr(&home, &["--setup-autocomplete=fish"]);
    assert!(ok, "{err}");
    let fish = home.join(".config/fish/completions");
    assert!(fs::read_to_string(fish.join("aqr.fish")).unwrap().contains("complete -c aqr -s o -l output"));
    assert!(fs::read_to_string(fish.join("unaqr.fish")).unwrap().contains("-w aqr"));
    if found("fish") {
        let o = Command::new("fish").arg("-n").arg(fish.join("aqr.fish")).output().unwrap();
        assert!(o.status.success(), "fish -n: {}", String::from_utf8_lossy(&o.stderr));
    }

    // bash completing: options, values after a space or =, files left to the shell
    if found("bash") {
        let script = home.join(".local/share/aqr/completion.bash");
        let complete = |words: &[&str]| {
            let words: Vec<String> = words.iter().map(|w| format!("'{w}'")).collect();
            let cmd = format!(
                ". '{}'; COMP_WORDS=({}); COMP_CWORD={}; _aqr; echo \"${{COMPREPLY[*]}}\"",
                script.display(),
                words.join(" "),
                words.len() - 1
            );
            let o = Command::new("bash").arg("-c").arg(cmd).output().unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8(o.stdout).unwrap().trim().to_owned()
        };
        assert_eq!(complete(&["aqr", "--for"]), "--format --force");
        assert_eq!(complete(&["aqr", "--format", "tar.x"]), "tar.xz");
        assert_eq!(complete(&["aqr", "--links", "=", "a"]), "all");
        assert_eq!(complete(&["aqr", "--normalize="]), "auto nfc nfd keep");
        assert_eq!(complete(&["aqr", "-o", "x"]), "");
        assert_eq!(complete(&["aqr", "x"]), "");
    }
    fs::remove_dir_all(&home).unwrap();
}

#[test]
fn alone() {
    let home = scratch("alone");
    for (args, told) in [
        (&["--setup-autocomplete=zsh", "x"][..], "a command alone"),
        (&["-q", "--setup-autocomplete=zsh"], "a command alone"),
        (&["--setup-autocomplete=tcsh"], "not a shell completed"),
    ] {
        let (ok, err) = aqr(&home, args);
        assert!(!ok && err.contains(told), "aqr {args:?}: {err}");
    }
    assert!(fs::read_dir(&home).unwrap().next().is_none(), "written in the home");
    fs::remove_dir_all(&home).unwrap();
}
