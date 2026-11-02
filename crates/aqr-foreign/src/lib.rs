//! What archives of other formats keep alike, both ways: aqr-unpack reading them, aqr-pack
//! writing them. Each conversion is here with its inverse, so that they agree.
//!
//! - [`charset`]: the character sets of names of no declared encoding, and their guessing.
//! - [`crypt`]: ZIP's traditional encryption, WinZip's AES and 7-Zip's AES.
//! - [`time`]: Windows' FILETIME and MS-DOS's local date and time.

pub mod charset;
pub mod crypt;
pub mod time;
