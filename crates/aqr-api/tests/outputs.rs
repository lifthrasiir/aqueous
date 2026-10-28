use aqr_api::emu::{Failure, apply};
use aqr_api::{Input, Memory, Output};

aqr_api::entry! {
    // `input0` split into two outputs of the sizes declared
    whole fn halves(params_len, in0_len, size0, size1) {
        assert_eq!(size0 + size1, in0_len);
        Output::<0>::copy_from::<Input<0>>(0, 0, size0);
        Output::<1>::copy_from::<Input<0>>(0, size0, size1);
    }
}

aqr_api::entry! {
    // the bytes of `input0` at even offsets, then those at odd ones, streamed
    part fn alternate(params_len, in0_len, size0, size1) {
        assert_eq!((size0, size1), (in0_len.div_ceil(2), in0_len / 2));
        let w = Input::<0>::size();
        let mut b = [0];
        for o in 0..in0_len {
            Input::<0>::request(o + 1);
            Input::<0>::read(o % w, &mut b);
            Input::<0>::release(o + 1);
            if o % 2 == 0 {
                Output::<0>::write(o / 2 % Output::<0>::size(), &b);
                Output::<0>::commit(o / 2 + 1);
            } else {
                Output::<1>::write(o / 2 % Output::<1>::size(), &b);
                Output::<1>::commit(o / 2 + 1);
            }
        }
    }
}

aqr_api::entry! {
    // writes its second output whatever is declared
    whole fn second(params_len, in0_len, size0) {
        Output::<1>::write(0, &[1]);
    }
}

#[test]
fn whole() {
    assert_eq!(apply(&halves::ENTRY, &[b"abcdef"], &[], b"", &[2, 4]).unwrap(), [&b"ab"[..], b"cdef"]);
    assert_eq!(apply(&halves::ENTRY, &[b"abc"], &[], b"", &[0, 3]).unwrap(), [&b""[..], b"abc"]);
}

#[test]
fn part() {
    let data: Vec<u8> = (0..70_001u32).map(|i| (i * 13) as u8).collect();
    let (even, odd): (Vec<_>, Vec<_>) = data.iter().enumerate().partition(|(i, _)| i % 2 == 0);
    let want = [even.iter().map(|p| *p.1).collect::<Vec<_>>(), odd.iter().map(|p| *p.1).collect()];
    let sizes = [want[0].len() as u64, want[1].len() as u64];
    assert_eq!(apply(&alternate::ENTRY, &[&data], &[], b"", &sizes).unwrap(), want);
}

#[test]
fn outputs_declared() {
    // as many output sizes as the entry point takes, at least one, and no output beyond them
    assert!(apply(&halves::ENTRY, &[b"abc"], &[], b"", &[3]).is_err());
    assert!(apply(&halves::ENTRY, &[b"abc"], &[], b"", &[]).is_err());
    let Err(Failure { what, .. }) = apply(&second::ENTRY, &[b"abc"], &[], b"", &[1]) else { panic!() };
    assert!(what.contains("aqr_output1"), "{what}");
    // an output of another size than declared
    let Err(Failure { what, .. }) = apply(&alternate::ENTRY, &[b"abc"], &[], b"", &[2, 2]) else { panic!() };
    assert!(what.contains("assertion"), "{what}");
}
