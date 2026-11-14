//! The emulation of libzstd against libzstd itself (the `zstd` crate's, 1.5.7): every frame
//! made again byte for byte.

use aqr_zstd::compress::compress;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Text-like words, runs, noise, copies of what came before and small symbols, in a mix.
fn mixed(r: &mut Rng, n: usize, words: &[Vec<u8>]) -> Vec<u8> {
    let kind = r.below(6);
    let mut b = Vec::with_capacity(n + 300);
    while b.len() < n {
        let m = r.below(100);
        if kind == 0 || m < 30 {
            b.extend_from_slice(&words[r.below(words.len())]);
            b.push(b' ');
        } else if m < 50 {
            let v = r.next() as u8;
            b.extend(std::iter::repeat_n(v, 1 + r.below(40)));
        } else if m < 60 {
            for _ in 0..1 + r.below(50) {
                b.push(r.next() as u8);
            }
        } else if m < 80 && b.len() > 10 {
            let s = r.below(b.len());
            let l = (3 + r.below(300)).min(b.len() - s);
            b.extend_from_within(s..s + l);
        } else {
            for _ in 0..1 + r.below(100) {
                b.push((r.next() & 3) as u8);
            }
        }
    }
    b.truncate(n);
    b
}

/// Shuffled bytes of many symbols of the same count: ties in the Huffman sort's buckets.
fn ties(r: &mut Rng) -> Vec<u8> {
    let k = [150, 163, 164, 165, 166, 167, 170, 200, 255, 256, 300, 511, 512, 700, 1000][r.below(15)];
    let mut b = Vec::new();
    for _ in 0..9 + r.below(112) {
        let s = r.next() as u8;
        let c = if r.below(10) < 7 { k } else { 1 + r.below(3 * k) };
        b.extend(std::iter::repeat_n(s, c));
    }
    for i in (1..b.len()).rev() {
        b.swap(i, r.below(i + 1));
    }
    b.truncate(250_000);
    b
}

fn check(d: &[u8]) {
    let want = zstd::bulk::compress(d, 3).unwrap();
    let made = compress(d, 3).unwrap();
    assert!(made == want, "{} bytes: made {} bytes, libzstd {}", d.len(), made.len(), want.len());
}

#[test]
fn level3_as_libzstd() {
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    let words: Vec<Vec<u8>> =
        (0..300).map(|_| (0..2 + r.below(8)).map(|_| b"abcdefghijklmnop"[r.below(16)]).collect()).collect();
    let sizes = [0, 1, 2, 5, 7, 8, 9, 12, 20, 63, 64, 65, 100, 255, 256, 300, 1000, 1024, 4000, 16384, 16385];
    let more = [20000, 60000, 65791, 65792, 131072, 131073, 140000, 200000, 262143];
    for &n in sizes.iter().chain(&more) {
        for _ in 0..4 {
            check(&mixed(&mut r, n, &words));
        }
    }
    for _ in 0..300 {
        let n = (2f64.powf(r.below(18_000) as f64 / 1000.0)) as usize;
        check(&mixed(&mut r, n, &words));
    }
    for _ in 0..200 {
        check(&ties(&mut r));
    }
}

/// Levels of other strategies and contents that libzstd would split are not emulated.
#[test]
fn not_emulated() {
    assert!(compress(b"abc", 1).is_none() && compress(b"abc", 19).is_none());
    assert!(compress(&vec![7; 256 << 10], 3).is_none());
}
