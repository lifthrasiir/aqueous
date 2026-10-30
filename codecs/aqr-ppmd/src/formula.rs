//! aqr-ppmd as an entry formula (see aqr-formula): the file as the concatenation of its header,
//! a slice of the stream `ppmd/header`, and its data, a slice of the stream `ppmd/data`.
//! `FORMAT.md` tells of the streams.

use aqr_formula::{Formula, Node};

/// The class of the stream of the header, stream 0.
pub const HEADER: &str = "ppmd/header";

/// The class of the stream of the data, stream 1.
pub const DATA: &str = "ppmd/data";

/// The formula of PPMd file `input`, or None where it is none ([`crate::header`]). The same
/// whatever `threads` is (none is used).
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    let h = crate::header(input)?;
    let mut f = Formula::default();
    let parts = vec![f.push(HEADER, input[..h.len].to_vec()), f.push(DATA, input[h.len..].to_vec())];
    f.root = Node::Concat(parts);
    Some(f)
}

/// Whether formula `f` is one of [`encode`]'s, or holds one (a scan's of a file, say).
pub fn is(f: &Formula) -> bool {
    f.streams.iter().any(|s| s.class == HEADER)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file's formula gives it back, its header and its data apart.
    #[test]
    fn header_and_data() {
        let d = [&b"\x8f\xaf\xac\x84\x20\0\0\0\xf9\x9f\x01\x40\0\0\0\0a"[..], &[7u8; 100][..]].concat();
        let f = encode(&d, 1).unwrap();
        assert!(is(&f));
        assert_eq!((f.streams[0].data.len(), f.streams[1].data.len()), (17, 100));
        let none = |_: &aqr_formula::Apply, _: &[&[u8]]| -> aqr_formula::Result<Vec<u8>> { unreachable!("no module") };
        assert!(f.eval(&none).unwrap() == d);
        assert!(encode(&d[..17], 1).is_none());
    }
}
