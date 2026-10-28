#![cfg(feature = "alloc")]

use aqr_api::emu::apply;
use aqr_api::{Input, Memory, Output, chan, task};

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

aqr_api::entry! {
    // `input0` through two tasks and channels of 2 messages, a byte a message: each adds 1
    whole fn piped(params_len, in0_len, size) {
        let mut data = vec![0; in0_len as usize];
        Input::<0>::read(0, &mut data);
        let ((tx0, rx0), (tx1, rx1)) = (chan::channel(2), chan::channel(2));
        let f = |i: usize, _: &mut Vec<u8>| {
            if i == 0 {
                data.iter().for_each(|b| tx0.send(&[b + 1]));
                // (it may send nothing)
                tx0.close();
            } else {
                while let Some(m) = rx0.recv() {
                    tx1.send(&[m[0] + 1]);
                }
                tx1.close();
            }
        };
        let _t = task::spawn(2, &f);
        let mut at = 0;
        while let Some(m) = rx1.recv() {
            Output::<0>::write(at, &m);
            at += 1;
        }
    }
}

aqr_api::entry! {
    // the module sending to the channel its task sends to
    whole fn two_senders(params_len, in0_len, size) {
        let (tx, rx) = chan::channel(4);
        tx.send(b"a");
        let f = |_: usize, _: &mut Vec<u8>| tx.send(b"b");
        task::spawn(1, &f).join(0);
        rx.recv();
    }
}

#[test]
fn channels() {
    let data: Vec<u8> = (0..2000u32).map(|i| (i % 200) as u8).collect();
    let want: Vec<u8> = data.iter().map(|b| b + 2).collect();
    assert_eq!(apply(&piped::ENTRY, &[&data], &[], b"", &[data.len() as u64]).unwrap(), [want]);
    assert_eq!(apply(&piped::ENTRY, &[b""], &[], b"", &[0]).unwrap(), [b""]);
    let e = apply(&two_senders::ENTRY, &[b""], &[], b"", &[1]).unwrap_err();
    assert!(e.what.contains("two instances"), "{e:?}");
}
