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
        // SAFETY: the tasks are dropped here (as in the others)
        let mut t = unsafe { task::spawn(chunks.len(), &f) };
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
        let r = unsafe { task::spawn(1, &f) }.join(0);
        Output::<0>::write(0, &r);
    }
}

aqr_api::entry! {
    // tasks of tasks, some results not taken
    whole fn nested(params_len, in0_len, size) {
        let inner = |i: usize, out: &mut Vec<u8>| out.push(i as u8);
        let outer = |i: usize, out: &mut Vec<u8>| {
            let mut t = unsafe { task::spawn(i + 1, &inner) };
            out.extend(t.join(i));
        };
        let mut t = unsafe { task::spawn(3, &outer) };
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
        let _t = unsafe { task::spawn_gang(0, 2, &f) };
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
        let f = |_: usize, _: &mut Vec<u8>| tx.send(b"b");
        let mut t = unsafe { task::spawn_gang(0, 1, &f) };
        tx.send(b"a");
        t.join(0);
        rx.recv();
    }
}

aqr_api::entry! {
    // channels made and a spawn of tasks that are no gang
    whole fn plain_spawn(params_len, in0_len, size) {
        let _c = chan::channel(1);
        let f = |_: usize, _: &mut Vec<u8>| {};
        unsafe { task::spawn(1, &f) }.join(0);
    }
}

aqr_api::entry! {
    // channels made and never given to a gang
    whole fn never_given(params_len, in0_len, size) {
        let _c = chan::channel(1);
    }
}

aqr_api::entry! {
    // both ends of a channel taken by the module
    whole fn both_ends(params_len, in0_len, size) {
        let (tx, rx) = chan::channel(1);
        let f = |_: usize, _: &mut Vec<u8>| {};
        let _t = unsafe { task::spawn_gang(0, 1, &f) };
        tx.send(b"a");
        rx.recv();
    }
}

aqr_api::entry! {
    // a channel taken before a spawn gave it
    whole fn not_yet(params_len, in0_len, size) {
        let (tx, _rx) = chan::channel(1);
        tx.send(b"a");
    }
}

aqr_api::entry! {
    // a task that is no gang's taking an end of the gang's channel
    whole fn outside_gang(params_len, in0_len, size) {
        let (tx, _rx) = chan::channel(1);
        let nothing = |_: usize, _: &mut Vec<u8>| {};
        let _g = unsafe { task::spawn_gang(0, 1, &nothing) };
        let f = |_: usize, _: &mut Vec<u8>| tx.send(b"a");
        unsafe { task::spawn_class(1, 1, &f) }.join(0);
    }
}

aqr_api::entry! {
    // a gang's task taking the end of a channel of the instance's before the gang's spawn, which
    // another gang's task made: the tasks of a gang are all a gang's
    whole fn made_by_task(params_len, in0_len, size) {
        let g = |_: usize, _: &mut Vec<u8>| {
            let (tx, _rx) = chan::channel(1);
            // (a task's channels are given to the gang it spawns)
            let f = |_: usize, _: &mut Vec<u8>| tx.send(b"a");
            unsafe { task::spawn_gang(1, 1, &f) }.join(0);
        };
        unsafe { task::spawn_gang(0, 1, &g) }.join(0);
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

/// Channels belong to the next spawn of their maker, which is of a gang; their ends are taken
/// by that gang's tasks or the instance that spawned it, one end by one instance.
#[test]
fn channel_ownership() {
    let fails = |e: &aqr_api::emu::Entry, what: &str| {
        let e = apply(e, &[b""], &[], b"", &[0]).unwrap_err();
        assert!(e.what.contains(what), "{e:?}");
    };
    fails(&plain_spawn::ENTRY, "no gang");
    fails(&never_given::ENTRY, "not given to a gang");
    fails(&both_ends::ENTRY, "both ends");
    fails(&not_yet::ENTRY, "before a spawn");
    fails(&outside_gang::ENTRY, "outside its gang");
    assert!(apply(&made_by_task::ENTRY, &[b""], &[], b"", &[0]).is_ok());
}
