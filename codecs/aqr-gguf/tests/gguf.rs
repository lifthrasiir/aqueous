//! GGUF files rebuilt from their formulas: made-up files of every type coded, whole and in
//! segments, the formula the same whatever the threads, and damaged inputs told.

use aqr_formula::{Formula, Node};
use aqr_gguf::formula::{self, MODULE};
use aqr_gguf::module::decode;

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
        let d = aqr_gguf::synth::file(seed, scale);
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
                    aqr_gguf::layout::Run::read(&a.params, &mut at).unwrap();
                }
                t
            })
            .collect();
        for q in aqr_gguf::layout::Quant::ALL {
            assert!(tags.contains(&q.tag()), "{seed}: {q:?} not coded: {tags:?}");
        }
        for fl in aqr_gguf::layout::Float::ALL {
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
    let d = aqr_gguf::synth::file(4, 8);
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
    let d = aqr_gguf::synth::file(5, 2);
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
    let d = aqr_gguf::synth::file(6, 1);
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
