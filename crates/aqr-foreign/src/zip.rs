//! The ZIP fields archives of other programs agree on: extra-field ids, hosts, flag bits and the
//! method of WinZip's AES, with WinZip's field (see [`crate::crypt`]).

/// Extra-field ids: Zip64's, NTFS's times, Info-ZIP's extended timestamp, its old and new Unix
/// fields, the Unicode path, and WinZip's AES.
pub const ZIP64: u16 = 0x0001;
pub const NTFS: u16 = 0x000a;
pub const EXT_TIME: u16 = 0x5455;
pub const UNIX_OLD: u16 = 0x7855;
pub const UNIX_NEW: u16 = 0x7875;
pub const UNICODE_PATH: u16 = 0x7075;
pub const WINZIP_AES: u16 = 0x9901;

/// The hosts of "version made by" written: MS-DOS and Unix.
pub const HOST_DOS: u16 = 0;
pub const HOST_UNIX: u16 = 3;

/// Bit 11 of the flags: the name is UTF-8. Bit 6: PKWARE's strong encryption.
pub const UTF8: u16 = 0x800;
pub const STRONG_ENCRYPTION: u16 = 0x40;

/// The method that stands for WinZip's AES (its field has the method under it).
pub const METHOD_AES: u16 = 99;
