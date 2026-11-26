//! `aqr --serve` driven as a shell extension drives it: archives opened, their directories
//! listed as `aqr -t --json` lists them, their files read in pieces as `aqr -d` writes them
//! (kept in memory, or spilled where they are over the bound), several reads at once, handles
//! shared and closed, and an archive changed opened anew.

mod common;

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

fn aqr(d: &Path, args: &[&str]) -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(d).args(args).output().unwrap();
    assert!(o.status.success(), "aqr {args:?}: {}", common::stderr(&o));
    String::from_utf8(o.stdout).unwrap()
}

/// `aqr --serve` greeted, and what it answers by request.
struct Served {
    child: Child,
    out: BufReader<ChildStdout>,
    /// Answers read while another's was waited for, with their bytes.
    early: Vec<(Value, Vec<u8>)>,
    next: u64,
}

impl Served {
    fn start(args: &[&str]) -> Served {
        let mut child = Command::new(env!("CARGO_BIN_EXE_aqr"))
            .arg("--serve")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut s = Served { child, out, early: Vec::new(), next: 0 };
        s.line(json!({"hello": 1}));
        let (hello, _) = s.message();
        assert_eq!(hello["hello"], 1);
        s
    }

    fn line(&mut self, v: Value) {
        let i = self.child.stdin.as_mut().unwrap();
        writeln!(i, "{v}").unwrap();
        i.flush().unwrap();
    }

    /// `v` sent as a request: its id.
    fn send(&mut self, mut v: Value) -> u64 {
        self.next += 1;
        v["id"] = self.next.into();
        self.line(v);
        self.next
    }

    /// The next answer, with the bytes after it.
    fn message(&mut self) -> (Value, Vec<u8>) {
        let mut l = String::new();
        assert!(self.out.read_line(&mut l).unwrap() > 0, "the server ended");
        let v: Value = serde_json::from_str(&l).unwrap();
        let mut d = vec![0; v["data"].as_u64().unwrap_or(0) as usize];
        self.out.read_exact(&mut d).unwrap();
        (v, d)
    }

    /// The next answer to request `id`.
    fn answer(&mut self, id: u64) -> (Value, Vec<u8>) {
        if let Some(k) = self.early.iter().position(|(v, _)| v["id"] == id) {
            return self.early.remove(k);
        }
        loop {
            let m = self.message();
            if m.0["id"] == id {
                return m;
            }
            self.early.push(m);
        }
    }

    fn ask(&mut self, v: Value) -> Value {
        let id = self.send(v);
        self.answer(id).0
    }

    fn open(&mut self, path: &Path) -> Value {
        self.ask(json!({"open": path}))
    }

    /// The entries of `dir`, of all the answers.
    fn list(&mut self, h: &Value, dir: &str) -> Vec<Value> {
        let id = self.send(json!({"list": h, "dir": dir}));
        let mut all = Vec::new();
        loop {
            let (v, _) = self.answer(id);
            assert!(v.get("error").is_none(), "{v}");
            all.extend(v["entries"].as_array().unwrap().iter().cloned());
            if v["more"] == false {
                return all;
            }
        }
    }

    /// File `path` read whole, `len` bytes a request.
    fn read(&mut self, h: &Value, path: &str, len: u64) -> Vec<u8> {
        let mut all = Vec::new();
        loop {
            let id = self.send(json!({"read": h, "path": path, "offset": all.len(), "len": len}));
            let (v, d) = self.answer(id);
            assert!(v.get("error").is_none(), "{path}: {v}");
            assert!(d.len() as u64 <= len.min(1 << 20));
            if d.is_empty() {
                return all;
            }
            all.extend(d);
        }
    }

    fn end(mut self) {
        drop(self.child.stdin.take());
        assert!(self.child.wait().unwrap().success());
    }
}

/// A tree of files: small ones sharing a leaf, one over a read's chunk, an empty one, and
/// directories within directories.
fn tree(d: &Path) -> Vec<(String, Vec<u8>)> {
    fs::create_dir_all(d.join("t/sub/deep")).unwrap();
    fs::create_dir_all(d.join("t/empty-dir")).unwrap();
    fs::write(d.join("t/a.txt"), common::text(1, 5000)).unwrap();
    fs::write(d.join("t/sub/big.txt"), common::text(2, 3 << 20)).unwrap();
    fs::write(d.join("t/sub/deep/c.txt"), common::text(3, 700)).unwrap();
    fs::write(d.join("t/zero"), b"").unwrap();
    for k in 0..20 {
        fs::write(d.join(format!("t/sub/s{k:02}.txt")), common::text(10 + k, 300)).unwrap();
    }
    aqr(d, &["-q", "-o", "t.aqr", "t"]);
    common::files(&d.join("t"))
}

/// The paths of `aqr -t --json`, with their kinds and sizes.
fn listed(d: &Path, archive: &str) -> Vec<(String, String, Option<u64>)> {
    let t: Value = serde_json::from_str(&aqr(d, &["-t", "--json", archive])).unwrap();
    let e = t["entries"].as_array().unwrap().iter();
    let mut out: Vec<_> = e
        .map(|e| (e["path"].as_str().unwrap().to_owned(), e["type"].as_str().unwrap().to_owned(), e["size"].as_u64()))
        .collect();
    out.sort();
    out
}

/// The entries served under `dir`, by path, walked.
fn walked(s: &mut Served, h: &Value, dir: &str, out: &mut Vec<(String, String, Option<u64>)>) {
    for e in s.list(h, dir) {
        let name = e["name"].as_str().unwrap();
        let path = if dir.is_empty() { name.to_owned() } else { format!("{dir}/{name}") };
        let kind = e["kind"].as_str().unwrap().to_owned();
        out.push((path.clone(), kind.clone(), e["size"].as_u64()));
        if kind == "dir" {
            walked(s, h, &path, out);
        }
    }
}

#[test]
fn listed_and_read() {
    let d = common::scratch("serve", "listed");
    let files = tree(&d);
    let mut s = Served::start(&[]);
    let o = s.open(&d.join("t.aqr"));
    assert_eq!(o["single"], false, "{o}");
    let h = o["archive"].clone();

    let mut served = Vec::new();
    walked(&mut s, &h, "", &mut served);
    served.sort();
    assert_eq!(served, listed(&d, "t.aqr"));
    let a = s.ask(json!({"stat": h, "path": "a.txt"}));
    assert_eq!((a["entry"]["kind"].as_str(), a["entry"]["size"].as_u64()), (Some("file"), Some(5000)));
    assert!(a["entry"]["mtime"][0].as_i64().is_some_and(|t| t > 1_500_000_000), "{a}");
    assert_eq!(s.ask(json!({"stat": h, "path": ""}))["entry"]["kind"], "dir");
    // what -t -v tells of a file, with `verbose`: as its JSON listing has it
    let t: Value = serde_json::from_str(&aqr(&d, &["-t", "--json", "t.aqr"])).unwrap();
    let j = t["entries"].as_array().unwrap().iter().find(|e| e["path"] == "sub/big.txt").unwrap().clone();
    let v = s.ask(json!({"stat": h, "path": "sub/big.txt", "verbose": true}))["entry"].clone();
    for k in ["data", "overhead", "share", "segment", "formula", "nodes"] {
        assert_eq!(v[k], j[k], "{k}: {v} {j}");
    }
    assert!(v["ratio"].as_str().is_some_and(|r| r.ends_with('%')), "{v}");
    assert_eq!(v["flags"].as_str().map(str::len), Some(7), "{v}");
    // (not asked for: not there)
    assert!(a["entry"].get("data").is_none(), "{a}");

    // every file as it was archived, in reads of several sizes
    for (k, (path, bytes)) in files.iter().enumerate() {
        let len = [1 << 20, 4096, 3 << 20][k % 3];
        assert!(s.read(&h, path, len) == *bytes, "{path}");
    }
    // (from the middle, and past the end)
    let id = s.send(json!({"read": h, "path": "sub/big.txt", "offset": 1_000_000, "len": 10}));
    assert_eq!(s.answer(id).1, files.iter().find(|f| f.0 == "sub/big.txt").unwrap().1[1_000_000..1_000_010]);
    let id = s.send(json!({"read": h, "path": "a.txt", "offset": 9999, "len": 10}));
    assert_eq!(s.answer(id), (json!({"id": id, "data": 0}), Vec::new()));

    // failures, each told by its request
    for (r, what) in [
        (json!({"read": h, "path": "nope"}), "not in the archive"),
        (json!({"read": h, "path": "sub"}), "not a file"),
        (json!({"list": h, "dir": "a.txt"}), "not a directory"),
        (json!({"list": h, "dir": "a.txt/x"}), "not in the archive"),
        (json!({"list": 999}), "no archive open"),
        (json!({"what": 1}), "not known"),
    ] {
        let v = s.ask(r.clone());
        assert!(v["error"].as_str().is_some_and(|e| e.contains(what)), "{r}: {v}");
    }
    let v = s.open(&d.join("missing.aqr"));
    assert!(v["error"].is_string(), "{v}");
    s.end();
    fs::remove_dir_all(&d).unwrap();
}

/// Reads of several files sent at once, each answered by its id; the files over the bound
/// spilled, read as the others are.
#[test]
fn at_once_and_spilled() {
    let d = common::scratch("serve", "at-once");
    let files = tree(&d);
    // (a bound under the big file: spilled; the small ones dropped and evaluated again)
    let mut s = Served::start(&["-Z", "serve:cache=100000"]);
    let h = s.open(&d.join("t.aqr"))["archive"].clone();
    let ids: Vec<(u64, &(String, Vec<u8>))> =
        files.iter().map(|f| (s.send(json!({"read": h, "path": f.0, "len": 1 << 20})), f)).collect();
    for (id, (path, bytes)) in ids {
        let (v, d) = s.answer(id);
        assert!(v.get("error").is_none(), "{path}: {v}");
        assert!(d == bytes[..bytes.len().min(1 << 20)], "{path}");
    }
    for (path, bytes) in &files {
        assert!(s.read(&h, path, 1 << 20) == *bytes, "{path}");
    }
    let t = s.ask(json!({"trim": true}));
    assert!(t["trimmed"].as_u64().is_some_and(|n| n <= 100_000), "{t}");
    assert!(s.read(&h, "a.txt", 1 << 20) == files[0].1);
    s.end();
    fs::remove_dir_all(&d).unwrap();
}

/// The same file opened twice is one handle, closed as many times; a file changed at the path
/// is opened anew, the handle before still reading what it was.
#[test]
fn handles() {
    let d = common::scratch("serve", "handles");
    let files = tree(&d);
    let mut s = Served::start(&[]);
    let a = s.open(&d.join("t.aqr"));
    let b = s.open(&d.join("t.aqr"));
    assert_eq!((&a["archive"], &a["identity"]), (&b["archive"], &b["identity"]));
    let h = a["archive"].clone();
    assert_eq!(s.ask(json!({"close": h}))["closed"], h);
    // (open still, once)
    assert_eq!(s.list(&h, "").len(), 4);
    assert_eq!(s.ask(json!({"close": h}))["closed"], h);
    assert!(s.ask(json!({"list": h}))["error"].is_string());

    let h = s.open(&d.join("t.aqr"))["archive"].clone();
    // (written beside, then in its place: the one open is still mapped where it is)
    fs::write(d.join("t/new.txt"), b"new").unwrap();
    aqr(&d, &["-q", "-o", "t2.aqr", "t"]);
    let replaced = fs::rename(d.join("t2.aqr"), d.join("t.aqr"));
    if replaced.is_ok() {
        let n = s.open(&d.join("t.aqr"));
        assert_eq!(n["changed"], true, "{n}");
        assert_ne!(n["archive"], h);
        assert_eq!(s.list(&n["archive"], "").len(), 5);
        assert_eq!(s.list(&h, "").len(), 4);
        assert!(s.read(&h, "a.txt", 1 << 20) == files[0].1);
    } else {
        // (Windows: a file mapped is not replaced)
        if !cfg!(windows) {
            panic!("not replaced: {replaced:?}");
        }
    }
    s.end();
    fs::remove_dir_all(&d).unwrap();
}

/// An archive of a single file: one entry, of the name it is extracted as.
#[test]
fn single() {
    let d = common::scratch("serve", "single");
    let text = common::text(7, 2 << 20);
    fs::write(d.join("one.txt"), &text).unwrap();
    aqr(&d, &["-q", "-o", "one.txt.aqr", "one.txt"]);
    for args in [&[][..], &["-Z", "serve:cache=1000"]] {
        let mut s = Served::start(args);
        let o = s.open(&d.join("one.txt.aqr"));
        assert_eq!(o["single"], true, "{o}");
        let h = o["archive"].clone();
        let e = s.list(&h, "");
        assert_eq!(e.len(), 1);
        assert_eq!((e[0]["name"].as_str(), e[0]["size"].as_u64()), (Some("one.txt"), Some(text.len() as u64)));
        assert!(s.read(&h, "one.txt", 1 << 20) == text);
        s.end();
    }
    fs::remove_dir_all(&d).unwrap();
}

/// Many entries in a directory: listed in several answers.
#[test]
fn many() {
    let d = common::scratch("serve", "many");
    fs::create_dir_all(d.join("t")).unwrap();
    for k in 0..5000 {
        fs::write(d.join(format!("t/f{k}")), k.to_string()).unwrap();
    }
    aqr(&d, &["-q", "-o", "t.aqr", "t"]);
    let mut s = Served::start(&[]);
    let h = s.open(&d.join("t.aqr"))["archive"].clone();
    let id = s.send(json!({"list": h, "dir": ""}));
    let (first, _) = s.answer(id);
    assert_eq!((first["entries"].as_array().unwrap().len(), &first["more"]), (4096, &json!(true)));
    let (last, _) = s.answer(id);
    assert_eq!((last["entries"].as_array().unwrap().len(), &last["more"]), (904, &json!(false)));
    let names: HashMap<String, ()> = first["entries"]
        .as_array()
        .unwrap()
        .iter()
        .chain(last["entries"].as_array().unwrap())
        .map(|e| (e["name"].as_str().unwrap().to_owned(), ()))
        .collect();
    assert_eq!(names.len(), 5000);
    assert_eq!(s.read(&h, "f1234", 100), b"1234");
    s.end();
    fs::remove_dir_all(&d).unwrap();
}

/// The frontends' side of the protocol (`aqr_ipc::serve`): requests from several threads at
/// once, each waited for; the program gone, every request failing so.
#[test]
fn client() {
    use aqr_ipc::serve::{Error, Served};
    let d = common::scratch("serve", "client");
    let files = tree(&d);
    let s = Served::start(Path::new(env!("CARGO_BIN_EXE_aqr")), &[]).unwrap();
    let h = s.ask(json!({"open": d.join("t.aqr")})).unwrap()["archive"].as_u64().unwrap();
    assert_eq!(s.list(h, "sub").unwrap().len(), 22);
    std::thread::scope(|scope| {
        for (path, bytes) in &files {
            let s = &s;
            scope.spawn(move || {
                let mut got = Vec::new();
                loop {
                    let d = s.read(h, path, got.len() as u64, 1 << 20).unwrap();
                    if d.is_empty() {
                        break;
                    }
                    got.extend(d);
                }
                assert!(got == *bytes, "{path}");
            });
        }
    });
    assert!(matches!(s.read(h, "nope", 0, 10), Err(Error::Failed(m)) if m.contains("not in the archive")));
    // (killed: what it had open gone with it)
    s.kill();
    while !s.gone() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(s.list(h, ""), Err(Error::Gone));
    drop(s);
    fs::remove_dir_all(&d).unwrap();
}

/// An encrypted archive: not opened without its passphrase, nor of a wrong one; of its own,
/// listed and read as any other (a tree, and a single file). How far its evaluation went told.
#[test]
fn encrypted() {
    let d = common::scratch("serve", "encrypted");
    let files = tree(&d);
    let text = common::text(9, 300_000);
    fs::write(d.join("one.txt"), &text).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_aqr"))
        .current_dir(&d)
        .env("AQR_PASS", "sesame")
        .args(["-q", "--passphrase-from=env:AQR_PASS", "-o", "e.aqr", "t"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", common::stderr(&o));
    let o = Command::new(env!("CARGO_BIN_EXE_aqr"))
        .current_dir(&d)
        .env("AQR_PASS", "sesame")
        .args(["-q", "--passphrase-from=env:AQR_PASS", "-o", "one.aqr", "one.txt"])
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", common::stderr(&o));
    let mut s = Served::start(&[]);
    let v = s.open(&d.join("e.aqr"));
    assert_eq!((&v["encrypted"], v.get("wrong")), (&json!(true), None), "{v}");
    let v = s.ask(json!({"open": d.join("e.aqr"), "passphrase": "open"}));
    assert_eq!((&v["encrypted"], &v["wrong"]), (&json!(true), &json!(true)), "{v}");
    let v = s.ask(json!({"open": d.join("e.aqr"), "passphrase": "sesame"}));
    let h = v["archive"].clone();
    assert!(h.is_u64(), "{v}");
    let mut served = Vec::new();
    walked(&mut s, &h, "", &mut served);
    served.sort();
    assert_eq!(served, listed(&d, "t.aqr"));
    for (path, bytes) in &files {
        assert!(s.read(&h, path, 1 << 20) == *bytes, "{path}");
    }
    let p = s.ask(json!({"progress": h}));
    assert_eq!(p["evaluating"], 0, "{p}");
    assert!(p["evaluated"][1].is_u64() && p["decompressed"][1].is_u64(), "{p}");
    let v = s.ask(json!({"open": d.join("one.aqr"), "passphrase": "sesame"}));
    assert_eq!(v["single"], true, "{v}");
    assert!(s.read(&v["archive"], "one", 1 << 20) == text);
    s.end();
    fs::remove_dir_all(&d).unwrap();
}

/// The journal points of an archive appended to (named versions), and the archive opened as it
/// was at each: of the files there then.
#[test]
fn journal() {
    let d = common::scratch("serve", "journal");
    fs::create_dir_all(d.join("t")).unwrap();
    fs::write(d.join("t/one.txt"), common::text(1, 3000)).unwrap();
    aqr(&d, &["-q", "-j", "first", "-o", "t.aqr", "t"]);
    fs::write(d.join("t/two.txt"), common::text(2, 2000)).unwrap();
    aqr(&d, &["-q", "-a", "-j", "second", "t.aqr", "t/two.txt"]);
    let mut s = Served::start(&[]);
    let h = s.open(&d.join("t.aqr"))["archive"].clone();
    let points = s.ask(json!({"journal": h}))["points"].as_array().unwrap().clone();
    let labels: Vec<&str> = points.iter().map(|p| p["label"].as_str().unwrap()).collect();
    assert_eq!(labels, ["first", "second"]);
    assert!(points[0]["shown"].as_str().is_some_and(|t| t.len() == 19), "{points:?}");
    let names = |s: &mut Served, h: &Value| {
        let mut n: Vec<String> = s.list(h, "").iter().map(|e| e["name"].as_str().unwrap().to_owned()).collect();
        n.sort();
        n
    };
    assert_eq!(names(&mut s, &h), ["one.txt", "t"]);
    let first = s.ask(json!({"open": d.join("t.aqr"), "at": points[0]["metadata"]}));
    let f = first["archive"].clone();
    assert_ne!(f, h, "{first}");
    assert_eq!(names(&mut s, &f), ["one.txt"]);
    assert!(s.read(&f, "one.txt", 1 << 20) == common::text(1, 3000));
    // (the same point again: the same handle)
    let again = s.ask(json!({"open": d.join("t.aqr"), "at": points[0]["metadata"]}));
    assert_eq!(again["archive"], f);
    s.end();
    fs::remove_dir_all(&d).unwrap();
}

/// Archives of other formats (ZIP, 7z, tar.gz: as aqr writes them), served as Aqueous ones
/// are: their entries, a file read alone (of the tar archive, all at once), no journal point.
#[test]
fn foreign() {
    let d = common::scratch("serve", "foreign");
    fs::create_dir_all(d.join("t/sub")).unwrap();
    fs::write(d.join("t/a.txt"), common::text(1, 5000)).unwrap();
    fs::write(d.join("t/sub/b.txt"), common::text(2, 300_000)).unwrap();
    fs::write(d.join("t/sub/e.txt"), b"").unwrap();
    let mut s = Served::start(&[]);
    for name in ["t.zip", "t.7z", "t.tar.gz"] {
        aqr(&d, &["-q", "-o", name, "t"]);
        let opened = s.open(&d.join(name));
        let h = opened["archive"].clone();
        assert!(h.is_u64(), "{name}: {h}");
        // (what its format has: never what -t -v tells of blocks)
        let fields = opened["fields"].as_array().unwrap();
        assert!(fields.contains(&json!("modified")) && !fields.contains(&json!("segment")), "{name}: {opened}");
        // (the folder itself at the top, as other archivers have it)
        let top: Vec<String> = s.list(&h, "").iter().map(|e| e["name"].as_str().unwrap().to_owned()).collect();
        let p = match &top[..] {
            [t] if t == "t" => "t/",
            _ => "",
        };
        let mut root: Vec<(String, String)> = s
            .list(&h, p.trim_end_matches('/'))
            .iter()
            .map(|e| (e["name"].as_str().unwrap().to_owned(), e["kind"].as_str().unwrap().to_owned()))
            .collect();
        root.sort();
        assert_eq!(root, [("a.txt".into(), "file".into()), ("sub".into(), "dir".into())], "{name}");
        let sub = s.list(&h, &format!("{p}sub"));
        let b = sub.iter().find(|e| e["name"] == "b.txt").unwrap();
        assert_eq!(b["size"], 300_000, "{name}: {b}");
        // (its compressed bytes and ratio, but of the tar archive, compressed whole)
        match name {
            "t.tar.gz" => assert!(b.get("data").is_none(), "{b}"),
            _ => assert!(
                b["data"].as_u64().is_some_and(|n| n > 0 && n < 300_000) && b["ratio"].is_string(),
                "{name}: {b}"
            ),
        }
        assert!(s.read(&h, &format!("{p}sub/b.txt"), 1 << 20) == common::text(2, 300_000), "{name}");
        assert!(s.read(&h, &format!("{p}a.txt"), 1 << 20) == common::text(1, 5000), "{name}");
        assert!(s.read(&h, &format!("{p}sub/e.txt"), 1 << 20).is_empty(), "{name}");
        assert_eq!(s.ask(json!({"journal": h}))["points"], json!([]));
    }
    s.end();
    fs::remove_dir_all(&d).unwrap();
}
