//! Archives made with `-A` (mirror blocks), damaged and still read by `aqr -t` and `aqr -d`,
//! which restore the damaged blocks from their mirror blocks.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aqr-archival-{name}-{}", std::process::id()));
    if d.exists() {
        fs::remove_dir_all(&d).unwrap();
    }
    fs::create_dir_all(&d).unwrap();
    d
}

/// `aqr` given `args` in `dir`, standard input `input`.
fn aqr_fed(dir: &Path, args: &[&str], input: &[u8]) -> Output {
    use std::io::Write;
    let mut c = Command::new(env!("CARGO_BIN_EXE_aqr"))
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(input).unwrap();
    c.wait_with_output().unwrap()
}

fn aqr_in(dir: &Path, args: &[&str]) -> Output {
    let o = aqr_fed(dir, args, b"");
    assert!(o.status.success(), "aqr {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    o
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn text(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed;
    let words = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta"];
    let mut t = Vec::new();
    while t.len() < n {
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        t.extend_from_slice(format!("{} {} ", words[(x >> 61) as usize], (x >> 40) % 1000).as_bytes());
    }
    t.truncate(n);
    t
}

/// A directory `src` of text files and a gzip file (made by `aqr --format gz`, recompressed:
/// a module block and formula blocks), and its files by path.
fn source(d: &Path) -> Vec<(String, Vec<u8>)> {
    let src = d.join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    // (repeating beyond DEFLATE's window: recompressed, as smaller)
    fs::write(d.join("big.txt"), text(1, 80_000).repeat(8)).unwrap();
    aqr_in(d, &["-q", "--format", "gz", "-o", "src/big.txt.gz", "big.txt"]);
    fs::remove_file(d.join("big.txt")).unwrap();
    for i in 0..5 {
        fs::write(src.join(format!("t{i}.txt")), text(10 + i, 3000 + 500 * i as usize)).unwrap();
    }
    fs::write(src.join("sub/u.txt"), text(99, 20_000)).unwrap();
    files(&src)
}

fn files(d: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![(d.to_owned(), String::new())];
    while let Some((dir, at)) = stack.pop() {
        for e in fs::read_dir(&dir).unwrap() {
            let e = e.unwrap();
            let name = format!("{at}{}", e.file_name().to_string_lossy());
            if e.file_type().unwrap().is_dir() {
                stack.push((e.path(), name + "/"));
            } else {
                out.push((name, fs::read(e.path()).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// The blocks of archive `a` (the generic structure, from its header to its footer): where each
/// starts, its type and where its data is.
fn blocks(a: &[u8]) -> Vec<(usize, u64, Range<usize>)> {
    let leb = |at: &mut usize| {
        let (mut v, mut shift) = (0u64, 0);
        loop {
            let b = a[*at];
            *at += 1;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return v;
            }
            shift += 7;
        }
    };
    let mut out = Vec::new();
    let mut at = 6;
    loop {
        let start = at;
        let ty = leb(&mut at);
        // (the footer's type)
        if ty == 0x90_5bcb_957d {
            return out;
        }
        let width = 1 << (ty % 4);
        let size = u64::from_le_bytes(std::array::from_fn(|i| if i < width { a[at + i] } else { 0 }));
        at += width;
        let data = at..at + size as usize;
        at = data.end + (data.end - start) % 2;
        out.push((start, ty, data));
    }
}

/// A byte in the middle of the data of the first block of `a` whose type is in `types`.
fn middle_of(a: &[u8], types: Range<u64>) -> usize {
    let (_, _, data) = blocks(a).into_iter().find(|b| types.contains(&b.1)).expect("a block of the types");
    (data.start + data.end) / 2
}

/// Blocks of each kind mirrored, damaged in an archive made with `-A`: restored, told, and the
/// files extracted as they were; in an archive made without, the same damage fails.
#[test]
fn restores_damaged_blocks() {
    let d = scratch("restore");
    let want = source(&d);
    let made = aqr_in(&d, &["-A", "-o", "a.aqr", "src"]);
    assert!(stderr(&made).contains("mirror blocks: "), "{}", stderr(&made));
    aqr_in(&d, &["-q", "-o", "plain.aqr", "src"]);
    let (a, plain) = (fs::read(d.join("a.aqr")).unwrap(), fs::read(d.join("plain.aqr")).unwrap());
    assert!(a.len() > plain.len());
    // the blocks before the mirror blocks as they are without -A (but for the footer)
    let footer = plain.len() - (6 + 4 + plain[plain.len() - 2] as usize);
    assert_eq!(a[..footer], plain[..footer]);

    let t = stderr(&aqr_in(&d, &["-t", "a.aqr"]));
    assert!(t.contains("archival profile: conforms"), "{t}");
    let all = blocks(&a).iter().filter(|b| matches!(b.1, 0x10..=0x17 | 0x50..=0x67 | 0x70..=0x77)).count();
    assert!(all >= 3);
    assert!(t.contains(&format!("{all} of {all} blocks to mirror mirrored")), "{t}");
    assert!(!t.contains("nearer"), "{t}");
    assert!(!t.contains("restored"), "{t}");

    for (kind, types) in [("metadata", 0x10..0x18), ("module", 0x61..0x64), ("formula", 0x70..0x78)] {
        let at = middle_of(&a, types.clone());
        let mut damaged = a.clone();
        damaged[at] ^= 0x5a;
        fs::write(d.join("damaged.aqr"), &damaged).unwrap();
        let t = stderr(&aqr_in(&d, &["-t", "damaged.aqr"]));
        assert!(t.contains("1 damaged block restored from mirror blocks"), "{kind}: {t}");
        let out = d.join(format!("out-{kind}"));
        aqr_in(&d, &["-q", "-d", "-o", out.to_str().unwrap(), "damaged.aqr"]);
        assert_eq!(files(&out), want, "{kind}");
        // (the file is left as it was: repaired in memory)
        assert_eq!(fs::read(d.join("damaged.aqr")).unwrap(), damaged);

        let mut plain = plain.clone();
        let at = middle_of(&plain, types);
        plain[at] ^= 0x5a;
        fs::write(d.join("damaged.aqr"), &plain).unwrap();
        let o = aqr_fed(&d, &["-t", "damaged.aqr"], b"");
        assert!(!o.status.success(), "{kind}: damage not found without -A");
    }

    // a damaged mirror block: told, and the archive read as it is
    let at = middle_of(&a, 0xc10..0xc79);
    let mut damaged = a.clone();
    damaged[at] ^= 0x5a;
    fs::write(d.join("damaged.aqr"), &damaged).unwrap();
    let t = stderr(&aqr_in(&d, &["-t", "damaged.aqr"]));
    assert!(t.contains("1 damaged mirror block") && t.contains(&format!("{} of {all}", all - 1)), "{t}");
    fs::remove_dir_all(&d).unwrap();
}

/// An archive of a single file made with `-A`, and an encrypted one: the payload has no spacer
/// blocks, so that a small archive's mirror blocks are nearer than the spec asks (told).
#[test]
fn single_and_encrypted() {
    let d = scratch("single");
    source(&d);
    aqr_in(&d, &["-q", "-A", "-o", "s.aqr", "src/big.txt.gz"]);
    let t = stderr(&aqr_in(&d, &["-t", "s.aqr"]));
    assert!(t.contains("archival profile: conforms") && t.contains("blocks to mirror mirrored"), "{t}");
    assert!(!t.contains("nearer"), "{t}");
    let s = fs::read(d.join("s.aqr")).unwrap();
    let at = middle_of(&s, 0x70..0x78);
    let mut damaged = s.clone();
    damaged[at] ^= 1;
    fs::write(d.join("damaged.aqr"), &damaged).unwrap();
    let t = stderr(&aqr_in(&d, &["-q", "-d", "-o", "big.txt.gz", "damaged.aqr"]));
    assert!(t.contains("1 damaged block restored"), "{t}");
    assert_eq!(fs::read(d.join("big.txt.gz")).unwrap(), fs::read(d.join("src/big.txt.gz")).unwrap());

    let o = aqr_fed(&d, &["-q", "-A", "-P", "-o", "e.aqr", "src"], b"secret\n");
    assert!(o.status.success(), "{}", stderr(&o));
    let o = aqr_fed(&d, &["-t", "-P", "e.aqr"], b"secret\n");
    let t = stderr(&o);
    assert!(o.status.success(), "{t}");
    assert!(t.contains("archival profile: conforms") && t.contains("nearer their blocks than the spec asks"), "{t}");
    fs::remove_dir_all(&d).unwrap();
}
