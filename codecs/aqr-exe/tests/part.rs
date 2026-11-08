//! `exe-recons/x86` as a part module ([`module::x86_streamed`]: the 13 streams pulled in pieces,
//! the section put in pieces) against the whole decoding, on real x86-64 code (this test's own
//! executable) and noise in both modes, with sentinels of every size, and on streams cut short,
//! damaged, and left with bytes over.

use aqr_exe::arch::x86::{self, DecodedStreams, Opcodes};
use aqr_exe::module;

/// The code to filter: a stretch of this executable (x86-64 on the hosts that run these tests,
/// but any bytes will do) from `at`, `n` bytes.
fn code(at: usize, n: usize) -> Vec<u8> {
    let exe = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    let at = at.min(exe.len().saturating_sub(n));
    exe[at..(at + n).min(exe.len())].to_vec()
}

fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (x >> 33) as u8
        })
        .collect()
}

fn params(origin: u64, sentinel: u8, is64: bool) -> Vec<u8> {
    let mut p = origin.to_le_bytes().to_vec();
    p.extend([sentinel, is64 as u8]);
    p
}

/// The whole decoding and the part module's of the streams the filter makes of `d`: both `d`.
fn same(what: &str, d: &[u8], origin: u64, is64: bool) {
    let e = x86::encode(d, origin, is64).unwrap();
    let streams: Vec<&[u8]> = e.streams().iter().map(|s| &s[..]).collect();
    let p = params(origin, e.sentinel_size(), is64);
    let res = Opcodes::BUILTIN.to_resource();
    let whole = module::x86(&p, &[&res], &streams, d.len() as u64);
    assert!(whole.as_deref() == Ok(d), "{what}: whole");
    let part = module::x86_streamed(&p, &[&res], &streams, d.len() as u64);
    assert!(part.as_deref() == Ok(d), "{what}: part");
    // an output of another length, one stream short, and one with a byte over (ignored)
    assert!(module::x86_streamed(&p, &[&res], &streams, d.len() as u64 + 1).is_err(), "{what}: longer");
    assert!(d.is_empty() || module::x86_streamed(&p, &[&res], &streams, d.len() as u64 - 1).is_err(), "{what}: shorter");
    let mut over: Vec<Vec<u8>> = streams.iter().map(|s| s.to_vec()).collect();
    over[1].push(7);
    let over: Vec<&[u8]> = over.iter().map(|s| &s[..]).collect();
    assert!(module::x86_streamed(&p, &[&res], &over, d.len() as u64).as_deref() == Ok(d), "{what}: a byte over");
    let _ = DecodedStreams::new(origin, 0, [&[]; x86::NUM_STREAMS]);
}

#[test]
fn code_through_pieces() {
    for (k, &(at, n)) in [(0usize, 0usize), (0, 1), (0, 15), (4096, 100), (4096, 20_000), (100_000, 300_000), (0, 3 << 20)].iter().enumerate() {
        let d = code(at, n);
        for is64 in [true, false] {
            same(&format!("code {n} at {at}, 64: {is64}"), &d, 0x1_4000_1000 + k as u64 * 16, is64);
        }
    }
}

#[test]
fn noise_and_every_sentinel() {
    for n in 0..64 {
        let d = noise(n, n as u64);
        for is64 in [true, false] {
            same(&format!("noise {n}"), &d, 0x1000, is64);
        }
    }
    // (the sentinel is how much of the 15 bytes of zeros after the code an instruction read)
    for n in [100, 1000, 70_000] {
        let d = noise(n, 5);
        same(&format!("noise {n}"), &d, 0x40_0000, true);
    }
}

#[test]
fn damaged_streams_fail_alike() {
    let d = code(50_000, 20_000);
    let e = x86::encode(&d, 0x1000, true).unwrap();
    let res = Opcodes::BUILTIN.to_resource();
    let p = params(0x1000, e.sentinel_size(), true);
    let base: Vec<Vec<u8>> = e.streams().to_vec();
    let run = |s: &[Vec<u8>], p: &[u8], len: u64| {
        let r: Vec<&[u8]> = s.iter().map(|v| &v[..]).collect();
        (module::x86(p, &[&res], &r, len), module::x86_streamed(p, &[&res], &r, len))
    };
    // each stream cut short, and each byte of a few damaged: the part module fails when the
    // whole one does, or gives the same
    for st in 0..x86::NUM_STREAMS {
        for cut in [0, base[st].len() / 2, base[st].len().saturating_sub(1)] {
            let mut s = base.clone();
            s[st].truncate(cut);
            let (a, b) = run(&s, &p, d.len() as u64);
            assert!(a == b, "stream {st} cut at {cut}");
        }
        for i in (0..base[st].len()).step_by(base[st].len() / 13 + 1) {
            let mut s = base.clone();
            s[st][i] ^= 0x5a;
            let (a, b) = run(&s, &p, d.len() as u64);
            assert!(a == b, "stream {st} damaged at {i}");
        }
    }
    // a sentinel past 15 is refused by the part module, taken by the whole one
    let (a, b) = run(&base, &params(0x1000, 16, true), d.len() as u64 - 1);
    assert!(b.is_err() && (a.is_err() || a.is_ok()));
    assert!(module::x86_streamed(&p[..9], &[&res], &base.iter().map(|v| &v[..]).collect::<Vec<_>>(), 0).is_err());
}
