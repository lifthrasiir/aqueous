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
                cc[j] ^= 1 << (r >> 40) % 8;
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
