//! How many files a spill ([`aqr_compress::spill::Spill`]) makes for many small streams of a
//! length not known as they are written. (A test binary of its own: it counts the files the
//! process has open, and sets the process's store.)

use std::path::Path;
use std::sync::Arc;

use aqr_compress::spill::Spill;
use aqr_formula::store;

/// The files of segments open: the unlinked files the process has open, by device and inode
/// (Unix); the files in `dir`, by name, as delete on close keeps them there until closed
/// (Windows).
#[cfg(unix)]
fn open_files(_: &Path) -> Vec<String> {
    use std::os::unix::fs::MetadataExt;
    let fds = std::fs::read_dir("/dev/fd").unwrap();
    let paths: Vec<_> = fds.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    let files = paths.iter().filter_map(|p| std::fs::metadata(p).ok()).filter(|m| m.nlink() == 0);
    files.map(|m| format!("{}:{}", m.dev(), m.ino())).collect()
}

#[cfg(not(unix))]
fn open_files(dir: &Path) -> Vec<String> {
    let files = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok());
    files.map(|e| e.file_name().to_string_lossy().into_owned()).collect()
}

/// The lengths of the files of [`open_files`] (as the file system tells them, sparse or not).
#[cfg(unix)]
fn open_lens(_: &Path) -> Vec<(String, u64)> {
    use std::os::unix::fs::MetadataExt;
    let fds = std::fs::read_dir("/dev/fd").unwrap();
    let paths: Vec<_> = fds.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    let files = paths.iter().filter_map(|p| std::fs::metadata(p).ok()).filter(|m| m.nlink() == 0);
    files.map(|m| (format!("{}:{}", m.dev(), m.ino()), m.len())).collect()
}

#[cfg(not(unix))]
fn open_lens(dir: &Path) -> Vec<(String, u64)> {
    let files = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok());
    files.map(|e| (e.file_name().to_string_lossy().into_owned(), e.metadata().unwrap().len())).collect()
}

/// Streams of a length not known (an archive's entries gathered by kind, `ByKind`), small and
/// kept, are regions of the segments shared by all rather than a file each.
#[test]
fn small_streams_share_segments() {
    let dir = std::env::temp_dir().join(format!("aqr-spill-count-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let before = open_files(&dir);
    let spill = Spill::create(&dir.join(".out")).unwrap();
    let guard = store::set(Some(Arc::new(spill)));
    let piece = vec![7u8; 10 << 10];
    let kept: Vec<_> = (0..100)
        .map(|_| {
            let mut w = store::Writer::new(None);
            (0..3).for_each(|_| w.write(&piece));
            w.into_bytes()
        })
        .collect();
    let made: Vec<_> = open_files(&dir).into_iter().filter(|f| !before.contains(f)).collect();
    assert!(kept.iter().all(|b| b.len() == 30 << 10 && b.iter().all(|&x| x == 7)));
    assert!(kept.iter().all(|b| b.is_shared()), "kept on the heap");
    assert!(made.len() <= 1, "{} files for 100 streams of 30 KiB", made.len());
    // (one longer than the heap takes: a region of its own from there, the bytes before first)
    let mut w = store::Writer::new(None);
    let long: Vec<u8> = (0..store::UNTOLD as u32 / 4 + 1000).flat_map(|i| i.to_le_bytes()).collect();
    long.chunks(1 << 20).for_each(|c| w.write(c));
    let b = w.into_bytes();
    assert!(b.is_shared() && b == long);
    drop(b);
    // (and its files of about its length, not of all it might grow to: on Windows a mapping
    // takes commit by its length, which files of 64 GiB each ran out of)
    let before: Vec<_> = open_files(&dir);
    let mut w = store::Writer::new(None);
    let long: Vec<u8> = (0..(300u32 << 20) / 4).flat_map(|i| i.wrapping_mul(2654435761).to_le_bytes()).collect();
    long.chunks(1 << 20).for_each(|c| w.write(c));
    let lens: u64 = open_lens(&dir).iter().filter(|f| !before.contains(&f.0)).map(|f| f.1).sum();
    let b = w.into_bytes();
    assert!(b.is_shared() && b == long);
    assert!(lens <= 4 * long.len() as u64, "files of {lens} bytes for a stream of {}", long.len());
    drop(b);
    drop(kept);
    drop(guard);
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
    std::fs::remove_dir(&dir).unwrap();
}
