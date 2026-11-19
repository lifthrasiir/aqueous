//! Every damage of a setup header: it is refused, or it parses, and then its packets (of the
//! undamaged setup's random ones) are walked within bounds and written back as they were.

mod common;

use aqr_testkit::{Refusing, damaged};
use aqr_vorbis::packet::{self, Scratch};
use aqr_vorbis::setup::Setup;
use common::{IDENT, random_packet, setup_bytes};
use std::time::Duration;

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 30);

#[test]
fn damaged_setups() {
    let good = Setup::parse(&setup_bytes(), IDENT).unwrap();
    let packets: Vec<Vec<u8>> = (0..12).map(|seed| random_packet(&good, seed).0).collect();
    let d = setup_bytes();
    let parsed = std::sync::atomic::AtomicUsize::new(0);
    damaged(&d, d.len(), d.len(), Duration::from_secs(20), |v| {
        let Ok(s) = Setup::parse(v, IDENT) else { return };
        parsed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let _ = Scratch::new(&s);
        for p in &packets {
            if let Ok(x) = packet::parse(&s, p) {
                assert_eq!(packet::write(&s, &x).as_deref(), Ok(&p[..]), "write back");
            }
            // and the packet cut short
            for cut in [0, 1, p.len() / 2] {
                let _ = packet::parse(&s, &p[..cut.min(p.len())]);
            }
        }
        assert!(aqr_testkit::most() < 1 << 28, "allocated {}", aqr_testkit::most());
    });
    // some damages (the unused bits, a table's values) leave a setup
    assert!(parsed.load(std::sync::atomic::Ordering::Relaxed) > 0);
}
