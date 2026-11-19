//! Edge cases: `find` on forged headers, frames cut or extended, damaged files and chunks
//! (every input of the module, native).

use std::time::Duration;

use aqr_formula::{Formula, Node};
use aqr_prores::{find, formula};

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn eval(f: &Formula, n: &Node) -> Result<Vec<u8>, String> {
    Ok(match n {
        Node::Slice { stream, start, len } => {
            f.streams[*stream].data[*start as usize..(*start + *len) as usize].to_vec()
        }
        Node::Concat(ns) => ns.iter().map(|n| eval(f, n)).collect::<Result<Vec<_>, _>>()?.concat(),
        Node::Apply(a) => {
            let inputs = a.inputs.iter().map(|n| eval(f, n)).collect::<Result<Vec<_>, _>>()?;
            let refs: Vec<&[u8]> = inputs.iter().map(|v| &v[..]).collect();
            formula::decode(&a.params, &refs, a.len).map_err(|e| e.to_string())?
        }
        other => panic!("not in these formulas: {other:?}"),
    })
}

/// The formula of `d` where it has one; what it gives back is `d`.
fn round_trip(d: &[u8]) {
    let _ = formula::starts(d);
    if let Some(f) = formula::encode(d, 1) {
        assert!(eval(&f, &f.root).unwrap() == d, "formula differs");
    }
}

/// The file and the offsets of its frames.
fn frames_of(name: &str) -> (Vec<u8>, Vec<usize>) {
    let d = fixture(name);
    let v = find(&d);
    assert!(v.len() >= 2, "{name}");
    (d, v)
}

fn size(d: &[u8], at: usize) -> usize {
    u32::from_be_bytes(d[at..at + 4].try_into().unwrap()) as usize
}

/// The frames found lie within `v`, one after the other.
fn found_sane(v: &[u8]) {
    let found = find(v);
    for &at in &found {
        assert!(at + size(v, at) <= v.len());
    }
    for w in found.windows(2) {
        assert!(w[0] + size(v, w[0]) <= w[1], "frames overlap");
    }
}

#[test]
fn find_forged_headers() {
    let (d, fs) = frames_of("ks-422.mov");
    let first = fs[0];
    let n = size(&d, first);
    // sizes of the first frame: zero, small, past the file, the largest
    for forged in [0u32, 1, 8, 27, 28, 35, n as u32 - 1, n as u32 + 1, d.len() as u32, u32::MAX, 1 << 31] {
        let mut v = d.clone();
        v[first..first + 4].copy_from_slice(&forged.to_be_bytes());
        found_sane(&v);
        round_trip(&v);
    }
    // header sizes (at 8) and the fields behind: zero, small, huge
    for off in [8usize, 10, 12, 16, 18, 20] {
        for x in [0u8, 1, 0x7f, 0x80, 0xff] {
            for lo in [false, true] {
                let mut v = d.clone();
                v[first + off + lo as usize] = x;
                found_sane(&v);
                round_trip(&v);
            }
        }
    }
    // `icpf` tags with nothing behind, at the end and at the start of the data
    for tail in [&b"icpf"[..], b"\0\0\0\x20icpf", &[0u8; 64], b"icpficpficpf"] {
        let mut v = d.clone();
        v.extend_from_slice(tail);
        assert_eq!(find(&v).len(), fs.len());
        round_trip(&v);
        found_sane(&[tail, &d[..]].concat());
    }
    // tags only, no frames behind them
    let mut v = d[..8].to_vec();
    v.extend(b"icpf".repeat(10_000));
    assert!(find(&v).is_empty());
}

#[test]
fn nested_icpf() {
    // a whole frame's start inside another's data is not found as a frame of its own: the
    // search goes on past the outer frame
    let (d, fs) = frames_of("aw-422hq.mov");
    let (a, n) = (fs[0], size(&d, fs[0]));
    let put = &d[a..a + (n / 4).min(64)];
    let mut outer = d[a..a + n].to_vec();
    let at = n - 4 - put.len();
    outer[at..at + put.len()].copy_from_slice(put);
    let v = [d[..a].to_vec(), outer, d[a + n..].to_vec()].concat();
    found_sane(&v);
    round_trip(&v);
    // the inner start alone in the gap between frames
    let v = [d[..a + n].to_vec(), put.to_vec(), d[a + n..].to_vec()].concat();
    found_sane(&v);
    round_trip(&v);
}

#[test]
fn cut_and_extended() {
    for name in ["ks-interlaced.mov", "ks-422.mov"] {
        let (d, fs) = frames_of(name);
        let end = fs.last().map(|&a| a + size(&d, a)).unwrap();
        // cut at the places of a frame: its header, its middle, one byte short of its end
        let mut cuts = vec![end - 1, end, end + 1];
        for &a in &fs {
            cuts.extend([a, a + 4, a + 8, a + 28, a + size(&d, a) / 2, a + size(&d, a) - 1]);
        }
        for c in cuts {
            if c < d.len() {
                found_sane(&d[..c]);
                round_trip(&d[..c]);
            }
        }
        // extended by bytes after the last frame, and the file doubled
        for extra in [&[0u8; 1][..], &[0u8; 4096][..], &[0xffu8; 100][..]] {
            let v = [&d[..], extra].concat();
            assert_eq!(find(&v).len(), fs.len());
            round_trip(&v);
        }
        round_trip(&[d.clone(), d.clone()].concat());
        // the last frame a byte longer or shorter than its size says
        let last = *fs.last().unwrap();
        let mut v = d.clone();
        let n = size(&v, last) as u32;
        v[last..last + 4].copy_from_slice(&(n + 1).to_be_bytes());
        round_trip(&v);
        v[last..last + 4].copy_from_slice(&(n - 1).to_be_bytes());
        round_trip(&v);
    }
}

#[test]
fn damaged_files() {
    let d = fixture("ks-interlaced.mov");
    let fs = find(&d);
    // the first frame's header at every byte
    let head = d[fs[0] - 4..(fs[0] + 160).min(d.len())].to_vec();
    aqr_testkit::damaged(&head, 200, 200, Duration::from_secs(30), found_sane);
    aqr_testkit::damaged(&d, 60, 0, Duration::from_secs(60), round_trip);
}

/// The inputs of an application of the fixtures' formulas, each damaged for the module alone.
#[test]
fn damaged_module_inputs() {
    for name in ["ks-interlaced.mov", "ks-4444-alpha.mov"] {
        let d = fixture(name);
        let f = formula::encode(&d, 1).unwrap();
        let apps: Vec<_> = match &f.root {
            Node::Concat(ns) => ns.iter().filter_map(|n| if let Node::Apply(a) = n { Some(a) } else { None }).collect(),
            _ => panic!(),
        };
        let a = apps[0];
        let inputs: Vec<Vec<u8>> = a.inputs.iter().map(|n| eval(&f, n).unwrap()).collect();
        assert_eq!(inputs.len(), 3);
        for (k, inp) in inputs.iter().enumerate() {
            // (side, coded and rest inputs of the chunk)
            let per = if inp.len() > 4096 { 50 } else { 100 };
            aqr_testkit::damaged(inp, per, 64, Duration::from_secs(30), |m| {
                let mut v: Vec<&[u8]> = inputs.iter().map(|v| &v[..]).collect();
                v[k] = m;
                let _ = formula::decode(&a.params, &v, a.len);
            });
        }
        // the params and the length told wrongly
        aqr_testkit::damaged(&a.params, 100, 100, Duration::from_secs(30), |m| {
            let v: Vec<&[u8]> = inputs.iter().map(|v| &v[..]).collect();
            let _ = formula::decode(m, &v, a.len);
        });
        let v: Vec<&[u8]> = inputs.iter().map(|v| &v[..]).collect();
        for len in [0, 1, a.len / 2, a.len - 1, a.len + 1, a.len * 2] {
            let _ = formula::decode(&a.params, &v, len);
        }
    }
}
