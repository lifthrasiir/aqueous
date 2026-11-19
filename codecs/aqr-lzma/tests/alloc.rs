//! Sizes that a header or a caller claims, past what the heap gives: an error or an answer,
//! never an allocation that aborts.

use aqr_lzma::lzma::{self, Props};
use aqr_testkit::{Refusing, case};

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 30);

const PROPS: Props = Props { lc: 3, lp: 0, pb: 2 };

/// A `.lzma` header's size of 1 TiB, a limit that allows it: the reservation is refused (as a
/// heap that small does), and the decoder fails.
#[test]
fn decode_reserving_a_huge_size() {
    case("decode huge size");
    assert!(lzma::decode(&[0; 16], PROPS, Some(1 << 40), usize::MAX).is_err());
    assert!(lzma::decode(&[0; 16], PROPS, Some(u64::MAX), usize::MAX).is_err());
}

/// `plausible` takes the length it looks at from its caller.
#[test]
fn plausible_of_a_huge_length() {
    case("plausible huge length");
    let _ = lzma::plausible(&[0; 16], PROPS, 1 << 40);
    let _ = lzma::plausible(&[0; 16], PROPS, usize::MAX / 2);
}
