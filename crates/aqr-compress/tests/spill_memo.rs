//! What verifying takes of the store ([`aqr_formula::store`]). (A test binary of its own: the
//! store is the process's.)

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use aqr_compress::Options;
use aqr_compress::spill::Spill;
use aqr_formula::Bytes;
use aqr_formula::store::Store;

/// A spill counting the bytes of the regions taken.
struct Counting(Spill, AtomicUsize);

impl Store for Counting {
    fn region(&self, n: usize) -> Option<(*mut u8, Bytes)> {
        self.1.fetch_add(n, Ordering::Relaxed);
        self.0.region(n)
    }

    fn find(&self, d: &[u8]) -> Option<Bytes> {
        self.0.find(d)
    }
}

fn gzip(d: &[u8]) -> Vec<u8> {
    let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(d).unwrap();
    z.finish().unwrap()
}

/// The output of a content's formula (a gzip file in a gzip file) is kept for the verification
/// as the content's bytes in the store, not copied again: verifying takes no more of the store
/// than not.
#[test]
fn contents_not_copied() {
    let mut x = 1u64;
    let noise: Vec<u8> = (0..400_000)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            0x80 | (x >> 60) as u8
        })
        .collect();
    let inner = gzip(&noise);
    let d = gzip(&inner);
    let dir = std::env::temp_dir().join(format!("aqr-compress-spill-memo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let taken = |verify: bool| {
        let store = Arc::new(Counting(Spill::create(&dir.join(".out")).unwrap(), AtomicUsize::new(0)));
        let guard = aqr_formula::store::set(Some(store.clone()));
        let c = aqr_compress::compress(&d, &Options { verify, threads: 2, ..Options::default() });
        drop(guard);
        assert!(aqr_decompress::decompress(&c, 1).unwrap() == d);
        (c, store.1.load(Ordering::Relaxed))
    };
    let (plain, plain_taken) = taken(false);
    let (verified, verified_taken) = taken(true);
    println!("{plain_taken} bytes taken, {verified_taken} verifying; the content of {} bytes", inner.len());
    assert!(plain == verified);
    assert!(plain_taken >= inner.len(), "the content spilled");
    assert_eq!(verified_taken, plain_taken);
    std::fs::remove_dir(&dir).unwrap();
}
