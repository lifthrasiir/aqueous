//! Developer options (`-Z`) that `aqr`'s options take their defaults from are taken as given:
//! those read when the options were made, before `-Z` was, are read again after it.

use std::fs;
use std::process::Command;

/// A zlib stream of `data` in stored blocks (no compressor needed).
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut z = vec![0x78, 0x01];
    let mut chunks = data.chunks(65535).peekable();
    while let Some(c) = chunks.next() {
        z.push(chunks.peek().is_none() as u8);
        z.extend((c.len() as u16).to_le_bytes());
        z.extend((!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend(((b << 16) | a).to_be_bytes());
    z
}

/// A small zlib stream archives smaller as it is (its formula's module costs more than it
/// saves); with `-Z no-smaller` its formula is kept whatever it comes to, the module with it.
#[test]
fn no_smaller_taken() {
    let d = std::env::temp_dir().join(format!("aqr-zopt-{}", std::process::id()));
    _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    let text: String = (0..2000).map(|i| format!("line {i} of some text\n")).collect();
    let input = d.join("in.zz");
    fs::write(&input, zlib_stored(text.as_bytes())).unwrap();
    let archive = |name: &str, z: &[&str]| {
        let out = d.join(name);
        let o = Command::new(env!("CARGO_BIN_EXE_aqr"))
            .args(["-q", "-c", "-f", "-o"])
            .arg(&out)
            .args(z)
            .arg(&input)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        fs::metadata(&out).unwrap().len()
    };
    let (smaller, any) = (archive("a.aqr", &[]), archive("b.aqr", &["-Z", "no-smaller"]));
    assert!(any > smaller + (8 << 10), "{any} with -Z no-smaller, {smaller} without");
    fs::remove_dir_all(&d).unwrap();
}
