use aqr_api::emu::{Failure, apply};
use aqr_api::{Input, Memory, Output, Packed};
use aqr_formula::packed::pack;

aqr_api::entry! {
    // the items of `input0` in the reverse order
    whole fn reversed(params_len, in0_len, size) {
        let p = Packed::<Input<0>>::new(in0_len);
        let mut at = 0;
        for i in (0..p.count()).rev() {
            let r = p.range(i);
            Output::<0>::copy_from::<Input<0>>(at, r.start, r.end - r.start);
            at += r.end - r.start;
        }
        assert_eq!(at, size);
    }
}

aqr_api::entry! {
    // the items of `input0` one after another, a byte at a time
    part fn joined(params_len, in0_len, size) {
        let p = Packed::<Input<0>>::request(in0_len);
        let mut at = 0;
        for i in 0..p.count() {
            for o in p.range(i) {
                let mut b = [0];
                Input::<0>::request(o + 1);
                Input::<0>::read(o, &mut b);
                Output::<0>::write(at, &b);
                at += 1;
                Output::<0>::commit(at);
            }
        }
    }
}

#[test]
fn whole() {
    let items: [&[u8]; 4] = [b"ab", b"", b"cde", b"f"];
    assert_eq!(apply(&reversed::ENTRY, &[&pack(&items)], &[], b"", &[6]).unwrap()[0], b"fcdeab");
    assert_eq!(apply(&reversed::ENTRY, &[&pack::<&[u8]>(&[])], &[], b"", &[0]).unwrap()[0], b"");
    // more ends than a chunk of them
    let many: Vec<Vec<u8>> = (0..100u8).map(|i| vec![i; i as usize % 3]).collect();
    let want: Vec<u8> = many.iter().rev().flatten().copied().collect();
    assert_eq!(apply(&reversed::ENTRY, &[&pack(&many)], &[], b"", &[want.len() as u64]).unwrap()[0], want);
}

#[test]
fn part() {
    let items: [&[u8]; 3] = [b"ab", b"", b"cde"];
    assert_eq!(apply(&joined::ENTRY, &[&pack(&items)], &[], b"", &[5]).unwrap()[0], b"abcde");
}

#[test]
fn invalid() {
    let p = pack::<&[u8]>(&[b"ab", b"cde"]);
    let mut bad = vec![p[..7].to_vec(), p[..p.len() - 1].to_vec(), [&p[..], b"x"].concat()];
    let mut count = p.clone();
    count[0] = 4;
    let mut ends = p.clone();
    ends[8] = 6;
    bad.extend([count, ends]);
    for b in &bad {
        for entry in [&reversed::ENTRY, &joined::ENTRY] {
            let Err(Failure { log, .. }) = apply(entry, &[b], &[], b"", &[5]) else { panic!("{b:?}") };
            assert_eq!(log.as_deref(), Some(&b"aqr_api: not a packed byte sequence"[..]), "{b:?}");
        }
    }
}
