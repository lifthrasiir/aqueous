use obj_recons::container::Options;
use obj_recons::formula::{self, MODULE};
use recons_formula::{Apply, Formula};

fn eval(f: &Formula, threads: usize) -> recons_formula::Result<Vec<u8>> {
    f.eval(&|a: &Apply, inputs: &[&[u8]]| {
        assert_eq!(a.module, MODULE);
        formula::decode(&a.params, inputs, a.len, threads).map_err(|e| recons_formula::Error::new(e.to_string()))
    })
}

/// Round trip through the formula under several block sizes, blocks stored whole and
/// threads; returns the formula of the default options.
fn rt(src: &[u8]) -> Option<Formula> {
    let mut first = None;
    for (block, force_whole, threads) in [
        (4 << 20, 0, 1),
        (1 << 10, 0, 1),
        (1 << 10, 0, 3),
        (1 << 10, 0x5A5A_1234_8421_0F0F, 1),
        (1 << 10, 0x5A5A_1234_8421_0F0F, 4),
        (1 << 10, 0xAAAA_AAAA_AAAA_AAAA, 2),
    ] {
        let opt = Options { block, force_whole, threads, ..Default::default() };
        let Some(f) = formula::encode_with(src, &opt) else { continue };
        assert_eq!(f.len(), src.len() as u64);
        for t in [1, 3] {
            assert_eq!(eval(&f, t).unwrap(), src, "block {block}, whole {force_whole:x}, threads {t}");
        }
        // The same formula whatever the threads of the encoder.
        assert_eq!(Some(&f), formula::encode_with(src, &Options { threads: 1, ..opt }).as_ref());
        first.get_or_insert(f);
    }
    first
}

fn xorshift(r: &mut u64) -> u64 {
    *r ^= *r << 13;
    *r ^= *r >> 7;
    *r ^= *r << 17;
    *r
}

#[test]
fn small_cases() {
    for s in [
        "v",
        "\n",
        "v 1 2 3",
        "v 1 2 3\n",
        "v 1 2 3\r\n",
        "v 1.5 -0 -0.000 .5 -.5 5. 1e5 1E-05 nan inf\n",
        "v 0.1 0.2\nvt 0.5 0.5\nvn 0 0 1\nvp 0.3\n",
        "f 1 2 3\nf 1/1 2/2 3/3\nf 1//1 2//2 3//3\nf 1/1/1 2/2/2 3/3/3\n",
        "v 0 0 0\nv 1 0 0\nv 0 1 0\nf -3 -2 -1\nf -3/-1 -2/-1 -1/-1\n",
        "f 0 1 2\nf 01 2 3\nf 1/ 2/ 3/\nf a b c\nf 1/2/3/4\n",
        "# comment\ng group\nusemtl m\ns off\no obj\nmtllib x.mtl\n",
        "v 1 2 3\nv 1 2 3\nf 1 2 -1\n# hi\n",
    ] {
        assert!(rt(s.as_bytes()).is_some(), "{s:?}");
    }
}

#[test]
fn none_cases() {
    assert!(formula::encode(b"", 1).is_none());
    // Every block stored whole.
    let opt = Options { force_whole: u64::MAX, ..Default::default() };
    assert!(formula::encode_with(b"v 1 2 3\n", &opt).is_none());
    let opt = Options { block: 1 << 10, force_whole: u64::MAX, ..Default::default() };
    assert!(formula::encode_with(&b"v 1 2 3\n".repeat(1000), &opt).is_none());
}

#[test]
fn structure() {
    let src = b"v 1 2 3\nv 1 2 3\nf 1 2 -1\n# hi\n";
    let f = formula::encode(src, 1).unwrap();
    let recons_formula::Node::Apply(a) = &f.root else { panic!("not an application") };
    assert_eq!((a.module, a.params.as_slice(), a.len), (MODULE, &[][..], src.len() as u64));
    assert_eq!(a.inputs.len(), 2 + obj_recons::block::NSTREAMS);
    assert_eq!(f.streams.len(), a.inputs.len());
    assert_eq!(f.streams[0].class, "obj/index");
    assert_eq!(f.streams[1].class, "obj/whole");
    assert_eq!(f.streams[2].class, "obj/ops");
    // One coded block; nothing whole.
    assert_eq!(f.streams[0].data[0] as usize, src.len() << 1 | 1);
    assert!(f.streams[1].data.is_empty());
}

#[test]
fn mesh() {
    let mut s = String::new();
    let (w, h) = (60, 60);
    for y in 0..h {
        for x in 0..w {
            s += &format!("v {x}.0 {y}.0 {}\nvt 0.{x:02} 0.{y:02}\nvn 0 0 1\n", (x * y) % 7);
        }
    }
    let mut r = 5u64;
    for y in 0..h - 1 {
        for x in 0..w - 1 {
            let (x, y) = if xorshift(&mut r).is_multiple_of(3) {
                ((r >> 8) as usize % (w - 1), (r >> 20) as usize % (h - 1))
            } else {
                (x, y)
            };
            let a = y * w + x + 1;
            let (b, c, d) = (a + 1, a + w + 1, a + w);
            s += &format!("f {a}/{a}/{a} {b}/{b}/{b} {c}/{c}/{c}\nf {a}/{a}/{a} {c}/{c}/{c} {d}/{d}/{d}\n");
        }
    }
    let f = rt(s.as_bytes()).unwrap();
    // rANS coded streams appear in the slots, and the formula is smaller than the input.
    let total: usize = f.streams.iter().map(|s| s.data.len()).sum();
    assert!(total < s.len() / 3, "{total} of {}", s.len());
}

#[test]
fn unit_normals_and_equal_lines() {
    let mut s = String::new();
    let mut r = 77u64;
    for i in 0..20000 {
        let mut g = || (xorshift(&mut r) >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0;
        let (x, y, z) = (g(), g(), g());
        let l = (x * x + y * y + z * z).sqrt().max(1e-9);
        let (x, y, z) = (x / l, y / l, z / l);
        s += &format!("v {:.6} {:.6} {:.6}\nvn {x:.6} {y:.6} {z:.6}\n", x * 3.0, y * 3.0, z * 3.0);
        let k = xorshift(&mut r) % 50;
        s += &format!("v {k}.5 {}.25 {}.75{}\n", k * 3, k * 7, if i % 3 == 0 { " 1.0" } else { "" });
    }
    rt(s.as_bytes()).unwrap();
}

#[test]
fn long_lines_and_junk() {
    let mut s = String::from("v 1 2 3\n# ");
    s.extend(std::iter::repeat_n('x', 5000));
    s += "\nf";
    for i in 1..2000 {
        s += &format!(" {i}");
    }
    s += "\nv 4 5 6\n";
    rt(s.as_bytes());
    let mut r = 0x1234567u64;
    let v: Vec<u8> = (0..200000).map(|_| xorshift(&mut r) as u8).collect();
    rt(&v);
}

/// Damaged inputs: errors or other output, never panics.
#[test]
fn corrupt_inputs() {
    let mut s = String::new();
    for i in 0..3000 {
        s += &format!("v {}.{:03} {} -{}.5\nvn 0 0.6 0.8\nvt 0.{i} 1\n", i, i % 1000, i * 7 % 13, i % 5);
        if i > 2 {
            s += &format!("f {}/{}/{} {}/{}/{} {}/{}/{}\n", i, i, i, i - 1, i - 1, i - 1, i - 2, i - 2, i - 2);
        }
    }
    let opt = Options { block: 4096, force_whole: 4, ..Default::default() };
    let f = formula::encode_with(s.as_bytes(), &opt).unwrap();
    let recons_formula::Node::Apply(a) = &f.root else { panic!() };
    let ins: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
    assert_eq!(formula::decode(&a.params, &ins, a.len, 1).unwrap(), s.as_bytes());
    let mut r = 99u64;
    for k in 0..2000 {
        let mut data: Vec<Vec<u8>> = ins.iter().map(|d| d.to_vec()).collect();
        let i = if k % 3 == 0 { 0 } else { xorshift(&mut r) as usize % data.len() };
        if data[i].is_empty() {
            continue;
        }
        if k % 4 == 0 {
            let n = xorshift(&mut r) as usize % data[i].len();
            data[i].truncate(n);
        } else {
            let j = xorshift(&mut r) as usize % data[i].len();
            data[i][j] ^= 1 << ((r >> 40) % 8);
        }
        let ins: Vec<&[u8]> = data.iter().map(|d| &d[..]).collect();
        for len in [a.len, a.len + 1, a.len.saturating_sub(5)] {
            let _ = formula::decode(&a.params, &ins, len, 1 + k % 3);
        }
        let _ = formula::decode(&[0], &ins, a.len, 1);
        let _ = formula::decode(&a.params, &ins[1..], a.len, 1);
    }
}

/// `formula::decode` against the container's `decompress` (`cargo test --release --test
/// formula -- --ignored --nocapture`).
#[test]
#[ignore]
fn bench_decode() {
    let mut s = String::new();
    let mut r = 5u64;
    let n = 400;
    for i in 0..n * n {
        let (x, y) = (i % n, i / n);
        let z = (x * y) % 97;
        s += &format!(
            "v {x}.{:03} {y}.{:03} {z}.5\nvt 0.{x:03} 0.{y:03}\nvn 0.{:06} 0.5 -0.{:06}\n",
            i % 1000,
            i * 7 % 1000,
            i * 13 % 999999,
            i % 999999
        );
    }
    for i in 0..n * n - n - 1 {
        let a = i + 1 + (xorshift(&mut r) % 3) as usize;
        s += &format!("f {a}/{a}/{a} {}/{}/{} {}/{}/{}\n", a + 1, a + 1, a + 1, a + n, a + n, a + n);
    }
    let src = s.as_bytes();
    let packed = obj_recons::compress(src);
    let f = formula::encode(src, 4).unwrap();
    let recons_formula::Node::Apply(a) = &f.root else { panic!() };
    let ins: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
    println!("{} bytes, container {}, slots {}", src.len(), packed.len(), ins.iter().map(|d| d.len()).sum::<usize>());
    for threads in [1, 4] {
        let best = |f: &dyn Fn() -> Vec<u8>| {
            (0..5)
                .map(|_| {
                    let t = std::time::Instant::now();
                    assert_eq!(f(), src);
                    t.elapsed().as_secs_f64() * 1e3
                })
                .fold(f64::MAX, f64::min)
        };
        let c = best(&|| obj_recons::container::decompress_threads(&packed, threads).unwrap());
        let m = best(&|| formula::decode(&a.params, &ins, a.len, threads).unwrap());
        println!("threads {threads}: container {c:.1} ms, formula {m:.1} ms");
    }
}
