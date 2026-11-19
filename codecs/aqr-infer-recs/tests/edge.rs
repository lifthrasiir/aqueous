//! Edge cases: special floats, either byte order, tiny and random inputs, the decoders damaged.

use std::time::Duration;

use aqr_formula::{Formula, Node};
use aqr_infer_recs::formula::{self, ENTRIES, FIELDS, FLOATS};
use aqr_infer_recs::module::Rule;
use aqr_infer_recs::module::{self, Float};
use aqr_infer_recs::{Choice, Form, Region, floats, infer, infer_if};

#[global_allocator]
static ALLOC: aqr_testkit::Refusing = aqr_testkit::Refusing(1 << 30);

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn eval(f: &Formula, n: &Node) -> Result<Vec<u8>, module::Error> {
    match n {
        Node::Slice { stream, start, len } => {
            Ok(f.streams[*stream].data[*start as usize..(start + len) as usize].to_vec())
        }
        Node::Concat(parts) => {
            let mut v = Vec::new();
            for p in parts {
                v.extend(eval(f, p)?);
            }
            Ok(v)
        }
        Node::Apply(a) => {
            let inputs = a.inputs.iter().map(|i| eval(f, i)).collect::<Result<Vec<_>, _>>()?;
            let inputs: Vec<&[u8]> = inputs.iter().map(|i| &i[..]).collect();
            match a.module {
                m if m == FIELDS => formula::decode_fields(&a.params, &inputs, a.len),
                m if m == ENTRIES => formula::decode_entries(&a.params, &inputs, a.len),
                m if m == FLOATS => formula::decode_floats(&a.params, &inputs, a.len),
                m => panic!("{m:?}"),
            }
        }
        n => panic!("{n:?}"),
    }
}

fn f32_special() -> Vec<u32> {
    vec![
        0,
        0x8000_0000,
        1,
        0x8000_0001,
        0x007f_ffff,
        0x0080_0000,
        0x7f7f_ffff,
        0xff7f_ffff,
        0x7f80_0000,
        0xff80_0000,
        0x7fc0_0000,
        0xffc0_0000,
        0x7f80_0001,
        0xffff_ffff,
        0x3f80_0000,
    ]
}

fn f64_special() -> Vec<u64> {
    vec![
        0,
        1 << 63,
        1,
        (1 << 63) | 1,
        0x000f_ffff_ffff_ffff,
        0x0010_0000_0000_0000,
        0x7fef_ffff_ffff_ffff,
        0xffef_ffff_ffff_ffff,
        0x7ff0_0000_0000_0000,
        0xfff0_0000_0000_0000,
        0x7ff8_0000_0000_0000,
        0xfff8_0000_0000_0000,
        0x7ff0_0000_0000_0001,
        u64::MAX,
        0x3ff0_0000_0000_0000,
    ]
}

/// Every bit pattern of the specials, with random ones, lifted (at the widest widths, nothing
/// dropped) and put back: the same bytes, for every kind of float and scale, even one that is
/// none (0, infinite, NaN, negative).
#[test]
fn special_floats_lift_and_back() {
    let mut rng = Rng(7);
    let scales = [0.01, 1.0, 1e-300, 1e300, 0.0, -0.5, f64::INFINITY, f64::NAN, f64::MIN_POSITIVE, 5e-324];
    for wide in [false, true] {
        for big in [false, true] {
            for div in [false, true] {
                for &k in &scales {
                    let w = if wide { 8 } else { 4 };
                    let mut recs: Vec<u8> = Vec::new();
                    let mut push = |v: u64| {
                        let mut b = v.to_le_bytes()[..w].to_vec();
                        if big {
                            b.reverse();
                        }
                        recs.extend(b);
                        recs.extend([0xab, 0xcd]);
                    };
                    let list: Vec<u64> =
                        if wide { f64_special() } else { f32_special().into_iter().map(u64::from).collect() };
                    for &v in &list {
                        push(v);
                    }
                    for _ in 0..64 {
                        let v = rng.next();
                        push(if wide { v } else { v & 0xffff_ffff });
                    }
                    let k_rec = w + 2;
                    let n = recs.len() / k_rec;
                    let fl = [Float { off: 0, wide, big, div, k, wq: 8, wr: w }];
                    let lifted = floats::lift(&recs, k_rec, &fl);
                    let back = module::unlift(&lifted, n, k_rec, &fl).unwrap();
                    assert!(back == recs, "wide {wide} big {big} div {div} k {k}");
                    // (through the module, params and all)
                    let p = module::floats_params(n, k_rec, &fl);
                    assert!(module::decode_floats(&p, &[&lifted], recs.len() as u64).unwrap() == recs);
                }
            }
        }
    }
}

/// A column of floats on a scale (a few thousand records) with specials sprinkled among them:
/// whatever is inferred restores the file, in either byte order and width.
#[test]
fn special_floats_in_files() {
    for (wide, big, sprinkled) in (0..8).map(|i| (i & 1 != 0, i & 2 != 0, i & 4 != 0)) {
        {
            let mut rng = Rng(11);
            let mut data = b"HEAD".to_vec();
            let n = 3000u32;
            for i in 0..n {
                let q = (rng.next() % 2000) as i64;
                let x = q as f64 * 0.01;
                let special = match if sprinkled { i % 211 } else { u32::MAX } {
                    0 => Some(f64::NAN),
                    1 => Some(f64::INFINITY),
                    2 => Some(f64::NEG_INFINITY),
                    3 => Some(-0.0),
                    4 => Some(f64::from_bits(1)),
                    5 => Some(f64::from_bits(0x000f_ffff_ffff_ffff)),
                    6 => Some(f64::MAX),
                    _ => None,
                };
                let x = special.unwrap_or(x);
                let mut b =
                    if wide { x.to_bits().to_le_bytes().to_vec() } else { (x as f32).to_bits().to_le_bytes().to_vec() };
                if big {
                    b.reverse();
                }
                data.extend(b);
                data.extend((i * 3).to_le_bytes());
            }
            data.extend(b"tail");
            let k = if wide { 12 } else { 8 };
            assert_eq!(data.len(), 4 + k * n as usize + 4);
            let region = Region::fixed(&data, 4, if wide { 12 } else { 8 }, n as usize);
            let choices = vec![Choice { gain: 0, region, form: Form::Fields }];
            let lifted = choices.iter().any(|c| !c.region.floats.is_empty());
            // (without specials the floats are lifted; one far from its q's float in a column (a NaN,
            // an infinity) makes its differences wider than lifted: the column is left as it is)
            assert!(lifted != sprinkled, "wide {wide} big {big} sprinkled {sprinkled}");
            let f = formula::formula(&data, &choices);
            assert!(eval(&f, &f.root).unwrap() == data, "wide {wide} big {big} {sprinkled}");
            let g = formula::formula_in(&data, &choices, 5000, 100);
            assert!(eval(&g, &g.root).unwrap() == data, "groups: wide {wide} big {big}");
        }
    }
}

/// Tiny and random inputs: nothing found is wrong, nothing panics.
#[test]
fn tiny_and_random_inputs() {
    let mut rng = Rng(5);
    for len in (0..300).chain([1000, 4096, 20_000]) {
        let data: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        for d in [data.clone(), vec![0; len], vec![0xff; len], (0..len).map(|i| i as u8).collect()] {
            let c = infer(&d, 1);
            if !c.is_empty() {
                let f = formula::formula(&d, &c);
                assert!(eval(&f, &f.root).unwrap() == d, "len {len}");
            }
            let _ = infer_if(&d, 0.0, 1);
            if let Some(f) = formula::encode(&d, 1) {
                assert!(eval(&f, &f.root).unwrap() == d);
            }
        }
    }
}

type App = (u8, Vec<u8>, Vec<Vec<u8>>, u64);

/// The applications under a node, each with its inputs evaluated: (module 0 fields, 1 entries,
/// 2 floats, params, inputs, length).
fn collect(f: &Formula, n: &Node, out: &mut Vec<App>) {
    match n {
        Node::Concat(p) => p.iter().for_each(|p| collect(f, p, out)),
        Node::Apply(a) => {
            let inputs = a.inputs.iter().map(|i| eval(f, i).unwrap()).collect();
            let m = if a.module == FIELDS {
                0
            } else if a.module == ENTRIES {
                1
            } else {
                2
            };
            out.push((m, a.params.clone(), inputs, a.len));
            a.inputs.iter().for_each(|i| collect(f, i, out));
        }
        _ => {}
    }
}

/// The formula of a file of records and of entries, whose modules are then run on damaged
/// params and inputs: an error or a result, no panic, hang or huge allocation.
#[test]
fn decoders_damaged() {
    let mut rng = Rng(3);
    // fixed records with floats; entries: a 4-byte head with its own length, a tail
    let mut data = b"HDR!".to_vec();
    for i in 0..400u32 {
        data.extend(i.to_le_bytes());
        data.extend(((rng.next() % 2000) as f32 * 0.01).to_le_bytes());
        data.extend([1, 2, 3, 4]);
    }
    let mut ent = Vec::new();
    for i in 0..300u32 {
        let tail = (i % 7 + (rng.next() % 3) as u32) as usize;
        ent.extend((8 + tail as u32).to_le_bytes());
        ent.extend(i.to_le_bytes());
        ent.extend((0..tail).map(|j| (j as u32 * i) as u8));
    }
    let mut starts = vec![0];
    while *starts.last().unwrap() < ent.len() {
        let s = *starts.last().unwrap();
        starts.push(s + u32::from_le_bytes(ent[s..s + 4].try_into().unwrap()) as usize);
    }
    let rule = Rule { d: 0, kind: 7, b: 0, align: 1 };
    let region = Region::entries(&ent, starts, rule, Some(8));
    let ent_choices = vec![Choice { gain: 0, region, form: Form::Fields }];
    let fixed = vec![Choice { gain: 0, region: Region::fixed(&data, 4, 12, 400), form: Form::Fields }];
    let mut seen = [false; 3];
    for (d, c) in [(&data, fixed), (&ent, ent_choices)] {
        println!("{} regions in {} bytes", c.len(), d.len());
        let f = formula::formula(d, &c);
        let mut apps = Vec::new();
        collect(&f, &f.root, &mut apps);
        for (m, params, inputs, len) in apps {
            seen[m as usize] = true;
            let t = Duration::from_secs(20);
            let run = |p: &[u8], i: &[Vec<u8>], len: u64| {
                let i: Vec<&[u8]> = i.iter().map(|v| &v[..]).collect();
                let _ = match m {
                    0 => formula::decode_fields(p, &i, len),
                    1 => formula::decode_entries(p, &i, len),
                    _ => formula::decode_floats(p, &i, len),
                };
            };
            aqr_testkit::damaged(&params, 64, 64, t, |p| run(p, &inputs, len));
            for k in 0..inputs.len() {
                aqr_testkit::damaged(&inputs[k], 48, 16, t, |x| {
                    let mut i = inputs.clone();
                    i[k] = x.to_vec();
                    run(&params, &i, len)
                });
            }
            for l in [0, 1, len.wrapping_sub(1), len + 1, u64::MAX, 1 << 40] {
                run(&params, &inputs, l);
            }
        }
    }
    println!("modules seen: {seen:?}");
    assert!(seen[0] && seen[1], "{seen:?}");
}
