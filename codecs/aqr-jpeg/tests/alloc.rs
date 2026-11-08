//! Allocations sized by what a file or an archive says (dimensions, sizes in the params, counts
//! in the scan info) are not made before they are checked against what the input or the declared
//! output can hold: the compressor takes any file, and a crafted archive fails as corrupt,
//! rather than aborting the process on a reservation of gigabytes. (A test binary of its own, with
//! the allocator refusing a single request of 1 GiB and above, as an exhausted heap does.)

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use aqr_formula::{Apply, Formula};
use aqr_jpeg::formula::{self, MODULE};
use aqr_jpeg::module;

const REFUSED: usize = 1 << 30;

/// The largest single request so far (of any thread).
static MOST: AtomicUsize = AtomicUsize::new(0);

struct Largest;

fn note(n: usize) -> bool {
    MOST.fetch_max(n, Relaxed);
    n < REFUSED
}

unsafe impl GlobalAlloc for Largest {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if note(l.size()) { unsafe { System.alloc(l) } } else { std::ptr::null_mut() }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        if note(l.size()) { unsafe { System.alloc_zeroed(l) } } else { std::ptr::null_mut() }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if note(n) { unsafe { System.realloc(p, l, n) } } else { std::ptr::null_mut() }
    }
}

#[global_allocator]
static ALLOC: Largest = Largest;

/// Runs `f` alone, and tells the largest request made meanwhile.
fn largest<R>(f: impl FnOnce() -> R) -> (R, usize) {
    static ONE: Mutex<()> = Mutex::new(());
    let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
    MOST.store(0, Relaxed);
    let r = f();
    (r, MOST.load(Relaxed))
}

fn module_fn(a: &Apply, inputs: &[&[u8]]) -> aqr_formula::Result<Vec<u8>> {
    assert_eq!(a.module, MODULE);
    formula::decode(&a.params, inputs, a.len, 0).map_err(|e| aqr_formula::Error::new(e.0))
}

fn segment(out: &mut Vec<u8>, marker: u8, body: &[u8]) {
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&(body.len() as u16 + 2).to_be_bytes());
    out.extend_from_slice(body);
}

/// A progressive grayscale JPEG of `w` x `h` pixels with a DC scan of a few bytes.
fn progressive_gray(w: u16, h: u16) -> Vec<u8> {
    let mut d = vec![0xFF, 0xD8];
    let mut dqt = vec![0u8];
    dqt.extend((0..64).map(|k| 2 + k as u8 / 4));
    segment(&mut d, 0xDB, &dqt);
    let mut sof = vec![8];
    sof.extend_from_slice(&h.to_be_bytes());
    sof.extend_from_slice(&w.to_be_bytes());
    sof.extend_from_slice(&[1, 1, 0x11, 0]);
    segment(&mut d, 0xC2, &sof);
    let mut dht = vec![0x00, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    dht.push(0);
    segment(&mut d, 0xC4, &dht);
    segment(&mut d, 0xDA, &[1, 1, 0x00, 0, 0, 0]);
    d.extend_from_slice(&[0x00; 16]);
    d.extend_from_slice(&[0xFF, 0xD9]);
    d
}

const GRADIENT: &[u8] = include_bytes!("data/gradient.jpg");
const PROGRESSIVE: &[u8] = include_bytes!("data/prog_script_recode.jpg");

/// A header giving 2^24 blocks (a plane of 2 GiB of coefficients) over a few bytes of data is
/// kept as it is, not reserved for.
#[test]
fn dimensions_beyond_the_data() {
    for (w, h) in [(32768, 32768), (32768, 16384)] {
        let d = progressive_gray(w, h);
        let (f, most) = largest(|| formula::encode(&d, 0));
        assert!(f.is_none());
        assert!(most < (64 << 20), "{w}x{h}: {most} bytes reserved");
    }
}

/// The inputs of the module's application of the formula `f` of `data`.
fn inputs(f: &Formula) -> (Apply, Vec<Vec<u8>>) {
    let a = f.applies()[0].clone();
    let ins = a
        .inputs
        .iter()
        .map(|n| Formula { streams: f.streams.clone(), root: n.clone(), shared: vec![] }.eval(&module_fn).unwrap())
        .collect();
    (a, ins)
}

fn decode(params: &[u8], ins: &[Vec<u8>], len: u64) -> Result<Vec<u8>, aqr_jpeg::jpeg::Error> {
    let refs: Vec<&[u8]> = ins.iter().map(|v| &v[..]).collect();
    formula::decode(params, &refs, len, 0)
}

/// Sizes and counts of the params and the scan info above what the declared output holds are
/// corrupt.
#[test]
fn archived_sizes_beyond_the_output() {
    let f = formula::encode(GRADIENT, 0).unwrap();
    let (a, ins) = inputs(&f);
    let len = a.len;
    assert_eq!(decode(&a.params, &ins, len).unwrap(), GRADIENT);
    let (progressive, sizes) = module::params(&a.params).unwrap();
    let big = 1u64 << 40;
    type Case = (&'static str, Vec<u8>, Vec<Vec<u8>>, u64);
    let mut cases: Vec<Case> = Vec::new();
    // the output itself is asked for and refused (the request is the length)
    let (r, _) = largest(|| decode(&a.params, &ins, big));
    assert!(r.is_err(), "output length: decoded");
    // a stripe's bits
    let mut z = sizes.clone();
    z.stripes[0].0 = 1 << 44;
    cases.push(("mid bits", module::put_params(progressive, &z), ins.clone(), len));
    let mut z = sizes.clone();
    z.stripes[0].1 = 1 << 44;
    cases.push(("body bits", module::put_params(progressive, &z), ins.clone(), len));
    // the restart markers after the last interval: [scans: 1, flags, trailing, stripes]
    let info = &ins[3];
    assert_eq!(info.len(), 4, "{info:?}");
    assert_eq!(info[2], 0);
    let mut c = ins.clone();
    let mut v = info[..2].to_vec();
    aqr_common::varint::put(&mut v, 1 << 31);
    v.push(info[3]);
    c[3] = v;
    cases.push(("trailing RSTs", a.params.clone(), c, len));
    for (name, params, ins, len) in cases {
        let (r, most) = largest(|| decode(&params, &ins, len));
        assert!(r.is_err(), "{name}: decoded");
        assert!(most < (64 << 20), "{name}: {most} bytes reserved");
    }
}

/// The same for a scan whose sizes are those of the unstuffed bytes, and the components held
/// whole (progressive).
#[test]
fn archived_sizes_beyond_the_output_whole() {
    let Some(f) = formula::encode(PROGRESSIVE, 0) else { return };
    let (a, ins) = inputs(&f);
    let (progressive, sizes) = module::params(&a.params).unwrap();
    assert!(progressive);
    assert_eq!(decode(&a.params, &ins, a.len).unwrap(), PROGRESSIVE);
    let mut z = sizes.clone();
    z.scans[0] = 1 << 44;
    let (r, most) = largest(|| decode(&module::put_params(progressive, &z), &ins, a.len));
    assert!(r.is_err());
    assert!(most < (64 << 20), "{most} bytes reserved");
}
