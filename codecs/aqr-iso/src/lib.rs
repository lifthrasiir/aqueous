//! ISO 9660 and UDF images (CD, DVD and BD images, UDF bridge discs) read into their files: a
//! file system of aqr-disk ([`FS`]), which takes the image apart into the contents of the files
//! and the rest (the descriptors, directories and padding).
//!
//! `iso9660` and `udf` read the directory trees into files (their names and extents).

pub mod iso9660;
pub mod udf;

pub use aqr_disk::File;
use aqr_disk::bytes::{le16, le32};

/// Bytes of a sector, of which the volume descriptors and UDF's logical blocks are.
pub const SECTOR: usize = 2048;

/// ISO 9660 and UDF as a file system of aqr-disk. Of the trees giving the same extent, the file
/// of the lowest rank names it: UDF's (0), Joliet's (1), then ISO 9660's own (2), whose names are
/// the shortest.
pub const FS: aqr_disk::FileSystem = aqr_disk::FileSystem { name: "iso", starts, files };

/// Whether `d` starts like an image: a volume structure descriptor at sector 16 (`CD001` of
/// ISO 9660, `BEA01` of UDF's recognition sequence).
pub fn starts(d: &dyn aqr_disk::Image) -> bool {
    d.get(16 * SECTOR + 1..16 * SECTOR + 6).is_some_and(|m| *m == *b"CD001" || *m == *b"BEA01")
}

/// The files of every directory tree of image `d`, UDF's first.
pub fn files(d: &dyn aqr_disk::Image) -> Vec<File> {
    let mut out = udf::files(d);
    out.extend(iso9660::files(d));
    out
}
