use aqr_zip::zip::{END, LOCATOR, find_end};

/// A zip64 locator whose offset of the end record is near `u64::MAX` is not a place to look.
#[test]
fn locator_offset_out_of_range() {
    for at in [u64::MAX, u64::MAX - 3, u64::MAX - 55, u64::MAX - 56] {
        let mut d = vec![0u8; 60];
        d.extend_from_slice(&LOCATOR);
        d.extend_from_slice(&[0; 4]);
        d.extend_from_slice(&at.to_le_bytes());
        d.extend_from_slice(&[0; 4]);
        d.extend_from_slice(&END);
        d.extend_from_slice(&[0; 18]);
        assert_eq!(find_end(&d), Some((80, 80..80)), "{at}");
    }
}
