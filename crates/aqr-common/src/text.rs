//! Bytes as text.

/// Bytes `b` as lowercase hexadecimal, two digits each.
pub fn hex(b: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(DIGITS[(x >> 4) as usize] as char);
        s.push(DIGITS[(x & 15) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn hex_is_lowercase_two_digits() {
        assert_eq!(super::hex(&[]), "");
        assert_eq!(super::hex(&[0, 1, 0x0f, 0xa0, 0xff]), "00010fa0ff");
    }
}
