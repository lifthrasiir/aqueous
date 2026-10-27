#![cfg(feature = "alloc")]

use aqr_api::emu::apply;
use aqr_api::{Input, Memory, Output, task};

aqr_api::entry! {
    // `input0` with each 1000-byte chunk reversed, by a task per chunk
    whole fn reverse(params_len, in0_len, size) {
        let mut data = vec![0; in0_len as usize];
        Input::<0>::read(0, &mut data);
        let chunks: Vec<&[u8]> = data.chunks(1000).collect();
        let f = |i: usize, out: &mut Vec<u8>| out.extend(chunks[i].iter().rev());
        let mut t = task::spawn(chunks.len(), &f);
        let mut at = 0;
        for i in 0..t.len() {
            let r = t.join(i);
            Output::<0>::write(at, &r);
            at += r.len() as u64;
        }
    }
}

aqr_api::entry! {
    // a task reading `input0` itself: empty to it
    whole fn reads_input(params_len, in0_len, size) {
        let f = |_: usize, out: &mut Vec<u8>| {
            let mut b = [0];
            Input::<0>::read(0, &mut b);
            out.push(b[0]);
        };
        let r = task::spawn(1, &f).join(0);
        Output::<0>::write(0, &r);
    }
}

aqr_api::entry! {
    // tasks of tasks, some results not taken
    whole fn nested(params_len, in0_len, size) {
        let inner = |i: usize, out: &mut Vec<u8>| out.push(i as u8);
        let outer = |i: usize, out: &mut Vec<u8>| {
            let mut t = task::spawn(i + 1, &inner);
            out.extend(t.join(i));
        };
        let mut t = task::spawn(3, &outer);
        let r = [t.join(2), t.join(0)].concat();
        Output::<0>::write(0, &r);
    }
}

#[test]
fn chunks() {
    let data: Vec<u8> = (0..4500u32).map(|i| (i * 7) as u8).collect();
    let want: Vec<u8> = data.chunks(1000).flat_map(|c| c.iter().rev().copied()).collect();
    assert_eq!(apply(&reverse::ENTRY, &[&data], &[], b"", &[data.len() as u64]).unwrap(), [want]);
    assert_eq!(apply(&reverse::ENTRY, &[b""], &[], b"", &[0]).unwrap(), [b""]);
}

#[test]
fn memories_empty_to_tasks() {
    assert!(apply(&reads_input::ENTRY, &[b"x"], &[], b"", &[1]).is_err());
}

#[test]
fn nested_tasks() {
    assert_eq!(apply(&nested::ENTRY, &[b""], &[], b"", &[2]).unwrap(), [vec![2, 0]]);
}
