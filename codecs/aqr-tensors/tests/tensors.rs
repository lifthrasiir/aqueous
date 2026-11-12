//! Files of tensors rebuilt from their formulas: made-up GGUF files of every type coded, whole and
//! in segments, the formula the same whatever the threads, and damaged inputs told; made-up files
//! of `torch.save` and safetensors.

use aqr_formula::{Formula, Node};
use aqr_tensors::formula::{self, MODULE};
use aqr_tensors::module::decode;

/// The inputs of application `a` of `f`.
fn inputs<'a>(f: &'a Formula, a: &aqr_formula::Apply) -> Vec<&'a [u8]> {
    (a.inputs.iter())
        .map(|n| match n {
            &Node::Slice { stream, start, len } => &f.streams[stream].data[start as usize..][..len as usize],
            _ => panic!("not a slice"),
        })
        .collect()
}

/// The file of formula `f`: its nodes one after another, the applications decoded.
fn eval(f: &Formula) -> Vec<u8> {
    let Node::Concat(nodes) = &f.root else { panic!("not a concatenation") };
    let mut out = Vec::new();
    for n in nodes {
        match n {
            Node::Apply(a) => {
                assert_eq!(a.module, MODULE);
                out.extend(decode(&a.params, &inputs(f, a), a.len, 1).unwrap());
            }
            &Node::Slice { stream, start, len } => {
                out.extend_from_slice(&f.streams[stream].data[start as usize..][..len as usize])
            }
            _ => panic!("a node of another kind"),
        }
    }
    out
}

fn apps(f: &Formula) -> Vec<&aqr_formula::Apply> {
    let Node::Concat(nodes) = &f.root else { panic!() };
    nodes.iter().filter_map(|n| if let Node::Apply(a) = n { Some(&**a) } else { None }).collect()
}

#[test]
fn rebuilt() {
    for (seed, scale) in [(1, 1), (2, 3), (3, 8)] {
        let d = aqr_tensors::synth::file(seed, scale);
        let f = formula::encode(&d, 4).unwrap();
        assert_eq!(eval(&f), d, "{seed}");
        assert!(formula::is(&f));
        // (every type coded: each quantised type's tag, and floats', in the params)
        let tags: std::collections::BTreeSet<u8> = apps(&f)
            .iter()
            .flat_map(|a| {
                let mut at = 0;
                let mut t = Vec::new();
                while at < a.params.len() {
                    t.push(a.params[at]);
                    aqr_tensors::layout::Run::read(&a.params, &mut at).unwrap();
                }
                t
            })
            .collect();
        for q in aqr_tensors::layout::Quant::ALL {
            assert!(tags.contains(&q.tag()), "{seed}: {q:?} not coded: {tags:?}");
        }
        for fl in aqr_tensors::layout::Float::ALL {
            assert!(tags.contains(&fl.tag()), "{seed}: {fl:?} not coded");
        }
        let coded: usize = f.streams.iter().filter(|s| s.class != formula::HEADER).map(|s| s.data.len()).sum();
        assert!(coded < d.len() * 97 / 100, "{seed}: {coded} of {}", d.len());
        // (the same whatever the threads)
        assert!(formula::encode(&d, 1).unwrap() == f, "{seed}: threads");
    }
}

#[test]
fn segments() {
    let d = aqr_tensors::synth::file(4, 8);
    for segment in [1 << 16, 100_000, 300_000] {
        let f = formula::encode_in(&d, 3, segment).unwrap();
        let a = apps(&f);
        assert!(a.len() > d.len() / segment / 2, "{segment}: {} applications", a.len());
        assert!(a.iter().all(|a| a.len <= (segment + 70_000) as u64), "{segment}");
        assert_eq!(eval(&f), d, "{segment}");
    }
}

/// Each input of an application cut short or longer, and the output's length off by one: told.
#[test]
fn damaged() {
    let d = aqr_tensors::synth::file(5, 2);
    let f = formula::encode(&d, 2).unwrap();
    for a in apps(&f) {
        let ins = inputs(&f, a);
        for k in 0..3 {
            let mut cut: Vec<Vec<u8>> = ins.iter().map(|i| i.to_vec()).collect();
            if !cut[k].is_empty() {
                cut[k].pop();
                let refs: Vec<&[u8]> = cut.iter().map(|c| &c[..]).collect();
                assert!(decode(&a.params, &refs, a.len, 1).is_err(), "truncated {k}");
            }
            cut[k].extend_from_slice(&[1, 2, 3]);
            let refs: Vec<&[u8]> = cut.iter().map(|c| &c[..]).collect();
            assert!(decode(&a.params, &refs, a.len, 1).is_err(), "longer {k}");
        }
        assert!(decode(&a.params, &ins, a.len + 1, 1).is_err());
        assert!(decode(&a.params, &ins, a.len - 1, 1).is_err());
        // (a flipped byte of the coded symbols decodes to something else, or fails)
        let mut flipped: Vec<Vec<u8>> = ins.iter().map(|i| i.to_vec()).collect();
        let n = flipped[0].len();
        flipped[0][n / 2] ^= 0x40;
        let refs: Vec<&[u8]> = flipped.iter().map(|c| &c[..]).collect();
        let want = decode(&a.params, &ins, a.len, 1).unwrap();
        assert!(
            decode(&a.params, &refs, a.len, 1).is_ok_and(|o| o != want) || decode(&a.params, &refs, a.len, 1).is_err()
        );
        // (coded symbols of noise, short and long: no panic, an error)
        let mut r = aqr_common::rng::Rng::new(a.len);
        for n in [200, 5000, 3 * ins[0].len() + 1000] {
            let noise: Vec<u8> = (0..n).map(|_| r.below(256) as u8 | 0x80).collect();
            assert!(decode(&a.params, &[&noise, ins[1], ins[2]], a.len, 1).is_err(), "noise of {n}");
        }
    }
}

/// Files that are not GGUF, or whose header does not hold, are not taken.
#[test]
fn not_taken() {
    let d = aqr_tensors::synth::file(6, 1);
    assert!(formula::encode(b"GGUF\x03\0\0\0", 1).is_none());
    assert!(formula::encode(&d[..100], 1).is_none());
    let mut v1 = d.clone();
    v1[4] = 1;
    assert!(formula::encode(&v1, 1).is_none());
    // (a file cut within its data: the tensors past its end as they are)
    let cut = &d[..d.len() * 2 / 3];
    if let Some(f) = formula::encode(cut, 2) {
        assert_eq!(eval(&f), cut);
    }
}

/// The tags of the runs of the applications of `f`.
fn tags(f: &Formula) -> std::collections::BTreeSet<u8> {
    apps(f)
        .iter()
        .flat_map(|a| {
            let mut at = 0;
            let mut t = Vec::new();
            while at < a.params.len() {
                t.push(a.params[at]);
                aqr_tensors::layout::Run::read(&a.params, &mut at).unwrap();
            }
            t
        })
        .collect()
}

/// Made-up files of `torch.save` and safetensors: rebuilt, their floats of each type coded (each
/// storage once), their header a stream of its own, the integers as they are.
#[test]
fn pth_and_safetensors() {
    use aqr_tensors::tensors::{self, Container};
    for (container, make) in [
        (Container::Pth, aqr_tensors::synth::pth as fn(u64, usize) -> Vec<u8>),
        (Container::Safetensors, aqr_tensors::synth::safetensors),
    ] {
        for (seed, scale) in [(1, 1), (2, 5)] {
            let d = make(seed, scale);
            assert_eq!(tensors::starts(&d), Some(container));
            let t = tensors::parse(&d).unwrap();
            // (of the six tensors, the integers not, and the view of another's storage not again)
            let want = if container == Container::Pth { 4 } else { 5 };
            assert_eq!(t.tensors.len(), want, "{container:?} {seed}");
            let f = formula::encode_in(&d, 3, 1 << 16).unwrap();
            assert_eq!(eval(&f), d, "{container:?} {seed}");
            assert!(f.streams.iter().any(|s| s.class == container.header()));
            let tags = tags(&f);
            for fl in aqr_tensors::layout::Float::ALL {
                assert!(tags.contains(&fl.tag()), "{container:?} {seed}: {fl:?} not coded");
            }
            assert!(formula::encode(&d, 1).unwrap() == formula::encode(&d, 4).unwrap());
        }
    }
}

/// Files of `torch.save` and safetensors whose structure does not hold are not taken (or taken
/// only as far as it does).
#[test]
fn pth_and_safetensors_not_taken() {
    let d = aqr_tensors::synth::pth(3, 1);
    // (the pickle of an opcode not known; the end of central directory gone)
    let mut bad = d.clone();
    let pkl = 30 + b"archive/data.pkl".len() + 4;
    let pkl = pkl + (64 - pkl % 64) % 64;
    assert_eq!(&bad[pkl..pkl + 2], &[0x80, 2]);
    bad[pkl + 2] = 0xff;
    assert!(formula::encode(&bad, 1).is_none());
    assert!(formula::encode(&d[..d.len() - 22], 1).is_none());
    // (big-endian)
    let at = d.windows(6).rposition(|w| w == b"little").unwrap();
    let mut big = d.clone();
    big[at..at + 6].copy_from_slice(b"big\n\n\n");
    assert!(formula::encode(&big, 1).is_none());

    let d = aqr_tensors::synth::safetensors(3, 1);
    assert!(formula::encode(&d[..100], 1).is_none());
    let mut bad = d.clone();
    bad[8] = b'[';
    assert!(formula::encode(&bad, 1).is_none());
    // (a header of a length past the file; a tensor past the end, left as it is)
    let mut bad = d.clone();
    bad[..8].copy_from_slice(&(d.len() as u64).to_le_bytes());
    assert!(formula::encode(&bad, 1).is_none());
    let cut = &d[..d.len() - 1000];
    let f = formula::encode(cut, 2).unwrap();
    assert_eq!(eval(&f), cut);
}
