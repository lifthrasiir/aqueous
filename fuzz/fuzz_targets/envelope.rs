//! An encrypted archive opened as `aqr -d` opens it: its envelope parsed (damaged or not),
//! unlocked by the seeds' passphrase, its payload decrypted as it is read (by its index, and
//! whole), then decrypted whole, chunks that do not authenticate too, its mirror blocks checked
//! of the bytes trusted, repaired and salvaged. Fuzzed, a chunk that does not authenticate is
//! decrypted all the same and taken to where its tag's first byte is even (aqr-archive's
//! `Unlocked::opens`), so that a mutation of a chunk is one of the payload.
#![no_main]

use aqr_archive::{Payload, envelope, mirror};
use libfuzzer_sys::fuzz_target;

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(aqr_fuzz::ALLOCATION_MAX);

fuzz_target!(|c: &[u8]| {
    let runner = aqr_fuzz::toys();
    let _ = envelope::open(c);
    let _ = envelope::is_damaged_encrypted(c);
    let Ok((e, _)) = envelope::open_damaged(c) else { return };
    // (scrypt's memory and time: the seeds' work factor alone)
    if e.work_factor() != aqr_fuzz::WORK_FACTOR {
        return;
    }
    let Ok(u) = e.unlock(aqr_fuzz::PASSPHRASE) else { return };
    let d = u.decrypting();
    let p = Payload::Sealed(&d);
    if p.of_tree()
        && let Ok(a) = aqr_archive::open_indexed(p, &runner, 1)
    {
        let _ = a.value(1);
    }
    if let Ok(a) = aqr_archive::open_decrypting(&d, &runner) {
        let _ = a.value(1);
    }
    let u = d.unlocked();
    let mut plain = vec![0; u.len()];
    let (failed, cut) = u.decrypt_lossy(&mut plain, 1);
    let erased: Vec<_> = failed.iter().map(|&m| u.chunk_range(m)).collect();
    let check = mirror::check_sealed(&plain, &erased, cut);
    let repaired = mirror::repair(&plain, &check, true, &erased).ok().flatten();
    let opened = match &repaired {
        Some(r) if !r.lost.is_empty() => aqr_archive::open_salvaging(&r.bytes, &runner, true),
        Some(r) => aqr_archive::open_payload(&r.bytes, &runner),
        None => aqr_archive::open_payload(&plain, &runner),
    };
    if let Ok(a) = opened {
        let _ = a.value(1);
    }
});
