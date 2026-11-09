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
