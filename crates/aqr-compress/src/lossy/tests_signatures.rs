//! Tests of `signatures`.

use super::signatures::{canonical, crc24, hash};
use super::*;
use std::process::{Command, Stdio};

fn check(name: &str, data: &[u8], sibs: &[(&str, &[u8])]) -> Option<String> {
    let mut names: HashMap<&[u8], usize> = HashMap::new();
    for (i, (n, _)) in sibs.iter().enumerate() {
        names.insert(n.as_bytes(), i);
    }
    let read = |i: usize| Some(sibs[i].1.to_vec());
    signatures::signed(name.as_bytes(), data, &names, &read)
}

/// A v4 signature packet (new format) of `hash_alg` whose prefix is that of the hash of `data`.
fn v4_packet(data: &[u8], sigtype: u8, hash_alg: u8, good: bool) -> Vec<u8> {
    let hashed = [5u8, 2, 0, 0, 0, 1]; // a creation time subpacket
    let mut body = vec![4, sigtype, 22, hash_alg, 0, hashed.len() as u8];
    body.extend_from_slice(&hashed);
    let mut trailer = body.clone();
    trailer.extend_from_slice(&[4, 0xFF]);
    trailer.extend_from_slice(&(body.len() as u32).to_be_bytes());
    let text = if sigtype == 1 { canonical(data) } else { data.to_vec() };
    let h = hash(hash_alg, &[&text, &trailer]).unwrap_or(vec![0, 0]);
    body.extend_from_slice(&[0, 0]); // no unhashed subpackets
    body.push(h[0] ^ if good { 0 } else { 1 });
    body.push(h[1]);
    body.extend_from_slice(&[0, 8, 1]); // an MPI of garbage
    let mut out = vec![0xC2, body.len() as u8];
    out.extend_from_slice(&body);
    out
}

#[test]
fn hand_made_prefix() {
    let data = b"hello\nworld\n";
    for alg in [1u8, 2, 8, 9, 10, 11] {
        let good = v4_packet(data, 0, alg, true);
        assert!(check("f", data, &[("f.sig", &good)]).unwrap().contains("f.sig (OpenPGP)"));
        assert!(check("f", b"hello\nworld!", &[("f.sig", &good)]).is_none());
        let bad = v4_packet(data, 0, alg, false);
        assert!(check("f", data, &[("f.sig", &bad)]).is_none());
    }
    // Text mode: the CR LF form.
    let text = v4_packet(data, 1, 8, true);
    assert!(check("f", data, &[("f.gpg", &text)]).is_some());
    assert!(check("f", b"hello\r\nworld\r\n", &[("f.gpg", &text)]).is_some());
    // Old format packet header (tag 2, two-byte length).
    let p = v4_packet(data, 0, 2, true);
    let mut old = vec![0x89, 0, (p.len() - 2) as u8];
    old.extend_from_slice(&p[2..]);
    assert!(check("f", data, &[("f.sig", &old)]).is_some());
    // Another name, or another suffix: nothing.
    assert!(check("g", data, &[("f.sig", &p)]).is_none());
    assert!(check("f", data, &[("f.sig2", &p)]).is_none());
}

#[test]
fn unknown_hash_and_version_are_conservative() {
    let data = b"x";
    let mut p = v4_packet(data, 0, 3, true); // RIPEMD-160
    assert!(check("f", data, &[("f.sig", &p)]).is_some());
    p = v4_packet(data, 0, 8, false);
    p[2] = 5; // a version 5
    assert!(check("f", data, &[("f.sig", &p)]).is_some());
    // Other types (a certification) are not of a file.
    let mut q = v4_packet(data, 0, 8, true);
    q[3] = 0x13;
    assert!(check("f", data, &[("f.sig", &q)]).is_none());
}

#[test]
fn garbage_is_nothing() {
    assert!(check("f", b"x", &[("f.sig", b"not a signature at all")]).is_none());
    assert!(check("f", b"x", &[("f.sig", b"")]).is_none());
    assert!(check("f", b"x", &[("f.sig", &[0x89, 0xFF, 1])]).is_none());
}

fn armor(bin: &[u8], crc_delta: u32) -> Vec<u8> {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let enc = |d: &[u8]| {
        let mut s = String::new();
        for c in d.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= c.len() {
                    s.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    s.push('=');
                }
            }
        }
        s
    };
    let crc = crc24(bin) ^ crc_delta;
    format!(
        "-----BEGIN PGP SIGNATURE-----\nVersion: test\n\n{}\n={}\n-----END PGP SIGNATURE-----\n",
        enc(bin),
        enc(&crc.to_be_bytes()[1..])
    )
    .into_bytes()
}

#[test]
fn armor_and_crc() {
    let data = b"data";
    let bin = v4_packet(data, 0, 8, true);
    assert!(check("f", data, &[("f.asc", &armor(&bin, 0))]).is_some());
    // A wrong CRC-24: not taken as a signature.
    assert!(check("f", data, &[("f.asc", &armor(&bin, 1))]).is_none());
}

#[test]
fn minisign_shape() {
    let b64 = |n: usize, head: &[u8]| {
        let mut v = head.to_vec();
        v.resize(n, 0x5A);
        let mut s = String::new();
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        for c in v.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                s.push(if i <= c.len() { A[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
            }
        }
        s
    };
    let good = format!(
        "untrusted comment: signature from minisign secret key\n{}\ntrusted comment: timestamp:1\n{}\n",
        b64(74, b"Ed"),
        b64(64, b"")
    );
    assert!(check("a/f", b"x", &[("a/f.minisig", good.as_bytes())]).unwrap().contains("(minisign)"));
    let bad_len = good.replace(&b64(64, b""), &b64(63, b""));
    assert!(check("a/f", b"x", &[("a/f.minisig", bad_len.as_bytes())]).is_none());
    let bad_algo = good.replace(&b64(74, b"Ed"), &b64(74, b"XX"));
    assert!(check("a/f", b"x", &[("a/f.minisig", bad_algo.as_bytes())]).is_none());
    assert!(check("a/f", b"x", &[("a/f.minisig", b"hello\n")]).is_none());
}

fn gpg(home: &std::path::Path, args: &[&str]) -> bool {
    Command::new("gpg")
        .env("GNUPGHOME", home)
        .args(["--batch", "--yes", "--pinentry-mode", "loopback", "--passphrase", ""])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn real_gpg() {
    let home = std::env::temp_dir().join(format!("aqr-sig-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let result = std::panic::catch_unwind(|| real_gpg_in(&home));
    let _ = std::fs::remove_dir_all(&home);
    result.unwrap();
}

fn real_gpg_in(home: &std::path::Path) {
    if !gpg(home, &["--quick-gen-key", "Test <t@example.invalid>", "ed25519", "sign", "never"]) {
        eprintln!("gpg unavailable: skipped");
        return;
    }
    let file = home.join("doc.txt");
    let mut data = b"line one\nline two\n".repeat(100);
    std::fs::write(&file, &data).unwrap();
    let f = file.to_str().unwrap();
    let mut other = data.clone();
    other[5] ^= 1;
    for digest in ["SHA256", "SHA512", "SHA1"] {
        for (armor, ext) in [(false, "sig"), (true, "asc")] {
            let out = home.join(format!("doc.txt.{ext}"));
            let _ = std::fs::remove_file(&out);
            let mut args = vec!["--digest-algo", digest, "--detach-sign"];
            if armor {
                args.push("--armor");
            }
            args.push(f);
            assert!(gpg(home, &args), "gpg sign failed");
            let sig = std::fs::read(&out).unwrap();
            let sib = format!("doc.txt.{ext}");
            assert!(check("doc.txt", &data, &[(&sib, &sig)]).is_some(), "{digest} {ext}");
            assert!(check("doc.txt", &other, &[(&sib, &sig)]).is_none(), "{digest} {ext} other");
        }
    }
    // Text mode (type 0x01) of a file with LF ends.
    let out = home.join("doc.txt.sig");
    let _ = std::fs::remove_file(&out);
    assert!(gpg(home, &["--textmode", "--detach-sign", f]));
    let sig = std::fs::read(&out).unwrap();
    assert!(check("doc.txt", &data, &[("doc.txt.sig", &sig)]).is_some(), "textmode");
    data[0] ^= 1;
    assert!(check("doc.txt", &data, &[("doc.txt.sig", &sig)]).is_none());
}
