//! Text shown on a terminal: the names of files and of archives' entries may hold any character,
//! and a control character (an escape sequence moving the cursor, changing colours or the
//! title; a carriage return writing over a line; a line feed forging a line of a listing read by
//! a script) or one reordering text (bidirectional controls) would otherwise be the terminal's to
//! act on. What is told on standard error is escaped whole (`eprintln!` of the crate), the
//! listings on standard output too, and each name in them as one line. `--json` and the frontends'
//! messages are escaped as JSON instead.

use std::borrow::Cow;

/// Whether `c` is escaped: a control (C0, DEL, C1; line feeds and tabs but where `lines`) or a
/// bidirectional control.
fn escaped(c: char, lines: bool) -> bool {
    match c {
        '\n' | '\t' => !lines,
        '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' => true,
        '\u{61c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => true,
        _ => false,
    }
}

/// `s` as it may be shown on a terminal: each character [`escaped`] as `\u{...}` (Rust's escape),
/// a line feed and a tab kept where `lines` (text of several lines; `false` for a name).
pub fn safe(s: &str, lines: bool) -> Cow<'_, str> {
    if !s.chars().any(|c| escaped(c, lines)) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match escaped(c, lines) {
            true => out.extend(c.escape_unicode()),
            false => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// [`safe`] of a name or path, one line.
pub fn name(s: &str) -> Cow<'_, str> {
    safe(s, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_controls() {
        assert_eq!(safe("plain 이름.txt", false), "plain 이름.txt");
        assert_eq!(safe("a\x1b[2Jb", true), "a\\u{1b}[2Jb");
        assert_eq!(safe("a\nb\tc\r", true), "a\nb\tc\\u{d}");
        assert_eq!(safe("a\nb", false), "a\\u{a}b");
        assert_eq!(safe("\u{9b}31m\u{202e}txt.exe", false), "\\u{9b}31m\\u{202e}txt.exe");
    }
}
