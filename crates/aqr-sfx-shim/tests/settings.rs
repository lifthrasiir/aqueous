//! The stub run with settings ([`aqr_sfx::config`]) before its archive: where the files go,
//! what is done with files there, the program run once they are written.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use aqr_archive::{Encoder, Entry, EntryNode, Extra, Leaf, Level, Segment, Tree};
use aqr_formula::Formula;
use aqr_sfx::config::{Config, Overwrite, Run, Text, Window};

/// An archive of `files` (each its path by `/` and its bytes; the directories made of their
/// paths), stored.
fn archive(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut f = Formula::default();
    let mut leaves = Vec::new();
    let mut entries: Vec<Entry> = Vec::new();
    let mut contents = Vec::new();
    for (i, (path, data)) in files.iter().enumerate() {
        let n = f.push("x", data.clone());
        leaves.push(Leaf { outputs: vec![n], sums: vec![None] });
        contents.push((i, 0));
        let mut parent = None;
        let parts: Vec<&str> = path.split('/').collect();
        for (k, part) in parts.iter().enumerate() {
            let last = k == parts.len() - 1;
            let at = entries.iter().position(|e| e.parent == parent && e.name == part.as_bytes() && !last);
            parent = Some(at.unwrap_or_else(|| {
                let node = if last { EntryNode::File(i) } else { EntryNode::Dir };
                entries.push(Entry { parent, name: part.as_bytes().to_vec(), node, extra: Extra::default() });
                entries.len() - 1
            }));
        }
    }
    let tree = Tree { entries, ..Tree::default() };
    let seg = Segment { f: &f, leaves: &leaves, tree: &tree, contents: &contents };
    Encoder::new(Level::STORED).encode_segment(&seg).unwrap().0
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-sfx-settings-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The stub with `tail` appended, as `name` in `d`.
fn program(d: &Path, name: &str, tail: &[u8]) -> PathBuf {
    let mut exe = std::fs::read(env!("CARGO_BIN_EXE_aqr-sfx-shim")).unwrap();
    exe.extend_from_slice(tail);
    let path = d.join(if cfg!(windows) { format!("{name}.exe") } else { name.into() });
    std::fs::write(&path, exe).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

/// Runs `exe` in `d` with `args`, `input` its standard input, the temporary directory `d/tmp`,
/// the user's language English.
fn run(d: &Path, exe: &Path, args: &[&str], input: &str) -> Output {
    run_in(d, exe, args, input, "en")
}

/// [`run`] of the user's language `lang`.
fn run_in(d: &Path, exe: &Path, args: &[&str], input: &str, lang: &str) -> Output {
    let tmp = d.join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let mut c = Command::new(exe);
    c.current_dir(d).args(args).env("TMP", &tmp).env("TEMP", &tmp).env("TMPDIR", &tmp).env("AQR_SFX_LANG", lang);
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), input.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn extracting(dest: &str, subdir: bool, overwrite: Overwrite) -> Config {
    Config { dest: dest.into(), subdir, overwrite, ..Config::default() }
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn where_the_files_go() {
    let d = scratch("dest");
    let a = archive(&[("a.txt", b"a".to_vec()), ("sub/b.txt", b"b".to_vec())]);
    let tail = [extracting("{here}/out", true, Overwrite::Always).record(), a.clone()].concat();
    let exe = program(&d, "pkg", &tail);
    let o = run(&d, &exe, &[], "");
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(std::fs::read(d.join("out/pkg/sub/b.txt")).unwrap(), b"b");
    // (the command line's destination before the settings')
    let o = run(&d, &exe, &["given"], "");
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(std::fs::read(d.join("given/a.txt")).unwrap(), b"a");

    // the settings right before an archive found by scanning (other bytes before them)
    let tail = [b"not a bundle".to_vec(), extracting("{cwd}/scanned", false, Overwrite::Ask).record(), a].concat();
    let exe = program(&d, "scan", &tail);
    let o = run(&d, &exe, &[], "");
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(std::fs::read(d.join("scanned/a.txt")).unwrap(), b"a");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn files_there() {
    let d = scratch("there");
    let a = archive(&[("a.txt", b"new a".to_vec()), ("b.txt", b"new b".to_vec())]);
    let there = || {
        let out = d.join("out");
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("a.txt"), "old a").unwrap();
        std::fs::write(out.join("b.txt"), "old b").unwrap();
    };
    let read = |n: &str| std::fs::read_to_string(d.join("out").join(n)).unwrap();
    for (overwrite, args, input, want) in [
        (Overwrite::Never, &[][..], "", ["old a", "old b"]),
        (Overwrite::Always, &[], "", ["new a", "new b"]),
        // asked: no for the first, yes for the second (a word not known asked again)
        (Overwrite::Ask, &[], "n\nperhaps\ny\n", ["old a", "new b"]),
        (Overwrite::Ask, &[], "a\n", ["new a", "new b"]),
        (Overwrite::Ask, &["-y"], "", ["new a", "new b"]),
    ] {
        there();
        let exe = program(&d, "x", &[extracting("{cwd}/out", false, overwrite).record(), a.clone()].concat());
        let o = run(&d, &exe, args, input);
        assert!(o.status.success(), "{overwrite:?}: {}", err(&o));
        assert_eq!([read("a.txt"), read("b.txt")], want.map(String::from), "{overwrite:?} {input:?}");
    }
    // asked, and stopped (or no answer: standard input ended)
    for input in ["q\n", ""] {
        there();
        let exe = program(&d, "x", &[extracting("{cwd}/out", false, Overwrite::Ask).record(), a.clone()].concat());
        let o = run(&d, &exe, &[], input);
        assert!(!o.status.success() && err(&o).contains("stopped"), "{input:?}: {}", err(&o));
        assert_eq!(read("a.txt"), "old a");
    }
    // with no settings, a file there fails
    there();
    let o = run(&d, &program(&d, "x", &a), &["out"], "");
    assert!(!o.status.success() && err(&o).contains("not written over"), "{}", err(&o));
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn asked_first() {
    let d = scratch("prompt");
    let a = archive(&[("a.txt", b"a".to_vec())]);
    let c = Config {
        prompt: Some(Text(vec![("en".into(), "Extract?".into()), ("ko".into(), "풀까요?".into())])),
        finish: Some(Text::plain("All done.")),
        ..extracting("{cwd}/out", false, Overwrite::Always)
    };
    let exe = program(&d, "x", &[c.record(), a].concat());
    let o = run(&d, &exe, &[], "n\n");
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(!d.join("out").exists());
    let o = run(&d, &exe, &[], "y\n");
    assert!(o.status.success() && err(&o).contains("Extract?") && err(&o).contains("All done."), "{}", err(&o));
    assert!(d.join("out/a.txt").exists());
    // (in the user's language)
    let o = run_in(&d, &exe, &["other"], "y\n", "ko-KR");
    assert!(o.status.success() && err(&o).contains("풀까요?"), "{}", err(&o));
    std::fs::remove_dir_all(&d).unwrap();
}

/// Started with a console of its own (as Explorer starts it where the manifest's
/// `consoleAllocationPolicy` is not known, or with no manifest: the stub with no settings), the
/// program gives the console up and tells in windows: its failure in a message box, which it
/// waits on (killed here), where with a console it would have ended at once.
#[cfg(windows)]
#[test]
fn console_given_up() {
    let d = scratch("console");
    std::fs::create_dir_all(d.join("out")).unwrap();
    std::fs::write(d.join("out/a.txt"), b"there").unwrap();
    // (no settings: a file there fails)
    let exe = program(&d, "detaching", &archive(&[("a.txt", b"a".to_vec())]));
    // (`start "" /wait`: the command line as it is, the empty title not quoted again)
    use std::os::windows::process::CommandExt;
    let mut c = Command::new("cmd")
        .current_dir(&d)
        .raw_arg(format!("/d /c start \"\" /wait \"{}\" out", exe.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2000));
    // (the program itself still there, waiting on its message box)
    let listed = Command::new("tasklist").args(["/nh", "/fi", "imagename eq detaching.exe"]).output().unwrap();
    let waiting = String::from_utf8_lossy(&listed.stdout).contains("detaching.exe");
    let _ = Command::new("taskkill").args(["/f", "/im", "detaching.exe"]).output();
    let _ = c.kill();
    let _ = c.wait();
    assert!(waiting, "ended at once: not in a message box");
    assert_eq!(std::fs::read(d.join("out/a.txt")).unwrap(), b"there");
    std::fs::remove_dir_all(&d).unwrap();
}

/// Where the files go asked (`extract.ask`): the place offered taken by an empty answer, another
/// given, cancelled where standard input ends (nothing written), not asked by `-y` nor where
/// the command line tells.
#[test]
fn where_asked() {
    let d = scratch("ask");
    let a = archive(&[("a.txt", b"a".to_vec())]);
    let c = Config { ask: true, ..extracting("{cwd}/out", true, Overwrite::Always) };
    let exe = program(&d, "x", &[c.record(), a].concat());
    let o = run(&d, &exe, &[], "\n");
    assert!(o.status.success() && err(&o).contains("Extract to: ["), "{}", err(&o));
    assert!(d.join("out/x/a.txt").exists());
    let o = run(&d, &exe, &[], "  there  \n");
    assert!(o.status.success() && d.join("there/a.txt").exists(), "{}", err(&o));
    let o = run(&d, &exe, &[], "");
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    for (args, at) in [(&["-y"][..], "out/x"), (&["given"][..], "given")] {
        std::fs::remove_dir_all(d.join("out")).ok();
        let o = run(&d, &exe, args, "");
        assert!(o.status.success() && !err(&o).contains("Extract to"), "{args:?}: {}", err(&o));
        assert!(d.join(at).join("a.txt").exists(), "{args:?}");
    }
    std::fs::remove_dir_all(&d).unwrap();
}

/// The license of the archive's `.aqr/sfx/`, of the user's language where there is one: shown
/// and agreed to before anything is written, declined, or not asked (`-y`).
#[test]
fn license_agreed_to() {
    let d = scratch("license");
    let a = archive(&[
        (".aqr/sfx/license.txt", b"\xef\xbb\xbfThe license, in English.\n".to_vec()),
        (".aqr/sfx/license.ko.txt", "한국어 사용 허락서.\n".as_bytes().to_vec()),
        ("a.txt", b"a".to_vec()),
    ]);
    let c = Config { license: Some("license.txt".into()), ..extracting("{cwd}/out", false, Overwrite::Always) };
    let exe = program(&d, "x", &[c.record(), a.clone()].concat());
    let o = run_in(&d, &exe, &[], "n\n", "en-US");
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert!(err(&o).contains("The license, in English.") && err(&o).contains("accept the license"), "{}", err(&o));
    assert!(!d.join("out").exists());
    let o = run_in(&d, &exe, &[], "y\n", "ko-KR");
    assert!(o.status.success() && err(&o).contains("한국어 사용 허락서."), "{}", err(&o));
    // (the stub's own words in the language too)
    assert!(err(&o).contains("사용권 계약에 동의합니까?"), "{}", err(&o));
    assert!(d.join("out/a.txt").exists());
    let o = run_in(&d, &exe, &["-y", "again"], "", "en");
    assert!(o.status.success() && !err(&o).contains("license"), "{}", err(&o));
    assert!(d.join("again/a.txt").exists());
    // (a banner, of windows alone: on the console neither read nor shown, there or not)
    let c = Config { banner: Some("banner.png".into()), ..c };
    let exe = program(&d, "b", &[c.record(), a].concat());
    let o = run_in(&d, &exe, &["banner"], "y\n", "en");
    assert!(o.status.success() && err(&o).contains("The license, in English."), "{}", err(&o));
    // (named but not in the archive: a failure, nothing written)
    let c = Config { license: Some("gone.txt".into()), ..extracting("{cwd}/gone", false, Overwrite::Always) };
    let exe = program(&d, "y", &[c.record(), archive(&[("a.txt", b"a".to_vec())])].concat());
    let o = run_in(&d, &exe, &[], "y\n", "en");
    assert!(!o.status.success() && err(&o).contains("no gone.txt in the archive's first segment"), "{}", err(&o));
    assert!(!d.join("gone").exists());
    std::fs::remove_dir_all(&d).unwrap();
}

/// Settings of a version not known fail (not read as others).
#[test]
fn version_not_known() {
    let d = scratch("version");
    let mut r = Config::default().record();
    // (the version: the CBOR's first pair, -1 => 1, after the magic and length)
    assert_eq!(&r[17..19], &[0x20, 0x01]);
    r[18] = 0x02;
    let o = run(&d, &program(&d, "x", &[r, archive(&[("a", vec![1])])].concat()), &[], "");
    assert!(!o.status.success() && err(&o).contains("version 2"), "{}", err(&o));
    std::fs::remove_dir_all(&d).unwrap();
}

/// The program run once the files are written (cmd.exe, as a file of the archive): its exit
/// code the program's, its working directory, its environment and `{dest}` in its arguments,
/// the files removed once it ends; `--noexec` and `--keep`.
#[cfg(windows)]
#[test]
fn program_run() {
    let d = scratch("run");
    let cmd = std::fs::read(std::env::var("SystemRoot").unwrap() + r"\System32\cmd.exe").unwrap();
    let a = archive(&[("bin/cmd.exe", cmd), ("bin/data.txt", b"data".to_vec()), (".aqr/sfx/sfx.toml", vec![])]);
    let copied = d.join("copied.txt");
    let told = d.join("told.txt");
    let (copied_s, told_s) = (copied.display().to_string(), told.display().to_string());
    let c = Config {
        run: Some(Run {
            program: [
                "bin/cmd.exe",
                "/c",
                "copy",
                "data.txt",
                &copied_s,
                "&",
                "echo",
                "%AQR_TEST%;{dest}>",
                &told_s,
                "&",
                "exit",
                "7",
            ]
            .map(String::from)
            .to_vec(),
            cwd: "bin".into(),
            window: Window::Hidden,
            wait: true,
            cleanup: true,
            exit_code: true,
            env: vec![("AQR_TEST".into(), "at {dest}".into())],
        }),
        ..extracting("{temp}", false, Overwrite::Always)
    };
    let exe = program(&d, "x", &[c.record(), a].concat());
    let o = run(&d, &exe, &[], "");
    assert_eq!(o.status.code(), Some(7), "{}", err(&o));
    assert_eq!(std::fs::read(&copied).unwrap(), b"data");
    // (the variable set, {dest} the temporary directory made, in it and in an argument)
    let told = std::fs::read_to_string(&told).unwrap();
    let (var, arg) = told.trim_end().split_once(';').unwrap();
    assert_eq!(var, format!("at {arg}"));
    assert!(Path::new(arg).starts_with(d.join("tmp")) && Path::new(arg) != d.join("tmp"), "{told}");
    // (the temporary directory made removed, with all that was written)
    assert_eq!(std::fs::read_dir(d.join("tmp")).unwrap().count(), 0);

    // kept: the files there, .aqr/ not written where the program runs
    std::fs::remove_file(&copied).unwrap();
    let o = run(&d, &exe, &["--keep", "kept"], "");
    assert_eq!(o.status.code(), Some(7), "{}", err(&o));
    assert!(d.join("kept/bin/data.txt").exists() && !d.join("kept/.aqr").exists() && copied.exists());
    // nothing run: the files written, .aqr/ among them
    std::fs::remove_file(&copied).unwrap();
    let o = run(&d, &exe, &["--noexec", "only"], "");
    assert!(o.status.success(), "{}", err(&o));
    assert!(d.join("only/bin/data.txt").exists() && d.join("only/.aqr/sfx/sfx.toml").exists() && !copied.exists());
    std::fs::remove_dir_all(&d).unwrap();
}

/// Shortcuts to files written (on stand-ins of the desktop and the start menu,
/// `AQR_SFX_SHORTCUTS`), read back by the shell: target, arguments of `{dest}`, working
/// directory, description, name in the user's language, the start menu's folder.
#[cfg(windows)]
#[test]
fn shortcuts_made() {
    use aqr_sfx::config::{Shortcut, ShortcutAt};
    let d = scratch("shortcuts");
    let a = archive(&[("bin/app.exe", b"MZ not run".to_vec()), ("doc.txt", b"doc".to_vec())]);
    let c = Config {
        shortcuts: vec![
            Shortcut {
                target: "bin/app.exe".into(),
                name: Text(vec![("en".into(), "App".into()), ("ko".into(), "앱".into())]),
                at: ShortcutAt::Desktop,
                folder: None,
                args: vec!["--in".into(), "{dest}".into()],
                description: Some(Text::plain("The app")),
            },
            Shortcut {
                target: "doc.txt".into(),
                name: Text::plain("Doc"),
                at: ShortcutAt::StartMenu,
                folder: Some("Example/Tools".into()),
                args: Vec::new(),
                description: None,
            },
        ],
        ..extracting("{cwd}/out", false, Overwrite::Always)
    };
    let exe = program(&d, "x", &[c.record(), a].concat());
    let links = d.join("links");
    let o = Command::new(&exe)
        .current_dir(&d)
        .env("AQR_SFX_LANG", "ko-KR")
        .env("AQR_SFX_SHORTCUTS", &links)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", err(&o));
    let read = |p: &Path| {
        let script = format!(
            "$l = (New-Object -ComObject WScript.Shell).CreateShortcut('{}'); \
             [Console]::OutputEncoding = [Text.Encoding]::UTF8; \
             $l.TargetPath; $l.Arguments; $l.WorkingDirectory; $l.Description",
            p.display()
        );
        let o = Command::new("powershell").args(["-NoProfile", "-Command", &script]).output().unwrap();
        String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect::<Vec<_>>()
    };
    let out = d.join("out");
    let strip = |p: &Path| p.display().to_string();
    let app = read(&links.join("desktop/앱.lnk"));
    let want = [
        strip(&out.join(r"bin\app.exe")),
        format!("--in {}", d.join("out").display()),
        strip(&out.join("bin")),
        "The app".into(),
    ];
    assert_eq!(app, want);
    let doc = read(&links.join(r"start-menu\Example\Tools\Doc.lnk"));
    assert_eq!(doc[0], strip(&out.join("doc.txt")));
    std::fs::remove_dir_all(&d).unwrap();
}
