//! Edge cases of the streams: damaged data and records, block headers lying about sizes, the x86
//! filter on stored blocks.

use std::time::Duration;

use aqr_azo::azo::{self, Token};
use aqr_azo::{module, recons};
use aqr_common::rng::Rng;

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 28);

/// A stream of blocks: (data, tokens or None for a stored block), the x86 flag as given.
fn stream(x86: bool, blocks: &[(Vec<u8>, Option<Vec<Token>>)]) -> Vec<u8> {
    let mut s = vec![azo::VERSION, x86 as u8];
    let put = |s: &mut Vec<u8>, n: usize, code: &[u8]| {
        for v in [n, code.len(), n ^ code.len()] {
            s.extend_from_slice(&(v as u32).to_be_bytes());
        }
        s.extend_from_slice(code);
    };
    for (d, t) in blocks {
        match t {
            Some(t) => put(&mut s, d.len(), &azo::encode_block(d, t, &mut azo::Model::default()).unwrap()),
            None => put(&mut s, d.len(), d),
        }
    }
    put(&mut s, 0, &[]);
    s
}

/// A stream's header of a block lying: its size, its code's size (the check made right).
fn header(size: u32, csize: u32) -> [u8; 12] {
    let mut h = [0; 12];
    h[..4].copy_from_slice(&size.to_be_bytes());
    h[4..8].copy_from_slice(&csize.to_be_bytes());
    h[8..].copy_from_slice(&(size ^ csize).to_be_bytes());
    h
}

/// The first byte (put without a token), two literals, then matches of the period 3 in pieces of 50.
fn coded(n: usize) -> (Vec<u8>, Vec<Token>) {
    let d: Vec<u8> = (0..n).map(|i| b"abc"[i % 3]).collect();
    let mut t = vec![Token::Literal; 2];
    let mut at = 3;
    while at < n {
        let k = 50.min(n - at);
        t.push(Token::Far(3, k as u32));
        at += k;
    }
    (d, t)
}

/// Everything made of `d` is an error or right; nothing panics, hangs or reserves what `d` cannot
/// hold.
fn check(d: &[u8]) {
    let limit = 1 << 20;
    if let Ok(sp) = recons::split(d, limit, 1) {
        // a stream taken rebuilds
        assert!(recons::join(&sp.data, &sp.record, 1).as_deref() == Ok(&d[..sp.used]), "split and join");
    }
    let _ = azo::decode(d, limit);
    let _ = azo::parse(d);
    assert!(aqr_testkit::most() < 64 << 20, "an allocation of {} bytes", aqr_testkit::most());
}

fn sample() -> Vec<u8> {
    let (d1, t1) = coded(700);
    let (d2, t2) = coded(300);
    let mut rng = Rng::new(3);
    let noise: Vec<u8> = (0..700).map(|_| rng.below(256) as u8).collect();
    stream(false, &[(d1, Some(t1)), (noise, None), (d2, Some(t2))])
}

#[test]
fn damaged_stream() {
    let s = sample();
    assert!(recons::split(&s, 1 << 20, 1).is_ok());
    aqr_testkit::damaged(&s, 200, 40, Duration::from_secs(20), check);
}

/// A record damaged, or cut, and the data damaged: the stream is rebuilt, or it is an error.
#[test]
fn damaged_record_and_data() {
    let s = sample();
    let sp = recons::split(&s, 1 << 20, 1).unwrap();
    aqr_testkit::damaged(&sp.record, 200, 40, Duration::from_secs(20), |r| {
        let _ = recons::join(&sp.data, r, 1);
        let _ = recons::block_sizes(&sp.data, r);
        let _ = module::decode(&[], &[&sp.data, r, &[]], 1000, 1);
    });
    aqr_testkit::damaged(&sp.data, 200, 40, Duration::from_secs(20), |d| {
        let _ = recons::join(d, &sp.record, 1);
        let _ = recons::block_sizes(d, &sp.record);
    });
}

/// Block headers lying about their sizes (checks made right): the stream is refused, and nothing
/// is reserved for the claim.
#[test]
fn sizes_lying() {
    let (d, t) = coded(200);
    let good = stream(false, &[(d.clone(), Some(t))]);
    let first = azo::HEADER;
    let code_len = u32::from_be_bytes(good[first + 4..first + 8].try_into().unwrap());
    for size in [1, 199, 201, 1 << 19, (1 << 19) + 1, 1 << 28, 0x7fff_ffff, u32::MAX] {
        // the same code, the size claimed otherwise
        let mut s = good.clone();
        s[first..first + 12].copy_from_slice(&header(size, code_len));
        aqr_testkit::case(&format!("size {size}"));
        check(&s);
        if size != 200 {
            assert!(recons::split(&s, usize::MAX, 1).is_err(), "split {size}");
            // (decode stops at the size, ignoring the code after it: split re-encodes)
        }
    }
    // the code's size claimed otherwise: past the stream, or short
    for csize in [0, 1, code_len - 1, code_len + 1, u32::MAX] {
        let mut s = good.clone();
        s[first..first + 12].copy_from_slice(&header(200, csize));
        check(&s);
    }
    // a thousand blocks' headers of the largest size each, one byte of code each: 512 MiB claimed
    // by 13 KB (the limit given is what bounds the decoding, nothing is reserved before the first
    // block fails)
    let mut s = vec![azo::VERSION, 0];
    for _ in 0..1000 {
        s.extend_from_slice(&header(512 << 10, 1));
        s.push(0);
    }
    s.extend_from_slice(&header(0, 0));
    aqr_testkit::case("a thousand big blocks");
    assert!(recons::split(&s, usize::MAX, 1).is_err());
    assert!(azo::decode(&s, usize::MAX).is_err());
    assert!(recons::split(&s, usize::MAX, 4).is_err());
}

/// A stored block of a stream with the x86 filter: its data is the unfiltered block where the
/// filter gives it back; a stored block the filter does not invert is refused (not taken
/// wrongly).
#[test]
fn x86_stored_blocks() {
    // E8 or E9 with addresses that the filter converts, and not, runs of them next to each other
    let mut rng = Rng::new(11);
    for round in 0..40 {
        let n = [5, 6, 9, 64, 1000][round % 5];
        let alphabet = [0xE8u8, 0xE9, 0x00, 0xFF, 0x01, 0x7f, 0x80];
        let d: Vec<u8> = (0..n).map(|_| alphabet[rng.below(alphabet.len() as u64) as usize]).collect();
        let s = stream(true, &[(d.clone(), None)]);
        match recons::split(&s, usize::MAX, 1) {
            Ok(sp) => {
                // the data is what the stream decodes to, and it rebuilds the stream
                assert!(sp.data == azo::decode(&s, usize::MAX).unwrap().data, "{round}: data");
                assert!(recons::join(&sp.data, &sp.record, 1).unwrap() == s, "{round}: join");
            }
            Err(_) => {
                // refused: the filter does not give the block back
                let mut u = d.clone();
                azo::unfilter(&mut u);
                let mut again = u.clone();
                azo::filter(&mut again);
                assert!(again != d, "{round}: a block that the filter gives back was refused");
            }
        }
    }
    // a block that the filter does not give back: a converted address whose bit 24 is clear
    // while its high byte is 0xFF (the unfilter extends from bit 24, losing the byte)
    let d = vec![0xE8, 0x00, 0x00, 0x00, 0xFF, 0x90, 0x90, 0x90, 0x90];
    let mut u = d.clone();
    azo::unfilter(&mut u);
    let mut again = u.clone();
    azo::filter(&mut again);
    let s = stream(true, &[(d.clone(), None)]);
    assert_eq!(recons::split(&s, usize::MAX, 1).is_ok(), again == d);
}
