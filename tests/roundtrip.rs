use obj_recons::container::{Options, Stats, compress_with};
use obj_recons::{compress, decompress};

/// Round trip with verification off, so that the coded path itself is checked; small
/// blocks too, so that lines are cut and state carries over.
fn rt(src: &[u8]) {
    for block in [1 << 10, 4 << 20] {
        let opt = Options { verify: false, block, ..Default::default() };
        let mut st = Stats::default();
        let c = compress_with(src, &opt, &mut st);
        assert_eq!(decompress(&c).unwrap(), src, "block {block}: {:?}", String::from_utf8_lossy(&src[..src.len().min(200)]));
        assert_eq!(st.whole, 0, "a block failed to code");
    }
    // Blocks stored whole in between (verification off, or it would hide a mismatch): the
    // encoder must not use what it learnt from them.
    let opt = Options { block: 1 << 10, verify: false, force_whole: 0x5A5A_1234_8421_0F0F, ..Default::default() };
    let c = compress_with(src, &opt, &mut Stats::default());
    assert_eq!(decompress(&c).unwrap(), src);
    // Threads: the same bytes, whole blocks or not, and the same file back.
    for (threads, force_whole) in [(2, 0), (3, 0x5A5A_1234_8421_0F0F), (5, u64::MAX), (4, 1)] {
        for verify in [false, true] {
            let opt = Options { block: 1 << 10, verify, force_whole, threads, ..Default::default() };
            let seq = Options { threads: 1, ..opt };
            let c = compress_with(src, &opt, &mut Stats::default());
            assert_eq!(c, compress_with(src, &seq, &mut Stats::default()), "threads {threads}");
            assert_eq!(obj_recons::container::decompress_threads(&c, threads).unwrap(), src);
        }
    }
    assert_eq!(decompress(&compress(src)).unwrap(), src);
}

#[test]
fn small_cases() {
    for s in [
        "",
        "\n",
        "v",
        "v 1 2 3",
        "v 1 2 3\n",
        "v 1 2 3\r\n",
        "v  1\t2   3 \n",
        "v 1.5 -0 -0.000 .5 -.5 5. 1e5 1E-05 nan inf\n",
        "v 0.1 0.2\nvt 0.5 0.5\nvn 0 0 1\nvp 0.3\n",
        "f 1 2 3\nf 1/1 2/2 3/3\nf 1//1 2//2 3//3\nf 1/1/1 2/2/2 3/3/3\n",
        "v 0 0 0\nv 1 0 0\nv 0 1 0\nf -3 -2 -1\nf -3/-1 -2/-1 -1/-1\n",
        "f 0 1 2\nf 01 2 3\nf 1/ 2/ 3/\nf a b c\nf 1/2/3/4\n",
        "l 1 2 3 4 5\n",
        "# comment\ng group\nusemtl m\ns off\no obj\nmtllib x.mtl\n",
        "\r\n\r\n\n\n",
        "v 1 2 3 4 5 6 7 8 9 10\n",
        "v 1.0000000000000000000001 2\n",
        "v 123456789012345678901234567890\n",
        "vn 0.577350 0.577350 0.577350\nvn 0 0 -1\nvn 1 0 0\nvn -0.000000 1.000000 0.000000\nvn 2 2 2\n",
    ] {
        rt(s.as_bytes());
    }
}

/// C's printf of a double.
fn c_fmt(fmt: &std::ffi::CStr, x: f64) -> String {
    unsafe extern "C" {
        fn snprintf(buf: *mut u8, n: usize, fmt: *const std::ffi::c_char, ...) -> i32;
    }
    let mut b = [0u8; 400];
    let n = unsafe { snprintf(b.as_mut_ptr(), b.len(), fmt.as_ptr(), x) } as usize;
    String::from_utf8(b[..n.min(b.len() - 1)].to_vec()).unwrap()
}

/// Every writer style, mixed in one file, with a vertex, a normal and a texture coordinate
/// on each line.
#[test]
fn writer_styles() {
    let xs = [0.1f64, -1.5, 476.1832, 1e-5, 123456.789, 0.0, -0.0, 3.4e38, 1e-30, 2.0];
    let mut s = String::new();
    for &x in &xs {
        let f = x as f32 as f64;
        for t in [
            c_fmt(c"%f", x),
            c_fmt(c"%g", x),
            c_fmt(c"%.9g", f),
            c_fmt(c"%.17g", x),
            c_fmt(c"%e", f),
            c_fmt(c"%.18e", x),
            format!("{}", x as f32),
            format!("{x}"),
        ] {
            s += &format!("v {t} {t} {t}\nvn {t} 0 1\nvt {t} {t}\n");
        }
    }
    rt(s.as_bytes());
    // Lines kept as text: only those of 3.4e38 printed positionally (39 digits), 5 per kind.
    let mut st = Stats::default();
    compress_with(s.as_bytes(), &Options::default(), &mut st);
    let raw = st.raw[obj_recons::block::S_TEXT];
    assert!(raw < 1300, "{raw} bytes of text");
}

/// Lines longer than a block, and a block holding no line end.
#[test]
fn long_lines() {
    let mut s = String::from("v 1 2 3\n# ");
    s.extend(std::iter::repeat_n('x', 5000));
    s += "\nf";
    for i in 1..2000 {
        s += &format!(" {i}");
    }
    s += "\nv 4 5 6\n";
    rt(s.as_bytes());
}

/// More templates than the table holds.
#[test]
fn many_templates() {
    let mut s = String::new();
    for i in 0..70000 {
        let sep = " ".repeat(1 + i % 7);
        s += &format!("v{sep}{}{}{}\n", i % 100, " ".repeat(1 + i / 7 % 11), "0.".to_string() + &"0".repeat(i / 77 % 15) + "1");
    }
    rt(s.as_bytes());
}

/// A mesh with shared vertices, faces in an irregular order, and separate vt and vn
/// numbering: every corner symbol is used. Blocks stored whole in between.
#[test]
fn mesh_corners() {
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
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            let (x, y) = if r.is_multiple_of(3) { ((r >> 8) as usize % (w - 1), (r >> 20) as usize % (h - 1)) } else { (x, y) };
            let a = y * w + x + 1;
            let (b, c, d) = (a + 1, a + w + 1, a + w);
            // The vt paired with a vertex changes over time (as at seams).
            let t = |i: usize| (i * 7 + (r >> 40) as usize % 3) % (w * h) + 1;
            s += &format!("f {a}/{}/{a} {b}/{}/{b} {c}/{}/{c}\nf {a}/{}/{} {c}/{}/{} {d}/{}/{}\n", t(a), t(b), t(c), t(a), w * h - a + 1, t(c), w * h - c + 1, t(d), w * h - d + 1);
        }
    }
    rt(s.as_bytes());
}

/// Unit normals in every direction (the predicted component and its sign change), in
/// fixed and `%g` formats, with vertices.
#[test]
fn unit_normals() {
    let mut s = String::new();
    let mut r = 77u64;
    for i in 0..20000 {
        let mut g = || {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            (r >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
        };
        let (x, y, z) = (g(), g(), g());
        let l = (x * x + y * y + z * z).sqrt().max(1e-9);
        let (x, y, z) = (x / l, y / l, z / l);
        s += &format!("v {:.6} {:.6} {:.6}\n", x * 3.0, y * 3.0, z * 3.0);
        if i % 2 == 0 {
            s += &format!("vn {x:.6} {y:.6} {z:.6}\n");
        } else {
            s += &format!("vn {} {} {}\n", c_fmt(c"%g", x), c_fmt(c"%g", y), c_fmt(c"%g", z));
        }
    }
    rt(s.as_bytes());
}

/// Repeated vertices among lines of more than three numbers (not kept for references),
/// with blocks stored whole: a line number the encoder saw in an undone block must not
/// stand for another line.
#[test]
fn equal_lines_and_undone_blocks() {
    let mut s = String::new();
    let mut r = 3u64;
    for i in 0..30000 {
        r ^= r << 13;
        r ^= r >> 7;
        r ^= r << 17;
        let k = r % 50;
        if i % 3 == 0 {
            s += &format!("v {k}.5 {}.25 {}.75 1.0\n", k * 3, k * 7);
        } else {
            s += &format!("v {k}.5 {}.25 {}.75\n", k * 3, k * 7);
        }
        if i % 5 == 0 {
            s += &format!("vt 0.{k} 0.{}\n", (k * 7) % 10);
        }
    }
    for mask in [0x5A5A_1234_8421_0F0Fu64, 0xAAAA_AAAA_AAAA_AAAA, 0x8888_4444_2222_1111] {
        for block in [1 << 10, 4 << 10] {
            let opt = Options { block, verify: false, force_whole: mask, ..Default::default() };
            assert_eq!(decompress(&compress_with(s.as_bytes(), &opt, &mut Stats::default())).unwrap(), s.as_bytes());
        }
    }
}

#[test]
fn binary_junk() {
    let mut r = 0x1234567u64;
    let v: Vec<u8> = (0..200000)
        .map(|_| {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            r as u8
        })
        .collect();
    rt(&v);
}

/// Corrupted containers: errors, never panics.
#[test]
fn corrupt_containers() {
    let mut s = String::new();
    for i in 0..3000 {
        s += &format!("v {}.{:03} {} -{}.5\nvn 0 0.6 0.8\nvt 0.{i} 1\n", i, i % 1000, i * 7 % 13, i % 5);
        if i > 2 {
            s += &format!("f {}/{}/{} {}/{}/{} {}/{}/{}\n", i, i, i, i - 1, i - 1, i - 1, i - 2, i - 2, i - 2);
        }
    }
    let c = compress_with(s.as_bytes(), &Options { block: 4096, ..Default::default() }, &mut Stats::default());
    assert_eq!(decompress(&c).unwrap(), s.as_bytes());
    let mut r = 99u64;
    for k in 0..3000 {
        let mut cc = c.clone();
        r ^= r << 13;
        r ^= r >> 7;
        r ^= r << 17;
        if k % 4 == 0 {
            cc.truncate(r as usize % cc.len());
        } else {
            for _ in 0..1 + k % 8 {
                r ^= r << 13;
                r ^= r >> 7;
                r ^= r << 17;
                let j = r as usize % cc.len();
                cc[j] ^= 1 << ((r >> 40) % 8);
            }
        }
        if let Ok(d) = decompress(&cc) {
            assert_eq!(d, s.as_bytes());
        }
    }
}

#[test]
#[ignore]
fn dump_writer_styles() {
    let xs = [0.1f64, -1.5, 476.1832, 1e-5, 123456.789, 0.0, -0.0, 3.4e38, 1e-30, 2.0];
    let mut s = String::new();
    for &x in &xs {
        let f = x as f32 as f64;
        for t in [c_fmt(c"%f", x), c_fmt(c"%g", x), c_fmt(c"%.9g", f), c_fmt(c"%.17g", x), c_fmt(c"%e", f), c_fmt(c"%.18e", x), format!("{}", x as f32), format!("{x}")] {
            s += &format!("v {t} {t} {t}\nvn {t} 0 1\nvt {t} {t}\n");
        }
    }
    std::fs::write(std::env::var("OUT").unwrap(), s).unwrap();
}
