//! The range coder's end of input: zeros past it are counted, and a model reset like a new one.

use aqr_walk::model::Model;
use aqr_walk::rc::{Coder, Decoder, Encoder, SliceSource, Source};

fn xorshift(s: &mut u64) -> u64 {
    *s ^= *s << 13;
    *s ^= *s >> 7;
    *s ^= *s << 17;
    *s
}

/// `n` bits of a skewed source coded under a few contexts of a model.
fn code<C: Coder>(rc: &mut C, bits: &[u32]) -> Vec<u32> {
    let mut m = Model::new(12);
    let mut out = Vec::new();
    for (i, &b) in bits.iter().enumerate() {
        out.push(rc.bit(m.slot((i % 7) as u32, (i % 3) as u32), b));
        if i % 5 == 0 {
            out.push(rc.flat(b));
        }
    }
    out
}

#[test]
fn a_valid_stream_is_not_read_past_its_end() {
    let mut s = 1u64;
    for n in [0usize, 1, 2, 7, 100, 1000, 20000] {
        for skew in [1u64, 8, 64] {
            let bits: Vec<u32> = (0..n).map(|_| xorshift(&mut s).is_multiple_of(skew) as u32).collect();
            let mut enc = Encoder::new();
            let coded = code(&mut enc, &bits);
            let data = enc.finish();
            let mut src = SliceSource::new(&data);
            let mut dec = Decoder::new(&mut src);
            let got = code(&mut dec, &vec![0; n]);
            assert_eq!(got, coded, "n {n} skew {skew}");
            assert!(!dec.past_end(), "n {n} skew {skew}: {} bytes", data.len());
            // (the encoder flushes all that the decoder reads)
            assert_eq!(src.over(), 0, "n {n} skew {skew}");
        }
    }
}

/// A valid stream cut by even one byte is told past its end once all of it is decoded: what a
/// decoder reads does not depend on the bytes, so it asks as many as were written.
#[test]
fn a_stream_cut_short_is_past_its_end() {
    let mut s = 5u64;
    for n in [1usize, 50, 3000] {
        let bits: Vec<u32> = (0..n).map(|_| xorshift(&mut s).is_multiple_of(3) as u32).collect();
        let mut enc = Encoder::new();
        code(&mut enc, &bits);
        let data = enc.finish();
        for cut in 1..=5.min(data.len()) {
            let mut src = SliceSource::new(&data[..data.len() - cut]);
            let mut dec = Decoder::new(&mut src);
            code(&mut dec, &vec![0; n]);
            assert!(dec.past_end(), "n {n}, {cut} of {} bytes cut", data.len());
            assert_eq!(src.over(), cut, "n {n}");
        }
    }
}

#[test]
fn zeros_past_the_end_are_counted() {
    // eight bytes decode as many bits as asked, but not unnoticed (three are past it at once: the
    // decoder starts by reading four)
    let mut dec = Decoder::new(SliceSource::new(&[0u8, 0, 0]));
    assert!(dec.past_end());
    let data = [0u8; 8];
    let mut dec = Decoder::new(SliceSource::new(&data));
    assert!(!dec.past_end());
    let mut m = Model::new(12);
    let mut asked = 0;
    while !dec.past_end() {
        dec.bit(m.slot(0, 0), 0);
        asked += 1;
        assert!(asked < 100_000_000, "never past the end");
    }
    assert!(asked > 0);
    // an empty source is past its end as soon as the coder is started and a few bytes are read
    let mut dec = Decoder::new(SliceSource::new(&[]));
    for _ in 0..64 {
        dec.flat(0);
    }
    assert!(dec.past_end());
}

#[test]
fn a_reset_model_is_a_new_one() {
    let mut s = 3u64;
    for bits in [4u32, 9, 10, 11, 14] {
        let mut used = Model::new(bits);
        let junk: Vec<u32> = (0..3000).map(|_| xorshift(&mut s) as u32).collect();
        let mut enc = Encoder::new();
        for &j in &junk {
            enc.bit(used.slot(j >> 3, j & 7), j >> 20 & 1);
        }
        used.reset();
        // coding with the reset model gives what a new model gives
        let bits_in: Vec<u32> = (0..2000).map(|_| xorshift(&mut s).is_multiple_of(5) as u32).collect();
        let (mut a, mut b) = (Encoder::new(), Encoder::new());
        let mut fresh = Model::new(bits);
        for (i, &x) in bits_in.iter().enumerate() {
            a.bit(used.slot(i as u32 % 11, i as u32 % 5), x);
            b.bit(fresh.slot(i as u32 % 11, i as u32 % 5), x);
        }
        assert_eq!(a.finish(), b.finish(), "bits {bits}");
    }
}

#[test]
fn resetting_costs_what_was_used() {
    // a model of 16 MiB reset 100000 times after a handful of bits each: it was 2 ms each (a minute in all)
    let mut m = Model::new(22);
    let mut enc = Encoder::new();
    let t = std::time::Instant::now();
    for i in 0..100_000u32 {
        for k in 0..4 {
            enc.bit(m.slot(i, k), k & 1);
        }
        m.reset();
    }
    let took = t.elapsed();
    assert!(took.as_secs_f64() < 2.0, "{took:?}");
}
