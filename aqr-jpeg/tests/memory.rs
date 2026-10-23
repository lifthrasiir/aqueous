//! Peak heap use of coding a large sequential JPEG: its coefficients are only ever held a
//! stripe at a time, so memory must not follow the pixels. (A test binary of its own: the
//! allocator counts every thread.)

use aqr_formula::Apply;
use aqr_jpeg::formula::{self, MODULE};
use aqr_jpeg::jpeg::{self, Coefs, ScanInfo};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(n: usize) {
    PEAK.fetch_max(LIVE.fetch_add(n, Relaxed) + n, Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            grew(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            grew(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        LIVE.fetch_sub(l.size(), Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, n) };
        if !q.is_null() {
            if n > l.size() {
                grew(n - l.size());
            } else {
                LIVE.fetch_sub(l.size() - n, Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Peak heap bytes above the current level while running `f`.
fn peak<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    let r = f();
    (r, PEAK.load(Relaxed) - base)
}

fn segment(out: &mut Vec<u8>, marker: u8, body: &[u8]) {
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&(body.len() as u16 + 2).to_be_bytes());
    out.extend_from_slice(body);
}

/// A baseline 4:4:4 JPEG of `w` x `h` pixels (restart interval `ri` MCUs, 0 for none):
/// smooth DC and a few small low-frequency AC coefficients per block.
fn synthetic(w: usize, h: usize, ri: u16) -> Vec<u8> {
    let mut d = vec![0xFF, 0xD8];
    let mut dqt = vec![0u8];
    dqt.extend((0..64).map(|k| 2 + k as u8 / 4));
    segment(&mut d, 0xDB, &dqt);
    let mut sof = vec![8];
    sof.extend_from_slice(&(h as u16).to_be_bytes());
    sof.extend_from_slice(&(w as u16).to_be_bytes());
    sof.push(3);
    for id in 1..=3 {
        sof.extend_from_slice(&[id, 0x11, 0]);
    }
    segment(&mut d, 0xC0, &sof);
    // DC: the standard luminance table; AC: EOB, ZRL and the (run, 1) symbols
    let mut dht = vec![0x00, 0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
    dht.extend(0..12);
    dht.extend_from_slice(&[0x10, 0, 1, 2, 3, 6, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    dht.push(0x00);
    dht.extend((0..16).map(|r| r << 4 | 1));
    dht.push(0xF0);
    segment(&mut d, 0xC4, &dht);
    if ri > 0 {
        segment(&mut d, 0xDD, &ri.to_be_bytes());
    }
    segment(&mut d, 0xDA, &[3, 1, 0x00, 2, 0x00, 3, 0x00, 0, 63, 0]);
    let at = d.len();
    d.extend_from_slice(&[0xFF, 0xD9]);
    let j = jpeg::parse(&d).unwrap();
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    let coefs: Vec<Coefs> = j
        .comps
        .iter()
        .map(|c| {
            let n = c.bw * c.bh;
            let mut o = Coefs { bw: c.bw, bh: c.bh, y0: 0, blocks: vec![[0; 64]; n], masks: vec![0; n] };
            for i in 0..n {
                let (bx, by) = ((i % c.bw) as i32, (i / c.bw) as i32);
                let b = &mut o.blocks[i];
                b[0] = ((bx * 3 + by * 2) % 200 - 100) as i16;
                for _ in 0..3 {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    b[1 + (x % 9) as usize] = if x & 1024 != 0 { 1 } else { -1 };
                }
                o.masks[i] = b.iter().enumerate().fold(0, |m, (k, &v)| m | ((v != 0) as u64) << k);
            }
            o
        })
        .collect();
    let mut scan = Vec::new();
    let info = ScanInfo { standard: true, ..Default::default() };
    jpeg::encode_scan(&j, &j.scans[0], &info, &coefs, &mut scan).unwrap();
    d.splice(at..at, scan);
    d
}

fn module(a: &Apply, inputs: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    assert_eq!(a.module, MODULE);
    formula::decode(&a.params, inputs, a.len, 2).map_err(|e| aqr_formula::Error::new(e.0))
}

#[test]
fn sequential_memory_does_not_follow_pixels() {
    // 4096 x 2048, 3 components: 393216 blocks, 53 MB as whole coefficient arrays
    for ri in [0, 64] {
        let data = synthetic(4096, 2048, ri);
        let (f, enc) = peak(|| formula::encode(&data, 2).expect("not coded"));
        // the encoder's pool threads free their thread-locals as they exit
        std::thread::sleep(std::time::Duration::from_millis(200));
        let (back, dec) = peak(|| f.eval(&module).unwrap());
        assert!(back == data, "roundtrip (ri {ri})");
        let mb = |x: usize| x as f64 / 1e6;
        println!("ri {ri}: {:.2} MB file, peak {:.1} MB encoding, {:.1} MB decoding", mb(data.len()), mb(enc), mb(dec));
        // a few stripes' worth (2 workers) on top of what follows the file's size
        let bound = 4 * data.len() + (32 << 20);
        assert!(enc < bound && dec < bound, "ri {ri}: peak {enc} / {dec} bytes, over {bound}");
    }
}
