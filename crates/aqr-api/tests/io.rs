use aqr_api::emu::{Failure, apply};
use aqr_api::{Bytes, BytesMut, Dst, Input, Mem, Out, Params, Resource, Src, finish};

/// `resource0`, then each input reversed by chunks read and written, then the parameters
/// copied directly: the same code natively and as a module.
fn mix<S: Src, D: Dst<S>>(params: &S, resources: &[S], inputs: &[S], out: &mut D) -> Result<(), &'static str> {
    let mut at = 0;
    for r in resources {
        out.copy_from(at, r, 0, r.len());
        at += r.len();
    }
    for i in inputs {
        let mut buf = [0; 3];
        let mut left = i.len();
        while left > 0 {
            let n = left.min(3);
            let b = &mut buf[..n as usize];
            i.read(left - n, b);
            b.reverse();
            out.write(at, b);
            at += n;
            left -= n;
        }
    }
    out.copy_from(at, params, 0, params.len());
    at += params.len();
    // read back
    let mut first = [0];
    if at > 0 {
        out.read(0, &mut first);
    }
    if at != out.len() || first == *b"!" { Err("not of the size declared") } else { Ok(()) }
}

aqr_api::entry! {
    whole fn mixed(params_len, res0_len, in0_len, in1_len, size) {
        let ins = [Mem::new::<Input<0>>(in0_len), Mem::new::<Input<1>>(in1_len)];
        let res = [Mem::new::<Resource<0>>(res0_len)];
        finish(mix(&Mem::new::<Params>(params_len), &res, &ins, &mut Out::new(size)));
    }
}

fn native(params: &[u8], res: &[&[u8]], ins: &[&[u8]], size: usize) -> Result<Vec<u8>, &'static str> {
    let mut out = vec![0; size];
    let (res, ins): (Vec<_>, Vec<_>) = (res.iter().map(|r| Bytes(r)).collect(), ins.iter().map(|i| Bytes(i)).collect());
    mix(&Bytes(params), &res, &ins, &mut BytesMut(&mut out))?;
    Ok(out)
}

#[test]
fn same_natively() {
    let in0: Vec<u8> = (0..70_000u32).map(|i| (i * 7) as u8).collect();
    for (params, res, ins) in [
        (&b"|"[..], &b"res"[..], [&b"abcdefg"[..], b"xy"]),
        (b"", b"", [&in0[..], b""]),
        (b"pp", b"r", [b"", &in0[..5]]),
    ] {
        let size = params.len() + res.len() + ins[0].len() + ins[1].len();
        let want = native(params, &[res], &ins, size).unwrap();
        assert_eq!(apply(&mixed::ENTRY, &ins, &[res], params, &[size as u64]).unwrap(), [want]);
    }
}

#[test]
fn errors_logged() {
    assert!(native(b"|", &[b"res"], &[b"ab", b""], 7).is_err());
    let Err(Failure { log, .. }) = apply(&mixed::ENTRY, &[b"ab", b""], &[b"res"], b"|", &[7]) else { panic!() };
    assert_eq!(log.as_deref(), Some(&b"not of the size declared"[..]));
    // past the length declared, though within the memory: as natively
    let Err(Failure { log, .. }) = apply(&mixed::ENTRY, &[b"ab", b""], &[b"res"], b"|", &[5]) else { panic!() };
    assert_eq!(log.as_deref(), Some(&b"aqr_api: out of bounds"[..]));
}
