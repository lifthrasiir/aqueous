//! Files read and written at offsets, whole or as an error, on every platform.

use std::fs::File;
use std::io;

/// `d` written to `f` at offset `at`.
pub fn write_all_at(f: &File, d: &[u8], at: u64) -> io::Result<()> {
    #[cfg(unix)]
    return std::os::unix::fs::FileExt::write_all_at(f, d, at);
    #[cfg(windows)]
    {
        let (mut at, mut d) = (at, d);
        while !d.is_empty() {
            match std::os::windows::fs::FileExt::seek_write(f, d, at)? {
                0 => return Err(io::ErrorKind::WriteZero.into()),
                n => (at, d) = (at + n as u64, &d[n..]),
            }
        }
        Ok(())
    }
}

/// `d` filled from `f` at offset `at`; `UnexpectedEof` if the file ends first.
pub fn read_exact_at(f: &File, d: &mut [u8], at: u64) -> io::Result<()> {
    #[cfg(unix)]
    return std::os::unix::fs::FileExt::read_exact_at(f, d, at);
    #[cfg(windows)]
    {
        let (mut at, mut d) = (at, d);
        while !d.is_empty() {
            match std::os::windows::fs::FileExt::seek_read(f, d, at)? {
                0 => return Err(io::ErrorKind::UnexpectedEof.into()),
                n => (at, d) = (at + n as u64, &mut d[n..]),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_and_read_at_offsets() {
        let path = std::env::temp_dir().join(format!("aqr-common-fsio-{}", std::process::id()));
        let f = File::options().read(true).write(true).create(true).truncate(true).open(&path).unwrap();
        write_all_at(&f, b"world", 6).unwrap();
        write_all_at(&f, b"hello", 0).unwrap();
        let mut d = [0u8; 11];
        read_exact_at(&f, &mut d, 0).unwrap();
        assert_eq!(&d[..5], b"hello");
        assert_eq!(&d[6..], b"world");
        let mut d = [0u8; 5];
        read_exact_at(&f, &mut d, 6).unwrap();
        assert_eq!(&d, b"world");
        let e = read_exact_at(&f, &mut [0u8; 4], 9).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
        drop(f);
        std::fs::remove_file(&path).unwrap();
    }
}
