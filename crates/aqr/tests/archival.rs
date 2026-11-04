//! Archives made with `-A` (mirror blocks), damaged and still read by `aqr -K` and `aqr -d`,
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
    // (a run refused before it reads its input, as a format of no encryption is, closes it
    // first: what it tells is what is checked)
    match c.stdin.take().unwrap().write_all(input) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => panic!("{e}"),
        _ => {}
    }
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
    // the blocks as they are without -A, but for the mirror blocks (the metadata block's before
    // the files, as the archive is smaller than 4 MiB), the spacer blocks and the metadata block
    // (whose offsets reach back over the mirror block before the files)
    let content = |a: &[u8]| -> Vec<u8> {
        blocks(a)
            .iter()
            .filter(|b| !(0xc10..0xc79).contains(&b.1) && b.1 > 3 && !(0x10..0x18).contains(&b.1))
            .flat_map(|(start, _, data)| a[*start..data.end + (data.end - start) % 2].to_vec())
            .collect()
    };
    assert_eq!(content(&a), content(&plain));
    let meta = blocks(&a).iter().find(|b| (0x10..0x18).contains(&b.1)).unwrap().0;
    assert!(blocks(&a)[0].1 >= 0xc10 && blocks(&a)[0].0 < meta);

    let t = stderr(&aqr_in(&d, &["-K", "a.aqr"]));
    assert!(t.contains("archival profile: conforms"), "{t}");
    let all = blocks(&a).iter().filter(|b| matches!(b.1, 0x10..=0x17 | 0x50..=0x67 | 0x70..=0x77)).count();
    assert!(all >= 3);
    assert!(t.contains(&format!("mirrored archive: yes ({all} of {all} blocks to mirror mirrored)")), "{t}");
    assert!(!t.contains("nearer"), "{t}");
    assert!(!t.contains("restored"), "{t}");

    for (kind, types) in [("metadata", 0x10..0x18), ("module", 0x61..0x64), ("formula", 0x70..0x78)] {
        let at = middle_of(&a, types.clone());
        let mut damaged = a.clone();
        damaged[at] ^= 0x5a;
        fs::write(d.join("damaged.aqr"), &damaged).unwrap();
        let t = stderr(&aqr_in(&d, &["-K", "damaged.aqr"]));
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
        let o = aqr_fed(&d, &["-K", "damaged.aqr"], b"");
        assert!(!o.status.success(), "{kind}: damage not found without -A");
    }

    // a damaged mirror block: told, and the archive read as it is
    let at = middle_of(&a, 0xc10..0xc79);
    let mut damaged = a.clone();
    damaged[at] ^= 0x5a;
    fs::write(d.join("damaged.aqr"), &damaged).unwrap();
    let t = stderr(&aqr_in(&d, &["-K", "damaged.aqr"]));
    assert!(
        t.contains("1 damaged mirror block") && t.contains(&format!("mirrored archive: no ({} of {all}", all - 1)),
        "{t}"
    );
    fs::remove_dir_all(&d).unwrap();
}

/// An archive of a single file made with `-A`, and an encrypted one: the payload has no spacer
/// blocks, so that a small archive's mirror blocks are nearer than the spec asks (told).
#[test]
fn single_and_encrypted() {
    let d = scratch("single");
    source(&d);
    aqr_in(&d, &["-q", "-A", "-o", "s.aqr", "src/big.txt.gz"]);
    let t = stderr(&aqr_in(&d, &["-K", "s.aqr"]));
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
    let o = aqr_fed(&d, &["-K", "-P", "e.aqr"], b"secret\n");
    let t = stderr(&o);
    assert!(o.status.success(), "{t}");
    assert!(
        t.contains("archival profile: conforms")
            && t.contains("mirrored archive: no")
            && t.contains("only nearer than the spec asks"),
        "{t}"
    );
    fs::remove_dir_all(&d).unwrap();
}

/// A file of `n` bytes that do not compress.
fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (x >> 56) as u8
        })
        .collect()
}

/// An archive made with `-A` cut short: after its metadata block (the footer made again), in
/// it (restored from its mirror block before the last 4 MiB, held back as the archive is made
/// in batches), but not before it (where files' blocks are lost); without `-A`, a cut into the
/// metadata block is lost.
#[test]
fn restores_truncated() {
    let d = scratch("truncated");
    let mut want = source(&d);
    // (files that do not compress, in batches of about 1 MiB: more than 4 MiB after the first)
    for i in 0..6 {
        let name = format!("n{i}.bin");
        let n = noise(900_000, 100 + i);
        fs::write(d.join("src").join(&name), &n).unwrap();
        want.push((name, n));
    }
    want.sort();
    aqr_in(&d, &["-q", "-A", "-B", "1M", "-o", "a.aqr", "src"]);
    aqr_in(&d, &["-q", "-B", "1M", "-o", "plain.aqr", "src"]);
    let (a, plain) = (fs::read(d.join("a.aqr")).unwrap(), fs::read(d.join("plain.aqr")).unwrap());
    let bs = blocks(&a);
    let meta = bs.iter().find(|b| (0x10..0x18).contains(&b.1)).unwrap().0;
    // the metadata block's mirror block, before it by more than 4 MiB (but not much more)
    let mirror = bs.iter().filter(|b| (0xc10..0xc18).contains(&b.1)).map(|b| b.0).collect::<Vec<_>>();
    assert_eq!(mirror.len(), 1);
    assert!(meta - mirror[0] > 4 << 20 && meta - mirror[0] < 6 << 20, "{} {meta}", mirror[0]);
    let t = stderr(&aqr_in(&d, &["-K", "a.aqr"]));
    assert!(t.contains("mirrored archive: yes"), "{t}");

    for (what, cut) in [("after", a.len() - 7), ("in", meta + 5), ("at", meta)] {
        fs::write(d.join("cut.aqr"), &a[..cut]).unwrap();
        let t = stderr(&aqr_in(&d, &["-K", "cut.aqr"]));
        assert!(t.contains("its footer made again"), "{what}: {t}");
        assert_eq!(t.contains("1 damaged block restored"), what != "after", "{what}: {t}");
        let out = d.join(format!("out-{what}"));
        aqr_in(&d, &["-q", "-d", "-o", out.to_str().unwrap(), "cut.aqr"]);
        assert_eq!(files(&out), want, "{what}");
    }
    // cut before the metadata block: salvaged, the files of the blocks lost told and left out,
    // the others extracted as they were (the run failing)
    for (k, cut) in [(1, meta - 100), (2, meta - (3 << 20))] {
        fs::write(d.join("cut.aqr"), &a[..cut]).unwrap();
        let out = d.join(format!("salvaged-{k}"));
        let o = aqr_fed(&d, &["-d", "-o", out.to_str().unwrap(), "cut.aqr"], b"");
        let t = stderr(&o);
        assert!(!o.status.success() && t.contains("salvaging what is left") && t.contains(" lost ("), "{t}");
        let got = files(&out);
        assert!(!got.is_empty() && got.len() < want.len(), "{t}");
        assert!(got.iter().all(|f| want.contains(f)));
        // (each file left out told)
        for (name, _) in want.iter().filter(|f| !got.contains(f)) {
            assert!(t.contains(&format!("{name}: lost (")), "{name}: {t}");
        }
        let o = aqr_fed(&d, &["-K", "cut.aqr"], b"");
        assert!(!o.status.success() && stderr(&o).contains(" files lost"), "{}", stderr(&o));
    }
    let meta = blocks(&plain).iter().find(|b| (0x10..0x18).contains(&b.1)).unwrap().0;
    fs::write(d.join("cut.aqr"), &plain[..meta + 5]).unwrap();
    assert!(!aqr_fed(&d, &["-K", "cut.aqr"], b"").status.success());
    fs::remove_dir_all(&d).unwrap();
}

/// An archive of a segment for each batch (`-S 1`) cut short in a segment before the last: the
/// segments before it extracted, the one cut short and those after it lost as a whole, the run
/// failing (with or without `-A`).
#[test]
fn salvages_segments() {
    let d = scratch("segments");
    fs::create_dir_all(d.join("src")).unwrap();
    let mut want = Vec::new();
    for i in 0..6 {
        let name = format!("n{i}.bin");
        let n = noise(300_000, 300 + i);
        fs::write(d.join("src").join(&name), &n).unwrap();
        want.push((name, n));
    }
    for mirrors in [false, true] {
        let mut args = vec!["-q", "-f", "-B", "300K", "-S", "1", "-o", "s.aqr", "src"];
        if mirrors {
            args.insert(0, "-A");
        }
        aqr_in(&d, &args);
        let a = fs::read(d.join("s.aqr")).unwrap();
        let metas: Vec<usize> = blocks(&a).iter().filter(|b| (0x10..0x18).contains(&b.1)).map(|b| b.0).collect();
        assert_eq!(metas.len(), 6, "{mirrors}");
        let t = stderr(&aqr_in(&d, &["-K", "s.aqr"]));
        assert!(!mirrors || t.contains("mirrored archive: yes"), "{t}");
        // (just after the third segment's metadata block: its footer made again, but the
        // segment not the one meant to be the last)
        fs::write(
            d.join("cut.aqr"),
            &a[..metas[2]
                + (blocks(&a).iter().find(|b| b.0 == metas[2]).unwrap().2.end - metas[2]).next_multiple_of(2)],
        )
        .unwrap();
        let out = d.join(format!("after-{mirrors}"));
        let o = aqr_fed(&d, &["-d", "-o", out.to_str().unwrap(), "cut.aqr"], b"");
        let t = stderr(&o);
        assert!(!o.status.success() && t.contains("its footer made again") && t.contains("segments after it"), "{t}");
        assert_eq!(files(&out).len(), 3, "{t}");
        // (in the fourth segment's metadata block: its file salvaged whole, of no name)
        fs::write(d.join("cut.aqr"), &a[..metas[3] + 10]).unwrap();
        let out = d.join(format!("unnamed-{mirrors}"));
        let o = aqr_fed(&d, &["-d", "-o", out.to_str().unwrap(), "cut.aqr"], b"");
        let t = stderr(&o);
        assert!(!o.status.success() && t.contains("1 file of no name"), "{t}");
        let got = files(&out);
        assert_eq!(got.len(), 4, "{t}");
        assert!(got.contains(&("#salvaged/leaf-1".to_string(), want[3].1.clone())), "{t}");
        let o = aqr_fed(&d, &["-K", "cut.aqr"], b"");
        assert!(!o.status.success() && stderr(&o).contains("1 file of no name"), "{}", stderr(&o));
        // (in the fourth segment's blocks)
        fs::write(d.join("cut.aqr"), &a[..metas[2] + 100_000]).unwrap();
        let out = d.join(format!("out-{mirrors}"));
        let o = aqr_fed(&d, &["-d", "-o", out.to_str().unwrap(), "cut.aqr"], b"");
        let t = stderr(&o);
        assert!(!o.status.success() && t.contains("a segment cut short lost"), "{t}");
        let got = files(&out);
        assert_eq!(got.len(), 3, "{t}");
        assert!(got.iter().all(|f| want.contains(f)));
    }
    fs::remove_dir_all(&d).unwrap();
}

/// An archive without mirror blocks whose file's block is damaged: the others extracted, that
/// file told as lost and left out (the run failing), its checksum telling.
#[test]
fn leaves_damaged_files_out() {
    let d = scratch("damaged-file");
    let want = source(&d);
    aqr_in(&d, &["-q", "-o", "a.aqr", "src"]);
    let mut a = fs::read(d.join("a.aqr")).unwrap();
    // (the last formula block: of a file alone, as the text files are solid)
    let (_, _, data) = blocks(&a).into_iter().rfind(|b| (0x70..0x78).contains(&b.1)).unwrap();
    a[(data.start + data.end) / 2] ^= 1;
    fs::write(d.join("a.aqr"), &a).unwrap();
    let out = d.join("out");
    let o = aqr_fed(&d, &["-d", "-o", out.to_str().unwrap(), "a.aqr"], b"");
    let t = stderr(&o);
    assert!(!o.status.success() && t.contains("1 file lost"), "{t}");
    let got = files(&out);
    assert_eq!(got.len(), want.len() - 1, "{t}");
    assert!(got.iter().all(|f| want.contains(f)));
    let gone = want.iter().find(|f| !got.contains(f)).unwrap();
    assert!(t.contains(&format!("{}: lost (invalid archive: checksum mismatch)", gone.0)), "{t}");
    fs::remove_dir_all(&d).unwrap();
}

/// An encrypted archive made with `-A`, damaged: its header (taken from a mirrored header), its
/// last chunk (of the mirror blocks after the metadata block and the footer: the footer made
/// again), a chunk of a file's block (that file lost, the others extracted).
#[test]
fn repairs_encrypted() {
    let d = scratch("encrypted");
    let mut want = source(&d);
    for i in 0..6 {
        let name = format!("n{i}.bin");
        let n = noise(900_000, 200 + i);
        fs::write(d.join("src").join(&name), &n).unwrap();
        want.push((name, n));
    }
    want.sort();
    let o = aqr_fed(&d, &["-q", "-A", "-P", "-B", "1M", "-o", "e.aqr", "src"], b"secret\n");
    assert!(o.status.success(), "{}", stderr(&o));
    let e = fs::read(d.join("e.aqr")).unwrap();
    let extract = |name: &str, bytes: &[u8]| {
        fs::write(d.join("x.aqr"), bytes).unwrap();
        let out = d.join(name);
        let o = aqr_fed(&d, &["-d", "-P", "-o", out.to_str().unwrap(), "x.aqr"], b"secret\n");
        (o.status.success(), stderr(&o), files(&out))
    };
    // the header magic and its checksum
    let mut x = e.clone();
    x[0] ^= 1;
    x[50] ^= 1;
    let (ok, t, got) = extract("header", &x);
    assert!(ok && t.contains("taken from mirrored header 1"), "{t}");
    assert_eq!(got, want);
    // the last chunk's tag
    let mut x = e.clone();
    let n = x.len();
    x[n - 3] ^= 1;
    let (ok, t, got) = extract("last", &x);
    assert!(ok && t.contains("1 encrypted chunk not authenticating") && t.contains("its footer made again"), "{t}");
    assert_eq!(got, want);
    // a chunk amid the files that do not compress, first in the archive
    let mut x = e.clone();
    x[n / 3] ^= 1;
    let (ok, t, got) = extract("amid", &x);
    assert!(!ok && t.contains("1 encrypted chunk not authenticating") && t.contains("1 file lost"), "{t}");
    assert_eq!(got.len(), want.len() - 1, "{t}");
    assert!(got.iter().all(|f| want.contains(f)));
    let gone = want.iter().find(|f| !got.contains(f)).unwrap();
    assert!(t.contains(&format!("{}: lost (", gone.0)), "{t}");
    fs::remove_dir_all(&d).unwrap();
}
