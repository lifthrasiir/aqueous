//! `aqr -t --json`: a line of JSON for each archive, its entries with their metadata.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-json-{name}-{}", std::process::id()));
    _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn aqr(d: &std::path::Path, args: &[&str]) -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_aqr")).current_dir(d).args(args).output().unwrap();
    assert!(o.status.success(), "aqr {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn listing() {
    let d = scratch("listing");
    fs::create_dir_all(d.join("t/sub")).unwrap();
    fs::write(d.join("t/a.txt"), "some text\n".repeat(100)).unwrap();
    fs::write(d.join("t/sub/b.txt"), "other text\n".repeat(10)).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("a.txt", d.join("t/l")).unwrap();
    aqr(&d, &["-q", "-o", "t.aqr", "t"]);
    aqr(&d, &["-q", "-o", "a.aqr", "t/a.txt"]);
    let out = aqr(&d, &["-t", "--json", "--each", "t.aqr", "a.aqr"]);
    let lines: Vec<Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2);
    let t = &lines[0];
    assert_eq!(t["archive"], "t.aqr");
    assert_eq!((t["files"].as_u64(), t["directories"].as_u64(), t["size"].as_u64()), (Some(2), Some(1), Some(1110)));
    let e: Vec<&Value> = t["entries"].as_array().unwrap().iter().collect();
    let paths: Vec<&str> = e.iter().map(|e| e["path"].as_str().unwrap()).collect();
    let mut want = vec!["a.txt", "l", "sub", "sub/b.txt"];
    if cfg!(not(unix)) {
        want.remove(1);
    }
    assert_eq!(paths, want);
    assert_eq!((e[0]["type"].as_str(), e[0]["size"].as_u64()), (Some("file"), Some(1000)));
    assert!(e[0]["mtime"].as_str().is_some_and(|t| t.contains('T')) && e[0]["formula"].is_string());
    #[cfg(unix)]
    assert_eq!((e[1]["type"].as_str(), e[1]["target"].as_str()), (Some("link"), Some("a.txt")));
    assert_eq!(e[want.len() - 2]["type"], "dir");
    let a = &lines[1];
    assert_eq!((a["archive"].as_str(), a["entries"][0]["path"].is_null()), (Some("a.aqr"), true));
    // (-q: the paths alone, no sizes)
    let q: Value = serde_json::from_str(&aqr(&d, &["-tq", "--json", "t.aqr", "sub"])).unwrap();
    assert_eq!(q["entries"].as_array().unwrap().len(), 2);
    assert!(q["entries"][1]["size"].is_null() && q["size"].is_null());
    fs::remove_dir_all(&d).unwrap();
}

/// The entries of a tree of Unix metadata: a FIFO, a hard link (a link group), extended
/// attributes, owner and mode; with `-v` each entry's size breakdown too.
#[cfg(target_os = "macos")]
#[test]
fn unix_entries() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let d = scratch("unix");
    let t = d.join("t");
    fs::create_dir_all(&t).unwrap();
    fs::write(t.join("a"), "some text\n".repeat(50)).unwrap();
    fs::hard_link(t.join("a"), t.join("h")).unwrap();
    let fifo = CString::new(t.join("fifo").as_os_str().as_bytes()).unwrap();
    // SAFETY: a valid path
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o640) }, 0);
    let (path, name, value) = (CString::new(t.join("a").as_os_str().as_bytes()).unwrap(), c"user.aqr", b"value");
    // SAFETY: the value is of its length
    assert_eq!(unsafe { libc::setxattr(path.as_ptr(), name.as_ptr(), value.as_ptr().cast(), value.len(), 0, 0) }, 0);
    aqr(&d, &["-q", "-p", "-o", "t.aqr", "t"]);
    let v: Value = serde_json::from_str(&aqr(&d, &["-t", "--json", "-v", "t.aqr"])).unwrap();
    let by = |p: &str| v["entries"].as_array().unwrap().iter().find(|e| e["path"] == p).cloned().expect(p);
    let (fifo, a, h) = (by("fifo"), by("a"), by("h"));
    assert_eq!(fifo["type"], "fifo");
    assert_eq!(fifo["mode"].as_u64().unwrap() & 0o777, 0o640 & !current_umask());
    assert!(a["xattrs"].as_array().unwrap().contains(&"user.aqr".into()), "{a}");
    assert!(a["link_group"].is_number() && a["link_group"] == h["link_group"], "{a} {h}");
    assert!(a["uid"].is_number() && a["gid"].is_number() && a["user"].is_string());
    assert!(a["data"].is_number() && a["segment"] == 1 && a["formula"].is_string());
    assert_eq!(v["others"], 1);
    fs::remove_dir_all(&d).unwrap();
}

#[cfg(target_os = "macos")]
fn current_umask() -> u64 {
    // SAFETY: umask only swaps the mask
    let m = unsafe { libc::umask(0o22) };
    unsafe { libc::umask(m) };
    m as u64
}
