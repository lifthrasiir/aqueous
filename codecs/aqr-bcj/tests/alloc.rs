//! A size that a caller claims, past what the heap gives: an error, never an allocation that aborts.

use aqr_bcj::bcj2::{self, Streams};
use aqr_testkit::{Refusing, case};

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 30);

#[test]
fn bcj2_of_a_huge_size() {
    case("bcj2 huge size");
    let s = Streams { main: vec![1, 2, 3], ..Default::default() };
    for size in [1 << 40, usize::MAX / 2, usize::MAX] {
        assert!(bcj2::decode(&s, &[0; 5], size).is_err());
    }
}
