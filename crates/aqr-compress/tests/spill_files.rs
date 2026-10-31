//! The files of a spill ([`aqr_compress::spill::Spill`]). (A test binary of its own: it counts
//! the files the process has open.)

use std::path::Path;

use aqr_compress::spill::Spill;
use aqr_formula::store::Store;

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

/// The files of a spill are closed once its regions are dropped, the spill still kept.
#[test]
fn dropped_regions_freed() {
    const SEGMENT: usize = 256 << 20;
    let dir = std::env::temp_dir().join(format!("aqr-spill-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let before = open_files(&dir);
    let spill = Spill::create(&dir).unwrap();
    let regions: Vec<_> = (0..3).map(|_| spill.region(SEGMENT).unwrap()).collect();
    regions.iter().for_each(|&(at, _)| unsafe { at.write(1) });
    let made: Vec<_> = open_files(&dir).into_iter().filter(|f| !before.contains(f)).collect();
    assert!(!made.is_empty());
    drop(regions);
    let after = open_files(&dir);
    assert!(made.iter().all(|f| !after.contains(f)), "{made:?} still open");
    drop(spill);
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
    std::fs::remove_dir(&dir).unwrap();
}
