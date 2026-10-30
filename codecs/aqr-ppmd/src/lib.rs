//! PPMd var. J files (Dmitry Shkarin's `PPMd`, `.pmd`): their header, by which they are known,
//! and the formula of a file as its header and its data apart ([`formula`]). The data is not
//! decoded: PPMd costs as much to decode as to encode, a few MB/s (README).

pub mod formula;

/// The signature a file starts with, 0x84ACAF8F little-endian.
pub const SIGNATURE: [u8; 4] = [0x8f, 0xaf, 0xac, 0x84];

/// The header of a file: the signature, the file's attributes (4 bytes), the model (2 bytes:
/// its order less 1, the memory in MB less 1, the variant from `A`), the length of the name
/// and the restore method (2 bytes: 14 bits and 2), the file's time (4 bytes), and the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Its length, the name's included: where the data starts.
    pub len: usize,
    /// The model's order (2 to 16).
    pub order: u8,
    /// The model's memory in MB (1 to 256).
    pub memory_mb: u16,
    /// The variant, `J` (others, of headers alike or not, are not taken: none was seen).
    pub variant: char,
    /// The restore method of a model whose memory is used up (0 to 2).
    pub restore: u8,
}

/// The header of a file that `d` starts with, followed by data; None where `d` does not
/// start like one.
pub fn header(d: &[u8]) -> Option<Header> {
    if !d.starts_with(&SIGNATURE) || d.len() < 16 {
        return None;
    }
    let word = |p: usize| u16::from_le_bytes([d[p], d[p + 1]]);
    let (model, name) = (word(8), word(10));
    let len = 16 + (name & 0x3fff) as usize;
    let h = Header {
        len,
        order: (model & 0xf) as u8 + 1,
        memory_mb: (model >> 4 & 0xff) + 1,
        variant: char::from(b'A' + (model >> 12) as u8),
        restore: (name >> 14) as u8,
    };
    (h.variant == 'J' && h.order >= 2 && h.restore <= 2 && len < d.len()).then_some(h)
}

/// Whether `d` starts with the header of a file ([`header`]).
pub fn starts(d: &[u8]) -> bool {
    header(d).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header of `enwik9.pmd` of the 10 GB benchmark: order 10, 256 MB, var. J, restore
    /// method 1, the name `enwik9`.
    #[test]
    fn enwik9_header() {
        let d = b"\x8f\xaf\xac\x84\x20\0\0\0\xf9\x9f\x06\x40\xe4\xb6\x66\x35enwik9<3\x06~";
        let h = header(d).unwrap();
        assert_eq!(h, Header { len: 22, order: 10, memory_mb: 256, variant: 'J', restore: 1 });
        // (a header and nothing after it, of another signature or of var. I, is not one)
        assert!(header(&d[..22]).is_none());
        assert!(header(&[&b"\x8f\xaf\xac\x85"[..], &d[4..]].concat()).is_none());
        assert!(header(&[&d[..9], &[0x8f], &d[10..]].concat()).is_none());
    }
}
