//! Peak heap use of taking apart a tile-compressed image whose header claims far more pixels
//! than its data could hold: it is not taken, nor its image held. (A test binary of its own:
//! the allocator counts every thread.)

use aqr_common::mem::{Counting, peak};

#[global_allocator]
static A: Counting = Counting;

fn card(s: &str) -> Vec<u8> {
    format!("{s:<80}").into_bytes()
}

fn header(cards: &[String]) -> Vec<u8> {
    let mut h: Vec<u8> = cards.iter().flat_map(|c| card(c)).collect();
    h.extend(card("END"));
    h.resize(h.len().next_multiple_of(2880), b' ');
    h
}

fn kv(k: &str, v: impl std::fmt::Display) -> String {
    format!("{k:<8}= {v:>20}")
}

#[test]
fn claimed_too_large() {
    let mut d = header(&[kv("SIMPLE", "T"), kv("BITPIX", 8), kv("NAXIS", 0)]);
    // one tile of 2^31 pixels, in a stream of 4 KiB of zeros
    d.extend(header(&[
        kv("XTENSION", "'BINTABLE'"),
        kv("BITPIX", 8),
        kv("NAXIS", 2),
        kv("NAXIS1", 8),
        kv("NAXIS2", 1),
        kv("PCOUNT", 4096),
        kv("GCOUNT", 1),
        kv("TFIELDS", 1),
        kv("TFORM1", "'1PB     '"),
        kv("TTYPE1", "'COMPRESSED_DATA'"),
        kv("ZIMAGE", "T"),
        kv("ZCMPTYPE", "'RICE_1  '"),
        kv("ZBITPIX", 32),
        kv("ZNAXIS", 2),
        kv("ZNAXIS1", 1 << 16),
        kv("ZNAXIS2", 1 << 15),
        kv("ZTILE1", 1 << 16),
        kv("ZTILE2", 1 << 15),
    ]));
    let mut data = vec![0u8; 8 + 4096];
    data[..4].copy_from_slice(&4096u32.to_be_bytes());
    d.extend(data);
    d.resize(d.len().next_multiple_of(2880), 0);
    let (f, bytes) = peak(|| aqr_fits::formula::encode(&d, 1).unwrap());
    assert!(f.applies().is_empty());
    assert!(bytes < 64 << 20, "{bytes}");
}
