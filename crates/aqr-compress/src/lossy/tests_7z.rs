//! `coding` of 7z archives: fixtures of 7-Zip (`7zz`, skipped where it is not found), the
//! rewriter's and the check's behaviour on them.

use super::*;
use std::path::{Path, PathBuf};
use std::process::Command;

const SEVENZ: &str = "7zz";

/// `n` bytes of source text (this crate's files, from a place `seed` picks).
fn text(n: usize, seed: u64) -> Vec<u8> {
    let files: [&[u8]; 6] = [
        include_bytes!("../archive.rs"),
        include_bytes!("../tests.rs"),
        include_bytes!("../exe.rs"),
        include_bytes!("../formats.rs"),
        include_bytes!("../detect.rs"),
        include_bytes!("../output.rs"),
    ];
    let all = files.concat();
    let from = (seed as usize * 7919) % all.len();
    all.iter().cycle().skip(from).take(n).copied().collect()
}

/// Text with x86 calls here and there (what BCJ converts).
fn code(n: usize, seed: u64) -> Vec<u8> {
    let mut t = text(n, seed);
    for i in (100..n.saturating_sub(8)).step_by(97) {
        t[i] = 0xe8;
        t[i + 1..i + 5].copy_from_slice(&((i * 31) as u32).to_le_bytes());
    }
    t
}

fn have_7z() -> bool {
    let ok = Command::new(SEVENZ).output().is_ok();
    if !ok {
        eprintln!("7zz not found: skipped");
    }
    ok
}

/// `coding`.
fn coding() -> Lossy {
    Lossy::parse("coding").unwrap()
}

/// The directory of a test, removed with it.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Dir {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("aqr-lossy-7z-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(p.join("in")).unwrap();
        Dir(p)
    }

    /// The files of the tests, in `in/`: three of text and a program.
    fn files(&self) -> Vec<(&'static str, Vec<u8>)> {
        let files = vec![
            ("a.txt", text(150_000, 1)),
            ("b.txt", text(120_000, 2)),
            ("prog.exe", code(100_000, 3)),
            ("c.txt", text(90_000, 4)),
        ];
        for (n, d) in &files {
            std::fs::write(self.0.join("in").join(n), d).unwrap();
        }
        files
    }

    /// `7zz` with `args` (in `in/`), its output; fails where it does.
    fn run(&self, args: &[&str]) -> String {
        let o = Command::new(SEVENZ).args(args).arg("-y").current_dir(self.0.join("in")).output().unwrap();
        let out = String::from_utf8_lossy(&o.stdout).to_string();
        assert!(o.status.success(), "7zz {args:?}: {out} {}", String::from_utf8_lossy(&o.stderr));
        out
    }

    /// The archive `name` of the files `names` as `opts` say (named `-m...`, and so on).
    fn make(&self, name: &str, opts: &[&str], names: &[&str]) -> Vec<u8> {
        let a = self.0.join(name);
        let a = a.to_str().unwrap();
        let mut args = vec!["a", "-t7z", "-bd", "-mmt=1"];
        args.extend(opts);
        args.push(a);
        args.extend(names);
        self.run(&args);
        std::fs::read(a).unwrap()
    }

    /// The listing of `d`, written as `name`: names, sizes, CRC-32s, times and attributes.
    fn list(&self, name: &str, d: &[u8]) -> Vec<String> {
        let p = self.0.join(name);
        std::fs::write(&p, d).unwrap();
        let out = self.run(&["l", "-slt", p.to_str().unwrap()]);
        let keys = ["Path = ", "Size = ", "CRC = ", "Modified = ", "Attributes = ", "Folder = "];
        out.split("----------")
            .nth(1)
            .unwrap_or("")
            .lines()
            .filter(|l| keys.iter().any(|k| l.starts_with(k)))
            .map(String::from)
            .collect()
    }

    /// The method line of the listing of `d`.
    fn methods(&self, name: &str, d: &[u8]) -> String {
        let p = self.0.join(name);
        std::fs::write(&p, d).unwrap();
        let out = self.run(&["l", "-slt", p.to_str().unwrap()]);
        out.lines().filter(|l| l.starts_with("Method = ")).collect::<Vec<_>>().join("\n")
    }

    /// The files 7zz extracts from `d`, by name.
    fn extract(&self, name: &str, d: &[u8], password: Option<&str>) -> Vec<(String, Vec<u8>)> {
        let p = self.0.join(name);
        std::fs::write(&p, d).unwrap();
        let out = self.0.join(format!("out-{name}"));
        let o = format!("-o{}", out.display());
        let pw = password.map(|p| format!("-p{p}"));
        let mut args = vec!["x", &o, p.to_str().unwrap()];
        args.extend(pw.as_deref());
        self.run(&args);
        let mut v: Vec<_> = std::fs::read_dir(&out)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (e.file_name().to_string_lossy().to_string(), std::fs::read(e.path()).unwrap())
            })
            .collect();
        v.sort();
        v
    }

    fn test(&self, name: &str, d: &[u8]) {
        let p = self.0.join(name);
        std::fs::write(&p, d).unwrap();
        self.run(&["t", p.to_str().unwrap()]);
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn crc(d: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(d);
    c.sum()
}

fn refix(r: &mut [u8]) {
    let (off, size) = (
        u64::from_le_bytes(r[12..20].try_into().unwrap()) as usize,
        u64::from_le_bytes(r[20..28].try_into().unwrap()) as usize,
    );
    let c = crc(&r[32 + off..32 + off + size]);
    r[28..32].copy_from_slice(&c.to_le_bytes());
    let c = crc(&r[12..32]);
    r[8..12].copy_from_slice(&c.to_le_bytes());
}

/// R of `x`, made, and checked: 7zz tests it, lists and extracts it as it does `x`.
fn assert_made(dir: &Dir, name: &str, x: &[u8], expect: &[(&str, Vec<u8>)], password: Option<&str>) -> Vec<u8> {
    assert_eq!(detect(x), Some(Format::SevenZip));
    let r = match rewrite(x, &coding(), &super::tests::high()) {
        Some((Rewrite::Made { .. }, Some(r))) => r,
        other => panic!("{name}: not made: {:?}", other.map(|o| o.0)),
    };
    check::sevenz(x, &r).unwrap_or_else(|e| panic!("{name}: {e}"));
    dir.test("r.7z", &r);
    assert_eq!(dir.list("x.7z", x), dir.list("r.7z", &r), "{name}");
    let (ex, er) = (dir.extract("x.7z", x, password), dir.extract("r.7z", &r, password));
    assert_eq!(ex, er, "{name}");
    let mut want: Vec<(String, Vec<u8>)> = expect.iter().map(|(n, d)| (n.to_string(), d.clone())).collect();
    want.sort();
    assert!(er == want, "{name}: contents");
    eprintln!("7z {name}: X {} R {}", x.len(), r.len());
    // (canonical: again gives nothing)
    assert!(rewrite(&r, &coding(), &super::tests::high()).is_none(), "{name}: again");
    r
}

fn plain(files: &[(&'static str, Vec<u8>)]) -> Vec<(&'static str, Vec<u8>)> {
    files.to_vec()
}

#[test]
fn lzma2_solid_and_plain_header() {
    if !have_7z() {
        return;
    }
    let dir = Dir::new();
    let files = dir.files();
    let names: Vec<&str> = files.iter().map(|f| f.0).collect();
    for (name, opts) in [
        ("lzma2 -mx9 encoded", vec!["-m0=lzma2", "-mx=9"]),
        ("lzma2 -mx9 plain", vec!["-m0=lzma2", "-mx=9", "-mhc=off"]),
        ("lzma -mx5 encoded", vec!["-m0=lzma", "-mx=5"]),
        ("lzma -mx5 plain", vec!["-m0=lzma", "-mx=5", "-mhc=off"]),
        ("lzma2 -mx1", vec!["-m0=lzma2", "-mx=1"]),
        ("lzma -mx3 lc4 lp0 pb0", vec!["-m0=lzma:lc=4:lp=0:pb=0", "-mx=3"]),
        ("lzma2 not solid", vec!["-m0=lzma2", "-ms=off"]),
        ("lzma not solid plain", vec!["-m0=lzma", "-ms=off", "-mhc=off"]),
    ] {
        let x = dir.make("x.7z", &opts, &names);
        assert_made(&dir, name, &x, &plain(&files), None);
    }
}

#[test]
fn filters_kept() {
    if !have_7z() {
        return;
    }
    let dir = Dir::new();
    let files = dir.files();
    let names: Vec<&str> = files.iter().map(|f| f.0).collect();
    for (name, opts, method) in [
        ("bcj lzma2", vec!["-m0=BCJ", "-m1=LZMA2", "-ms=off"], "BCJ"),
        ("bcj lzma solid", vec!["-m0=BCJ", "-m1=LZMA:d=1m"], "BCJ"),
        ("delta lzma2", vec!["-m0=Delta:4", "-m1=LZMA2"], "Delta"),
        ("arm64 lzma2", vec!["-m0=ARM64", "-m1=LZMA2", "-mhc=off"], "ARM64"),
    ] {
        let x = dir.make("x.7z", &opts, &names);
        assert!(dir.methods("x.7z", &x).contains(method), "{name}: {}", dir.methods("x.7z", &x));
        assert_made(&dir, name, &x, &plain(&files), None);
    }
}

#[test]
fn other_coders_kept() {
    if !have_7z() {
        return;
    }
    let dir = Dir::new();
    let files = dir.files();
    let names: Vec<&str> = files.iter().map(|f| f.0).collect();
    let big = |x: &[u8]| rewrite(x, &coding(), &super::tests::high());
    // PPMd, BZip2, Deflate: folders as they are, nothing to change
    for m in ["-m0=PPMd", "-m0=BZip2", "-m0=Deflate", "-m0=Copy"] {
        let x = dir.make("x.7z", &[m, "-mhc=off"], &names);
        assert!(big(&x).is_none(), "{m}");
        let x = dir.make("x.7z", &[m], &names);
        assert!(big(&x).is_none(), "{m} encoded header");
    }
    // BCJ2 (four packed streams): kept, the header's LZMA remade, there is no change
    let x = dir.make(
        "x.7z",
        &["-m0=BCJ2", "-m1=LZMA", "-m2=LZMA", "-m3=LZMA", "-mb0:1", "-mb0s1:2", "-mb0s2:3"],
        &["prog.exe"],
    );
    assert!(big(&x).is_none(), "bcj2");
    // a PPMd folder with an LZMA2 one appended: only the second is made again
    let _ = std::fs::remove_file(dir.0.join("x.7z"));
    dir.make("x.7z", &["-m0=PPMd", "-mhc=off"], &["a.txt"]);
    let x = dir.make("x.7z", &["-m0=lzma2", "-mhc=off"], &["b.txt", "c.txt"]);
    let r = assert_made(
        &dir,
        "ppmd and lzma2",
        &x,
        &[("a.txt", files[0].1.clone()), ("b.txt", files[1].1.clone()), ("c.txt", files[3].1.clone())],
        None,
    );
    let _ = r;
}

#[test]
fn encrypted_kept() {
    if !have_7z() {
        return;
    }
    let dir = Dir::new();
    let files = dir.files();
    let names: Vec<&str> = files.iter().map(|f| f.0).collect();
    let r = |x: &[u8]| match rewrite(x, &coding(), &super::tests::high()) {
        Some((Rewrite::Kept { reason, .. }, None)) => Some(reason),
        None => None,
        other => panic!("{:?}", other.map(|o| o.0)),
    };
    // (header and files encrypted)
    let x = dir.make("x.7z", &["-m0=lzma2", "-mhe=on", "-psecret"], &names);
    let why = r(&x).expect("kept");
    assert!(why.contains("encrypted"), "{why}");
    // (files only: the folders are AES's; 7zz encodes the header with them nevertheless)
    let x = dir.make("x.7z", &["-m0=lzma2", "-psecret", "-mhe=off"], &names);
    assert!(r(&x).is_none_or(|w| w.contains("encrypted")));
}

#[test]
fn layouts_kept() {
    if !have_7z() {
        return;
    }
    let dir = Dir::new();
    let files = dir.files();
    let names: Vec<&str> = files.iter().map(|f| f.0).collect();
    let x = dir.make("x.7z", &["-m0=lzma2", "-mx=9"], &names);
    let why = |x: &[u8]| match rewrite(x, &coding(), &super::tests::high()) {
        Some((Rewrite::Kept { reason, .. }, None)) => reason,
        other => panic!("{:?}", other.map(|o| o.0)),
    };
    // data after the archive
    assert_eq!(why(&[&x[..], &[0u8; 4]].concat()), "data after the header, or a multi-volume archive");
    // a header damaged
    let mut bad = x.clone();
    let n = bad.len();
    bad[n - 3] ^= 1;
    assert_eq!(why(&bad), "a header's CRC-32 not right");
    // a stream damaged: not decoded (or its data not what the header tells)
    let dir2 = Dir::new();
    let files2 = dir2.files();
    let x2 = dir2.make("x.7z", &["-m0=lzma2", "-mhc=off"], &files2.iter().map(|f| f.0).collect::<Vec<_>>());
    let mut bad = x2.clone();
    bad[100] ^= 0xff;
    let w = why(&bad);
    assert!(w.contains("stream") || w.contains("data"), "{w}");
}

#[test]
fn archives_of_the_writer() {
    // (no 7zz: the files of aqr-7z's writer, a plain header, LZMA2 of the greedy encoder)
    let files = [("a.txt", text(300_000, 5)), ("e.txt", Vec::new()), ("b.txt", text(100_000, 6))];
    let x = aqr_7z::write::solid(&files.iter().map(|f| (f.0, &f.1[..])).collect::<Vec<_>>());
    let r = match rewrite(&x, &coding(), &super::tests::high()) {
        Some((Rewrite::Made { .. }, Some(r))) => r,
        other => panic!("not made: {:?}", other.map(|o| o.0)),
    };
    check::sevenz(&x, &r).unwrap();
    eprintln!("7z writer: X {} R {}", x.len(), r.len());
    assert!(rewrite(&r, &coding(), &super::tests::high()).is_none());
    // the header (but the pack size) as it was
    let h = |d: &[u8]| d[d.len() - 100..].to_vec();
    assert_eq!(h(&x)[60..], h(&r)[60..]);
    // the files, by aqr-7z's parser
    let (span, _) = aqr_7z::header::signature(&r).unwrap();
    let Some(aqr_7z::header::Top::Plain(hd)) = aqr_7z::header::parse(&r[span.start as usize..]) else { panic!() };
    assert_eq!(hd.files.len(), 3);
    assert_eq!(hd.main.pack_sizes[0] as usize, span.start as usize - 32);
    if have_7z() {
        let dir = Dir::new();
        dir.test("r.7z", &r);
        let got = dir.extract("r.7z", &r, None);
        let want: Vec<(String, Vec<u8>)> = {
            let mut v: Vec<_> = files.iter().map(|f| (f.0.to_string(), f.1.clone())).collect();
            v.sort();
            v
        };
        assert!(got == want);
    }
}

#[test]
fn check_fails_on_tampering() {
    if !have_7z() {
        return;
    }
    let dir = Dir::new();
    let files = dir.files();
    let names: Vec<&str> = files.iter().map(|f| f.0).collect();
    for (name, opts) in [("plain", vec!["-m0=lzma2", "-mhc=off"]), ("encoded", vec!["-m0=lzma", "-mx=3"])] {
        let x = dir.make("x.7z", &opts, &names);
        let r = match rewrite(&x, &coding(), &super::tests::high()) {
            Some((Rewrite::Made { .. }, Some(r))) => r,
            other => panic!("not made: {:?}", other.map(|o| o.0)),
        };
        check::sevenz(&x, &r).unwrap();
        let hdr = 32 + u64::from_le_bytes(r[12..20].try_into().unwrap()) as usize;
        let fails = |bad: &[u8], what: &str| {
            let e = check::sevenz(&x, bad);
            assert!(e.is_err(), "{name}: {what} passes");
            eprintln!("{name}: {what}: {}", e.unwrap_err());
        };
        // a byte of a packed stream
        for at in [40, 32 + (hdr - 32) / 2, hdr - 5] {
            let mut bad = r.clone();
            bad[at] ^= 0x04;
            fails(&bad, &format!("packed byte {at}"));
        }
        // the start header CRC, the next header's CRC, its offset and its size
        for at in [8, 11, 28, 31, 12, 20] {
            let mut bad = r.clone();
            bad[at] ^= 1;
            fails(&bad, &format!("start header byte {at}"));
        }
        // the same fixed up: a byte of the header (the sizes of a plain one, a name)
        let mut bad = r.clone();
        let n = bad.len();
        bad[n - 6] ^= 1;
        refix(&mut bad);
        fails(&bad, "the header, its CRC-32 fixed");
        // a pack size, in a plain header
        if name == "plain" {
            let at = hdr + 6 + bad[hdr..].windows(4).position(|w| w == [6, 0, 1, 9]).unwrap() - 6 + 4;
            let mut bad = r.clone();
            bad[at] ^= 1;
            refix(&mut bad);
            fails(&bad, "a pack size");
            // a file name
            let u: Vec<u8> = "a.txt".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
            let at = hdr + r[hdr..].windows(u.len()).position(|w| w == &u[..]).unwrap();
            let mut bad = r.clone();
            bad[at] = b'z';
            refix(&mut bad);
            fails(&bad, "a file name");
        }
        // truncated, and with bytes after
        fails(&r[..r.len() - 1], "cut");
        fails(&[&r[..], &[0]].concat(), "a byte after");
        // not a rewriting of x: another archive
        let y = dir.make("y.7z", &["-m0=lzma2", "-mhc=off"], &names[..2]);
        fails(&y, "another archive");
        let _ = Path::new("");
    }
}
