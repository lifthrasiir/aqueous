//! The frame of `aqr --ipc`'s messages (`man/aqripc.7`), both ways: a line of JSON (an object)
//! and, where it has `"blob": N` at its top, N bytes as they are right after its newline.
//!
//! - A request is `{"q":N,"op":OP,...}`, numbered by the side that sends it; its replies are
//!   `{"r":N,...}`, each but the last with `"more":true`. Both sides ask: `aqr`'s questions are
//!   its requests (`ask`).
//! - A failure is the last reply's `"error":{"kind":K,"message":M}` ([`kind`]).
//! - A path is a string, or `{"raw":HEX}` where it is not text ([`os_value`]).

use std::ffi::{OsStr, OsString};
use std::io::{BufRead, Write};

use serde_json::{Value, json};

/// The version of the protocol (the hello's).
pub const VERSION: u64 = 2;

/// The most bytes of a blob, unless the hello asked for more ([`CHUNK_MAX`] at most): a read
/// longer comes in replies of so many, for other replies to come between.
pub const CHUNK: usize = 1 << 20;
pub const CHUNK_MAX: usize = 4 << 20;

/// The kinds of failure (`error.kind`).
pub mod kind {
    pub const NOT_KNOWN: &str = "not-known";
    pub const BAD_REQUEST: &str = "bad-request";
    pub const BUSY: &str = "busy";
    pub const CANCELLED: &str = "cancelled";
    pub const NOT_FOUND: &str = "not-found";
    pub const NOT_A_DIRECTORY: &str = "not-a-directory";
    pub const NOT_A_FILE: &str = "not-a-file";
    pub const NO_ARCHIVE: &str = "no-archive";
    pub const ENCRYPTED: &str = "encrypted";
    pub const WRONG_PASSPHRASE: &str = "wrong-passphrase";
    pub const FAILED: &str = "failed";
}

/// A failure, as a reply's `error`.
pub fn error(kind: &str, message: impl Into<String>) -> Value {
    json!({"kind": kind, "message": message.into()})
}

/// Writes message `v` and its blob (`"blob"` set where there is one), flushed.
pub fn write(out: &mut impl Write, v: &Value, blob: &[u8]) -> std::io::Result<()> {
    match blob.is_empty() {
        true => writeln!(out, "{v}")?,
        false => {
            let mut v = v.clone();
            v["blob"] = blob.len().into();
            writeln!(out, "{v}")?;
            out.write_all(blob)?;
        }
    }
    out.flush()
}

/// The next message of `input` and its blob; `None` at its end. A line that is not an object of
/// JSON is a message of nothing (`Null`), to be ignored.
pub fn read(input: &mut impl BufRead) -> std::io::Result<Option<(Value, Vec<u8>)>> {
    let mut line = Vec::new();
    if input.read_until(b'\n', &mut line)? == 0 {
        return Ok(None);
    }
    let v: Value = serde_json::from_slice(&line).ok().filter(Value::is_object).unwrap_or(Value::Null);
    let mut blob = vec![0; v["blob"].as_u64().unwrap_or(0) as usize];
    input.read_exact(&mut blob)?;
    Ok(Some((v, blob)))
}

/// `s` as a message has it: a string, or `{"raw":HEX}` where it is not text (of Windows, its
/// units of UTF-16 little-endian; elsewhere, its bytes).
pub fn os_value(s: &OsStr) -> Value {
    if let Some(t) = s.to_str() {
        return t.into();
    }
    #[cfg(windows)]
    let bytes: Vec<u8> = std::os::windows::ffi::OsStrExt::encode_wide(s).flat_map(u16::to_le_bytes).collect();
    #[cfg(unix)]
    let bytes: Vec<u8> = std::os::unix::ffi::OsStrExt::as_bytes(s).to_vec();
    #[cfg(not(any(windows, unix)))]
    let bytes: Vec<u8> = s.to_string_lossy().into_owned().into_bytes();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    json!({"raw": hex})
}

/// What [`os_value`] made, again; `None` where it is neither.
pub fn os_of(v: &Value) -> Option<OsString> {
    if let Some(s) = v.as_str() {
        return Some(s.into());
    }
    let hex = v["raw"].as_str()?;
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Vec<u8> =
        (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok()).collect::<Option<_>>()?;
    #[cfg(windows)]
    {
        if !bytes.len().is_multiple_of(2) {
            return None;
        }
        let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        Some(std::os::windows::ffi::OsStringExt::from_wide(&units))
    }
    #[cfg(unix)]
    {
        Some(std::os::unix::ffi::OsStringExt::from_vec(bytes))
    }
    #[cfg(not(any(windows, unix)))]
    {
        String::from_utf8(bytes).ok().map(OsString::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames() {
        let mut out = Vec::new();
        write(&mut out, &json!({"r": 1, "more": true}), b"ab\ncd").unwrap();
        write(&mut out, &json!({"r": 1}), b"").unwrap();
        out.extend_from_slice(b"not json\n[1]\n");
        let mut input = &out[..];
        let (v, b) = read(&mut input).unwrap().unwrap();
        assert_eq!((v, b), (json!({"r": 1, "more": true, "blob": 5}), b"ab\ncd".to_vec()));
        assert_eq!(read(&mut input).unwrap().unwrap(), (json!({"r": 1}), Vec::new()));
        assert_eq!(read(&mut input).unwrap().unwrap(), (Value::Null, Vec::new()));
        assert_eq!(read(&mut input).unwrap().unwrap(), (Value::Null, Vec::new()));
        assert!(read(&mut input).unwrap().is_none());
        // (a blob cut short is an error)
        let mut short = &b"{\"blob\":4}\nab"[..];
        assert!(read(&mut short).is_err());
    }

    #[test]
    fn paths() {
        assert_eq!(os_value(OsStr::new("a/b c")), json!("a/b c"));
        assert_eq!(os_of(&json!("a/b c")).unwrap(), "a/b c");
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            let lone = OsString::from_wide(&[0x61, 0xd800, 0x62]);
            let v = os_value(&lone);
            assert_eq!(v, json!({"raw": "610000d86200"}));
            assert_eq!(os_of(&v).unwrap(), lone);
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let bad = OsString::from_vec(vec![0x61, 0xff]);
            let v = os_value(&bad);
            assert_eq!(v, json!({"raw": "61ff"}));
            assert_eq!(os_of(&v).unwrap(), bad);
        }
        assert!(os_of(&json!({"raw": "6"})).is_none());
        assert!(os_of(&json!(3)).is_none());
    }
}
