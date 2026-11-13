//! The file attributes archives of other formats keep: the kinds of a POSIX mode, and MS-DOS's
//! (Windows') attribute bits.

/// The mask of a POSIX mode's file type, and its directory, regular file and symbolic link.
pub const S_IFMT: u32 = 0o170000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFLNK: u32 = 0o120000;

/// MS-DOS's attribute of a directory, of a read-only file, and those of Windows kept (read-only,
/// hidden, system, archive).
pub const DOS_DIR: u32 = 0x10;
pub const DOS_READ_ONLY: u32 = 0x01;
pub const DOS_KEPT: u32 = 0x27;

/// The bit of 7-Zip's attributes telling a Unix mode in their high 16 bits.
pub const UNIX_EXTENSION: u32 = 0x8000;
