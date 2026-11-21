//! Self-extracting programs made by `aqr -c --format sfx` (Windows x86-64): run, they write the
//! files out as they were; and the modules run as WebAssembly by the pack's interpreter blob.
//! The aqr built has the pack of `target/aqr-sfx/` (`AQR_SFX_PACK`'s where that is set;
//! `crates/aqr-sfx/pack.py` makes it) written into a copy of it, as the release does; the tests
//! fail where there is none.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    common::scratch("sfx", name)
}

/// The aqr built, with the pack written into a copy of it where there is one.
fn aqr() -> &'static Path {
    static AQR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    AQR.get_or_init(|| {
        let exe = PathBuf::from(env!("CARGO_BIN_EXE_aqr"));
        #[cfg(windows)]
        {
            // (the target directory: the exe's is that of its profile)
            let target = exe.parent().and_then(Path::parent).unwrap();
            let dir = std::env::var_os("AQR_SFX_PACK").map_or_else(|| target.join("aqr-sfx"), PathBuf::from);
            if let (Ok(pack), Ok(id)) = (fs::read(dir.join("sfx.pack")), fs::read_to_string(dir.join("pack.id"))) {
                let copy = scratch("aqr").join("aqr.exe");
                fs::copy(&exe, &copy).unwrap();
                aqr_sfx::resource::embed(&copy, &pack, id.trim()).unwrap();
                return copy;
            }
        }
        exe
    })
}

fn aqr_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(aqr()).current_dir(dir).args(args).output().unwrap()
}

/// What needs the pack (Windows x86-64): the tests are here, so that a run without it (CI's:
/// the release workflow builds the pack and tries an SFX of it) can leave them out by their
/// path (`packed::`, .config/nextest.toml).
#[cfg(all(windows, target_arch = "x86_64"))]
mod packed {
    use super::*;

    /// Text of words (which aqr-wrt recompresses, so that the archive has a module).
    fn prose(n: usize) -> Vec<u8> {
        let words = ["the", "archive", "of", "files", "written", "out", "again", "when", "run", "and", "kept"];
        let mut t = Vec::new();
        let mut x = 7u32;
        while t.len() < n {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            t.extend_from_slice(words[(x >> 16) as usize % words.len()].as_bytes());
            t.push(if x.is_multiple_of(13) { b'\n' } else { b' ' });
        }
        t
    }

    /// Makes `out` (`--format sfx`) of `inputs` in `dir` (the aqr tested carrying the pack).
    fn made(dir: &Path, inputs: &[&str], out: &str) {
        let mut args = vec!["-c", "--format", "sfx", "-o", out];
        args.extend_from_slice(inputs);
        let o = aqr_in(dir, &args);
        let err = String::from_utf8_lossy(&o.stderr);
        let pack = "(the pack: crates/aqr-sfx/pack.py, which `cargo aqr` runs, or AQR_SFX_PACK)";
        assert!(o.status.success(), "aqr {args:?}: {err} {pack}");
        assert!(err.contains("self-extracting"), "{err}");
    }

    /// Runs self-extracting program `exe` in `dir`, writing the files under `to`.
    fn run(dir: &Path, exe: &str, to: &str) {
        let o = Command::new(dir.join(exe)).current_dir(dir).arg(to).output().unwrap();
        assert!(o.status.success(), "{exe}: {}", String::from_utf8_lossy(&o.stderr));
    }

    #[test]
    fn sfx_of_a_directory() {
        let d = scratch("dir");
        fs::create_dir_all(d.join("in/sub")).unwrap();
        fs::write(d.join("in/a.txt"), prose(200_000)).unwrap();
        fs::write(d.join("in/sub/b.bin"), (0..5000u32).map(|i| (i * 31 % 251) as u8).collect::<Vec<_>>()).unwrap();
        fs::write(d.join("in/sub/empty"), b"").unwrap();
        made(&d, &["in"], "in.exe");
        run(&d, "in.exe", "out");
        for f in ["a.txt", "sub/b.bin", "sub/empty"] {
            assert!(fs::read(d.join("in").join(f)).unwrap() == fs::read(d.join("out").join(f)).unwrap(), "{f}");
        }
        fs::remove_dir_all(&d).unwrap();
    }

    /// A single file is in the program by its name (not as an archive of a single file, which has none).
    #[test]
    fn sfx_of_a_file() {
        let d = scratch("file");
        fs::write(d.join("a.txt"), prose(100_000)).unwrap();
        made(&d, &["a.txt"], "a.exe");
        run(&d, "a.exe", "out");
        assert!(fs::read(d.join("a.txt")).unwrap() == fs::read(d.join("out/a.txt")).unwrap());
        fs::remove_dir_all(&d).unwrap();
    }

    /// The Windows SDK's `signtool` (`AQR_SIGNTOOL`, else the latest of the SDK's x64 tools).
    fn signtool() -> Option<PathBuf> {
        if let Some(t) = std::env::var_os("AQR_SIGNTOOL") {
            return Some(t.into());
        }
        let bin = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
        let mut found: Vec<PathBuf> = fs::read_dir(bin)
            .ok()?
            .flatten()
            .map(|e| e.path().join(r"x64\signtool.exe"))
            .filter(|p| p.exists())
            .collect();
        found.sort();
        found.pop()
    }

    /// A program signed once it is made, by `--run-after` (with a certificate of its own, made by
    /// OpenSSL: no store of the system's touched), runs as one unsigned does, and aqr reads its
    /// archive (bytes before it and after it, which locating does not take: the signature is left
    /// out first). Where `signtool` or `openssl` is not there, told and passed.
    #[test]
    fn signed_afterwards() {
        let Some(signtool) = signtool() else {
            eprintln!("signed_afterwards: no signtool (the Windows SDK), not tried");
            return;
        };
        let d = scratch("signed");
        let openssl = |args: &[&str]| Command::new("openssl").current_dir(&d).args(args).output();
        let made_cert = openssl(&[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-keyout",
            "k.pem",
            "-out",
            "c.pem",
            "-days",
            "1",
            "-nodes",
            "-subj",
            "/CN=aqr test",
            "-addext",
            "extendedKeyUsage=codeSigning",
        ]);
        if !made_cert.is_ok_and(|o| o.status.success()) {
            eprintln!("signed_afterwards: no openssl to make a certificate, not tried");
            return;
        }
        let o =
            openssl(&["pkcs12", "-export", "-out", "t.pfx", "-inkey", "k.pem", "-in", "c.pem", "-passout", "pass:x"]);
        assert!(o.unwrap().status.success());
        fs::create_dir_all(d.join("in")).unwrap();
        fs::write(d.join("in/a.txt"), prose(200_000)).unwrap();
        fs::write(d.join("in/b.bin"), (0..5000u32).map(|i| (i * 7 % 253) as u8).collect::<Vec<_>>()).unwrap();
        let sign = format!("\"{}\" sign /q /fd sha256 /f t.pfx /p x \"%AQR_OUTPUT%\"", signtool.display());
        let o = aqr_in(&d, &["-c", "-q", "--format", "sfx", "-o", "in.exe", "--run-after", &sign, "in"]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let signed = fs::read(d.join("in.exe")).unwrap();
        assert!(aqr_archive::pe_certificate_table(&signed).is_some(), "a signature");
        // the system's check of it: its hash holds (its root is not trusted)
        let ps = format!("(Get-AuthenticodeSignature -LiteralPath '{}').Status", d.join("in.exe").display());
        let status = Command::new("powershell").args(["-NoProfile", "-Command", &ps]).output().unwrap();
        let status = String::from_utf8_lossy(&status.stdout).trim().to_string();
        assert!(!["NotSigned", "HashMismatch", "NotSupportedFileFormat", ""].contains(&status.as_str()), "{status}");
        // run, and read by aqr
        run(&d, "in.exe", "out");
        let o = aqr_in(&d, &["-d", "-o", "back", "in.exe"]);
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success() && err.contains("its signature"), "{err}");
        for out in ["out", "back"] {
            for f in ["a.txt", "b.bin"] {
                assert!(fs::read(d.join("in").join(f)).unwrap() == fs::read(d.join(out).join(f)).unwrap(), "{out}/{f}");
            }
        }
        let o = aqr_in(&d, &["-t", "-q", "in.exe"]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        fs::remove_dir_all(&d).unwrap();
    }

    /// An icon of one image (a PNG by its signature, all that is checked of it).
    fn icon() -> Vec<u8> {
        let png = b"\x89PNG\r\n\x1a\n and the rest";
        let mut ico = vec![0, 0, 1, 0, 1, 0, 32, 32, 0, 0, 1, 0, 32, 0];
        ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
        ico.extend_from_slice(&22u32.to_le_bytes());
        ico.extend_from_slice(png);
        ico
    }

    /// A program of the settings of `.aqr/sfx/` (docs/sfx-config-design.md): its icon, version
    /// and subsystem in its image, run (cmd.exe of the archive: its exit code the program's, the
    /// files written to a new temporary directory and removed once it ends); and the settings
    /// kept in the archive as files, which make the same program again.
    #[test]
    fn sfx_of_settings() {
        let d = scratch("settings");
        fs::create_dir_all(d.join("in/.aqr/sfx")).unwrap();
        fs::create_dir_all(d.join("in/bin")).unwrap();
        let cmd = fs::read(std::env::var("SystemRoot").unwrap() + r"\System32\cmd.exe").unwrap();
        fs::write(d.join("in/bin/cmd.exe"), cmd).unwrap();
        fs::write(d.join("in/bin/data.txt"), prose(50_000)).unwrap();
        fs::write(d.join("in/.aqr/sfx/icon.ico"), icon()).unwrap();
        let copied = d.join("copied.txt");
        let toml = format!(
            "mode = \"run\"\ntitle = \"Example\"\n\n[run]\nprogram = [\"bin/cmd.exe\", \"/c\", \"copy\", \"data.txt\", {:?}, \"&\", \"exit\", \"7\"]\ncwd = \"bin\"\nwindow = \"hidden\"\n\n[pe]\nicon = \"icon.ico\"\nsubsystem = \"gui\"\nversion = \"1.2.3\"\nproduct = \"Example\"\ncompany = \"Example Inc.\"\n",
            copied.display().to_string()
        );
        fs::write(d.join("in/.aqr/sfx/sfx.toml"), &toml).unwrap();
        made(&d, &["in"], "setup.exe");
        let exe = fs::read(d.join("setup.exe")).unwrap();
        let res = aqr_sfx::pe::resources(&exe).unwrap();
        let kinds: Vec<_> = res.iter().map(|r| r.kind.clone()).collect();
        use aqr_sfx::pe::Id::Num;
        assert_eq!(kinds, [Num(3), Num(14), Num(16), Num(24)]);
        // the files of .aqr/sfx/ in the first segment, alone (docs/sfx-config-design.md, 2.1)
        let c = &exe[aqr_archive::locate(&exe).unwrap()];
        let all = aqr_archive::open(c, &aqr_archive::NoModules).unwrap();
        let segments = all.segment_metadata().unwrap();
        assert_eq!(segments.len(), 2);
        let first = aqr_archive::open_upto(c, &aqr_archive::NoModules, segments[0]).unwrap();
        let t = first.tree().unwrap();
        let files: Vec<_> = (t.entries.iter().enumerate())
            .filter(|(_, e)| matches!(e.node, aqr_archive::EntryNode::File(_)))
            .map(|(i, _)| aqr_sfx::config::path_of(t, i, None))
            .collect();
        assert_eq!(files, [".aqr/sfx/icon.ico", ".aqr/sfx/sfx.toml"]);
        let pe = u32::from_le_bytes(exe[0x3c..0x40].try_into().unwrap()) as usize;
        assert_eq!(u16::from_le_bytes([exe[pe + 24 + 68], exe[pe + 25 + 68]]), 2, "of windows");

        let tmp = d.join("tmp");
        fs::create_dir_all(&tmp).unwrap();
        let o = Command::new(d.join("setup.exe")).current_dir(&d).env("TMP", &tmp).env("TEMP", &tmp).output().unwrap();
        assert_eq!(o.status.code(), Some(7), "{}", String::from_utf8_lossy(&o.stderr));
        assert!(fs::read(&copied).unwrap() == fs::read(d.join("in/bin/data.txt")).unwrap());
        assert_eq!(fs::read_dir(&tmp).unwrap().count(), 0, "the files written removed");

        // the archive's .aqr/sfx/ as it was, and the program made of it again the same (up to the
        // archive: its root's times may differ)
        let o = aqr_in(&d, &["-d", "-q", "-o", "out", "setup.exe"]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(fs::read_to_string(d.join("out/.aqr/sfx/sfx.toml")).unwrap(), toml);
        made(&d, &["out"], "again.exe");
        let again = fs::read(d.join("again.exe")).unwrap();
        let (at, at_again) = (aqr_archive::locate(&exe).unwrap().start, aqr_archive::locate(&again).unwrap().start);
        assert!(exe[..at] == again[..at_again]);
        fs::remove_dir_all(&d).unwrap();
    }

    /// Settings not right fail when the program is made, none made.
    #[test]
    fn settings_checked() {
        let d = scratch("checked");
        fs::create_dir_all(d.join("in/.aqr/sfx")).unwrap();
        fs::write(d.join("in/a.txt"), prose(1000)).unwrap();
        for (toml, told) in [
            ("tilte = \"x\"\n", "tilte: not a setting known"),
            ("[run]\nprogram = [\"setup.exe\"]\n", "run.program: setup.exe: not a file of the archive"),
            ("[pe]\nicon = \"icon.ico\"\n", "pe.icon: icon.ico: not a file of .aqr/sfx/"),
        ] {
            fs::write(d.join("in/.aqr/sfx/sfx.toml"), toml).unwrap();
            let o = aqr_in(&d, &["-c", "--format", "sfx", "-o", "x.exe", "in"]);
            let err = String::from_utf8_lossy(&o.stderr);
            assert!(!o.status.success() && err.contains(told), "{toml}: {err}");
            assert!(!d.join("x.exe").exists());
        }
        fs::remove_dir_all(&d).unwrap();
    }

    /// The modules run as WebAssembly (`-Z wasm`) by the interpreter blob of the pack, as aqr.exe
    /// links no Wasmtime: an archive of a module (aqr-wrt's) tested and extracted as it was.
    #[test]
    fn wasm_through_the_interpreter() {
        let d = scratch("interp");
        fs::create_dir_all(d.join("in")).unwrap();
        fs::write(d.join("in/a.txt"), prose(300_000)).unwrap();
        fs::write(d.join("in/b.txt"), prose(5_000)).unwrap();
        let o = aqr_in(&d, &["-c", "-q", "-o", "a.aqr", "in"]);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let pack = "(the pack: crates/aqr-sfx/pack.py, which `cargo aqr` runs, or AQR_SFX_PACK)";
        for args in [&["-K", "-Z", "wasm", "a.aqr"][..], &["-d", "-q", "-Z", "wasm", "-o", "out", "a.aqr"]] {
            let o = aqr_in(&d, args);
            assert!(o.status.success(), "aqr {args:?}: {} {pack}", String::from_utf8_lossy(&o.stderr));
        }
        for f in ["a.txt", "b.txt"] {
            assert!(fs::read(d.join("in").join(f)).unwrap() == fs::read(d.join("out").join(f)).unwrap(), "{f}");
        }
        // (and a module it ran: the archive of text has aqr-wrt's)
        let list = String::from_utf8_lossy(&aqr_in(&d, &["-t", "-v", "a.aqr"]).stdout).into_owned();
        assert!(list.contains("wrt"), "{list}");
        fs::remove_dir_all(&d).unwrap();
    }
}

/// `.aqr/sfx/` archived but not into a self-extracting program: told, and kept as files.
#[test]
fn settings_not_of_a_program() {
    let d = scratch("not-sfx");
    fs::create_dir_all(d.join("in/.aqr/sfx")).unwrap();
    fs::write(d.join("in/.aqr/sfx/sfx.toml"), "title = \"x\"\n").unwrap();
    let o = aqr_in(&d, &["-c", "-o", "a.aqr", "in"]);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success() && err.contains(".aqr/sfx/: archived as files"), "{err}");
    let o = aqr_in(&d, &["-d", "-q", "-o", "out", "a.aqr"]);
    assert!(o.status.success() && d.join("out/.aqr/sfx/sfx.toml").exists());
    // (not of the root: not told)
    let o = aqr_in(&d, &["-c", "-o", "b.aqr", "in", "out"]);
    assert!(!String::from_utf8_lossy(&o.stderr).contains(".aqr/sfx/"));
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn sfx_not_with() {
    let d = scratch("not");
    fs::write(d.join("a.txt"), b"text").unwrap();
    for (flag, what) in [("-P", "-P"), ("-A", "-A"), ("-a", "-a")] {
        let o = aqr_in(&d, &["-c", flag, "--format", "sfx", "-o", "a.exe", "a.txt"]);
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(!o.status.success() && err.contains(&format!("--format sfx: not with {what}")), "{flag}: {err}");
    }
    let o = aqr_in(&d, &["-c", "--format", "sfx", "-o", "-", "a.txt"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("not with standard input or output"));
    fs::remove_dir_all(&d).unwrap();
}
