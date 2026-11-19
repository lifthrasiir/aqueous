//! Edge cases: a committed DivX 3 AVI (ffmpeg's msmpeg4: I and P frames, AC-3 audio) end to end, AVIs
//! of odd structure (a `movi` in a header string, `ix00` and `AVIX`, truncation, forged sizes), the
//! module's plans at large pictures, and damaged or lying module inputs.

use std::time::Duration;

use aqr_archive::Evaluate;
use aqr_div3::{Codec, FrameKind, formula, module, native};
use aqr_formula::Formula;
use aqr_testkit::{Refusing, case, damaged, most, within};

#[global_allocator]
static ALLOC: Refusing = Refusing(1 << 30);

const LIMIT: Duration = Duration::from_secs(60);

fn fixture() -> Vec<u8> {
    std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/divx3-ac3.avi")).unwrap()
}

fn chunk(id: &[u8; 4], d: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend_from_slice(&(d.len() as u32).to_le_bytes());
    v.extend_from_slice(d);
    if d.len() & 1 == 1 {
        v.push(0);
    }
    v
}

fn list(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = b"LIST".to_vec();
    v.extend_from_slice(&(body.len() as u32 + 4).to_le_bytes());
    v.extend_from_slice(kind);
    v.extend_from_slice(body);
    v
}

enum S {
    /// A video stream: handler, width and height as the bitmap header holds them.
    Video([u8; 4], i32, i32),
    /// An audio stream of a format tag.
    Audio(u16),
}

/// An AVI of `streams`; `strn` is put in the first stream's list as a chunk, `items` are the chunks of the data list as
/// they are, `after` follows the data list.
fn avi(streams: &[S], strn: &[u8], items: &[Vec<u8>], after: &[u8]) -> Vec<u8> {
    let mut hdrl = chunk(b"avih", &[0; 56]);
    for (i, s) in streams.iter().enumerate() {
        let mut l = Vec::new();
        match s {
            S::Video(h, w, ht) => {
                let mut strh = b"vids".to_vec();
                strh.extend_from_slice(h);
                strh.extend_from_slice(&[0; 48]);
                let mut strf = Vec::new();
                for v in [40u32, *w as u32, *ht as u32, 0x0018_0001, u32::from_le_bytes(*b"DIV3")] {
                    strf.extend_from_slice(&v.to_le_bytes());
                }
                strf.extend_from_slice(&[0; 20]);
                l.extend(chunk(b"strh", &strh));
                l.extend(chunk(b"strf", &strf));
            }
            S::Audio(tag) => {
                let mut strh = b"auds\0\0\0\0".to_vec();
                strh.extend_from_slice(&[0; 48]);
                l.extend(chunk(b"strh", &strh));
                l.extend(chunk(b"strf", &[&tag.to_le_bytes()[..], &[0; 16]].concat()));
            }
        }
        if i == 0 && !strn.is_empty() {
            l.extend(chunk(b"strn", strn));
        }
        hdrl.extend(list(b"strl", &l));
    }
    let mut body = b"AVI ".to_vec();
    body.extend(list(b"hdrl", &hdrl));
    body.extend(list(b"movi", &items.concat()));
    body.extend_from_slice(after);
    let mut v = b"RIFF".to_vec();
    v.extend_from_slice(&(body.len() as u32).to_le_bytes());
    v.extend(body);
    v
}

fn noise(seed: usize, n: usize) -> Vec<u8> {
    let mut x = (seed as u32).wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

/// An AVI of DivX 3 video (noise: frames that do not parse) of `n` frames, odd and even in length.
fn noisy(n: usize) -> Vec<u8> {
    let items: Vec<Vec<u8>> = (0..n).map(|i| chunk(b"00dc", &noise(i, 300 + 7 * i))).collect();
    avi(&[S::Video(*b"div3", 64, 48)], &[], &items, &[])
}

/// What the modules do for the formulas of this crate and the ones it embeds: natively, or serially (as the Wasm
/// modules do).
fn modules(a: &aqr_formula::Apply, i: &[&[u8]], serial: bool) -> aqr_formula::Result<Vec<u8>> {
    let err = |e: &dyn std::fmt::Display| aqr_formula::Error(e.to_string());
    match a.module.name {
        "div3-recons" if serial => module::decode_serial(&a.params, i, a.len).map_err(|e| err(&e)),
        "div3-recons" => formula::decode(&a.params, i, a.len, 2).map_err(|e| err(&e)),
        "ac3-recons" if serial => aqr_ac3::module::decode_serial(&a.params, i, a.len, 1).map_err(|e| err(&e)),
        "ac3-recons" => aqr_ac3::formula::decode(&a.params, i, a.len, 2).map_err(|e| err(&e)),
        "mp3-recons" => {
            let r: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
            aqr_mp3::formula::decode(&a.params, &r, i, a.len, 2).map_err(|e| err(&e))
        }
        m => panic!("{m}"),
    }
}

fn evaluates_to(f: &Formula, file: &[u8]) {
    let native = f.eval(&|a: &aqr_formula::Apply, i: &[&[u8]]| modules(a, i, false)).unwrap();
    assert!(native == file, "native decoding differs");
    let serial = f.eval(&|a: &aqr_formula::Apply, i: &[&[u8]]| modules(a, i, true)).unwrap();
    assert!(serial == file, "serial decoding differs");
}

/// The formula of `file` (frames and audio taken whatever they come to), checked to give it back.
fn roundtrip(file: &[u8]) -> Option<Formula> {
    let f = formula::encode_unchecked(file, 2)?;
    evaluates_to(&f, file);
    Some(f)
}

fn size(f: &Formula, class: &str) -> usize {
    f.streams.iter().filter(|s| s.class.starts_with(class)).map(|s| s.data.len()).sum()
}

#[test]
fn fixture_is_divx3_with_i_and_p_frames() {
    let file = fixture();
    let v = formula::video(&file).expect("an AVI with video");
    assert!(v.div3 && v.ac3 && !v.mp3);
    assert_eq!((v.width, v.height), (64, 48));
    assert_eq!(v.frames.len(), 8);
    assert!(!v.audio.is_empty());
    let mut codec = Codec::new(64, 48);
    let kinds: Vec<_> = v
        .frames
        .iter()
        .map(|r| {
            let mut enc = aqr_div3::Encoder::new();
            codec.encode(&mut enc, &file[r.clone()])
        })
        .collect();
    assert!(kinds.iter().all(Option::is_some), "every frame parses: {kinds:?}");
    assert_eq!(kinds[0], Some(FrameKind::I));
    assert_eq!(kinds.iter().filter(|k| **k == Some(FrameKind::I)).count(), 2, "gop of 4: {kinds:?}");
    assert_eq!(kinds.iter().filter(|k| **k == Some(FrameKind::P)).count(), 6);
}

#[test]
fn fixture_round_trips_smaller() {
    let file = fixture();
    let f = formula::encode(&file, 2).expect("a formula");
    evaluates_to(&f, &file);
    let v = formula::video(&file).unwrap();
    let frames: usize = v.frames.iter().map(|r| r.len()).sum();
    assert!(size(&f, formula::CODED) < frames, "{} -> {}", frames, size(&f, formula::CODED));
    assert_eq!(size(&f, formula::KEPT), 0);
    // the same on one thread
    let g = formula::encode(&file, 1).expect("a formula");
    assert!(g.streams.iter().map(|s| &s.data).eq(f.streams.iter().map(|s| &s.data)), "threads change the streams");
}

#[test]
fn fixture_damaged_round_trips_or_is_refused() {
    let file = fixture();
    damaged(&file, 40, 60, LIMIT, |d| {
        if let Some(f) = formula::encode_unchecked(d, 1) {
            evaluates_to(&f, d);
        }
        let _ = formula::starts(d);
    });
}

#[test]
fn fixture_cut_everywhere_in_the_headers_and_frames() {
    let file = fixture();
    for at in (0..file.len()).step_by(97).chain(file.len() - 12..file.len()) {
        let d = &file[..at];
        let r = within(LIMIT, || {
            if let Some(f) = formula::encode_unchecked(d, 1) {
                evaluates_to(&f, d);
            }
        });
        assert!(r.is_ok(), "cut at {at}: {r:?}");
    }
}

#[test]
fn damaged_module_inputs_fail_cleanly() {
    let file = fixture();
    let v = formula::video(&file).unwrap();
    let f = formula::encode(&file, 1).unwrap();
    let get = |class: &str| f.streams.iter().find(|s| &*s.class == class).unwrap().data.to_vec();
    let audio: Vec<u8> = v.audio.iter().flat_map(|r| file[r.clone()].iter().copied()).collect();
    let (coded, skeleton, layout) = (get(formula::CODED), get(formula::SKELETON), get(formula::LAYOUT));
    let params = [64, 0, 48, 0];
    let len = file.len() as u64;
    let inputs = [coded, skeleton, layout, audio];
    let run = |p: &[u8], i: [&[u8]; 4], len: u64| {
        let _ = native::decode(p, &i, len, 2);
        let _ = module::decode_serial(p, &i, len);
    };
    let [c, s, l, a] = [&inputs[0][..], &inputs[1][..], &inputs[2][..], &inputs[3][..]];
    assert!(native::decode(&params, &[c, s, l, a], len, 2).unwrap() == file);
    damaged(c, 60, 40, LIMIT, |d| run(&params, [d, s, l, a], len));
    damaged(s, 60, 60, LIMIT, |d| run(&params, [c, d, l, a], len));
    damaged(l, 60, 60, LIMIT, |d| run(&params, [c, s, d, a], len));
    damaged(a, 30, 10, LIMIT, |d| run(&params, [c, s, l, d], len));
    damaged(&params, 4, 4, LIMIT, |d| run(d, [c, s, l, a], len));
    // lengths of the file that are wrong
    for l2 in [0, 1, len - 1, len + 1, u64::MAX, 1 << 40] {
        run(&params, [c, s, l, a], l2);
    }
}

#[test]
fn lying_layouts_and_tables_cost_nothing() {
    use aqr_common::varint::put;
    case("lying layouts");
    let layout_of = |items: &[(u64, u64, bool)]| {
        let mut v = Vec::new();
        for &(gap, n, audio) in items {
            put(&mut v, gap);
            put(&mut v, n << 1 | audio as u64);
        }
        v
    };
    let table = |t: &[(u64, u64)]| {
        let mut v = Vec::new();
        put(&mut v, t.len() as u64);
        for &(a, b) in t {
            put(&mut v, a);
            put(&mut v, b);
        }
        v
    };
    let frame_max = module::FRAME_MAX as u64;
    // frames of the most the module takes, a few thousand of them, in one chunk of a table: the output's room is
    // reserved from what the data holds, not what the layout says
    let many = layout_of(&vec![(0, frame_max, false); 5000]);
    let mut coded = table(&[(5000, 3)]);
    coded.extend([0; 3]);
    let some = layout_of(&vec![(0, frame_max, false); 20]);
    let mut some_coded = table(&[(20, 3)]);
    some_coded.extend([0; 3]);
    let cases: Vec<(&str, Vec<u8>, Vec<u8>, u64)> = vec![
        ("20 frames of FRAME_MAX in one chunk", some, some_coded, 20 * frame_max),
        ("5000 frames of FRAME_MAX, len lies", many, coded.clone(), 1 << 20),
        ("a frame past FRAME_MAX", layout_of(&[(0, frame_max + 1, false)]), table(&[(1, 0)]), 1 << 30),
        ("a gap past u64", layout_of(&[(u64::MAX, 1, false)]), table(&[(1, 0)]), 10),
        ("a length past u64", layout_of(&[(0, u64::MAX >> 1, true)]), table(&[]), 10),
        ("gaps adding past u64", layout_of(&[(u64::MAX, 0, true), (u64::MAX, 0, true)]), table(&[]), 10),
        (
            "100 chunks of a frame",
            layout_of(&vec![(0, 1, false); 100]),
            {
                let mut t = table(&vec![(1, 1); 100]);
                t.extend(vec![0u8; 100]);
                t
            },
            100,
        ),
        (
            "20000 chunks of nothing",
            layout_of(&[(0, 1, false)]),
            {
                let mut t = table(&vec![(0, 0); 20000]);
                t.push(0);
                t
            },
            1,
        ),
        ("a table of chunks of frames the layout lacks", vec![], table(&[(1 << 60, 0)]), 0),
        ("a table whose bytes pass the input", vec![], table(&[(0, 1 << 60)]), 0),
    ];
    for (name, layout, coded, len) in &cases {
        let t = std::time::Instant::now();
        let r = within(Duration::from_secs(60), || {
            for params in [[64, 0, 48, 0], [0xff, 0xff, 0xff, 0xff], [0; 4]] {
                let _ = native::decode(&params, &[coded, &[], layout, &[]], *len, 2);
                let _ = module::decode_serial(&params, &[coded, &[], layout, &[]], *len);
            }
        });
        assert!(r.is_ok(), "{name}: {r:?}");
        assert!(t.elapsed() < Duration::from_secs(10), "{name}: {:?}", t.elapsed());
    }
}

#[test]
fn plans_follow_the_picture() {
    let initial = 1 << 20;
    let plan = |w: u16, h: u16| {
        let mut p = w.to_le_bytes().to_vec();
        p.extend(h.to_le_bytes());
        module::plan(initial, &p)
    };
    let kept = module::plan(initial, &[0; 4]).unwrap();
    let small = plan(64, 48).unwrap();
    let hd = plan(1920, 1080).unwrap();
    let big = plan(4096, 4096).unwrap();
    assert!(kept < small && small < hd && hd < big, "{kept} {small} {hd} {big}");
    // the model is 16 MiB; the picture's scratch a byte or two a macroblock's block
    let mib = |n: u64| n as f64 / (1 << 20) as f64;
    println!("plan: kept {:.1} small {:.1} 1080p {:.1} 4096^2 {:.1} MiB", mib(kept), mib(small), mib(hd), mib(big));
    assert!(big < 128 << 20, "a 4096 by 4096 picture's plan is {} MiB", mib(big));
    // sizes past what the encoder takes still have a plan (the application bound turns them away)
    let huge = plan(65535, 65535).unwrap();
    println!("plan: 65535^2 {:.1} MiB", mib(huge));
    assert!(huge > big);
    assert!(plan(0, 5).is_some());
    assert!(module::plan(initial, &[0; 3]).is_none() && module::plan(initial, &[]).is_none());
    // what the codec takes of the heap is within its plan (the plan's blocks are of the stack in Wasm)
    for (w, h) in [(64, 48), (1920, 1080), (4096, 4096)] {
        case("codec memory");
        let c = Codec::new(w, h);
        let most = most();
        drop(c);
        let p = plan(w as u16, h as u16).unwrap();
        assert!((most as u64) < p, "{w}x{h}: largest block {most} of a plan of {p}");
    }
}

#[test]
fn heights_and_widths_at_the_edges() {
    let frames = |n: usize| -> Vec<Vec<u8>> { (0..n).map(|i| chunk(b"00dc", &noise(i, 200))).collect() };
    let v = |w: i32, h: i32| avi(&[S::Video(*b"div3", w, h)], &[], &frames(5), &[]);
    // the bounds: width 1..=4096, height non-zero and at most 4096 in magnitude
    for (w, h, ok) in [
        (64, 48, true),
        (4096, 4096, true),
        (4096, -4096, true),
        (4097, 48, false),
        (64, 4097, false),
        (64, -4097, false),
        (0, 48, false),
        (-64, 48, false),
        (64, 0, false),
        (i32::MIN, 48, false),
        (i32::MAX, 48, false),
        (64, i32::MIN, false),
        (64, i32::MAX, false),
    ] {
        let file = v(w, h);
        let r = within(LIMIT, || formula::video(&file).map(|v| (v.div3, v.width, v.height)));
        let got = r.unwrap_or_else(|e| panic!("{w}x{h}: {e}"));
        assert_eq!(got.is_some_and(|g| g.0), ok, "{w}x{h}: {got:?}");
        if ok {
            roundtrip(&file).unwrap_or_else(|| panic!("{w}x{h}: no formula"));
        }
    }
}

#[test]
fn handlers_decide_what_is_div3() {
    for (h, div3) in [
        (*b"div3", true),
        (*b"DIV3", true),
        (*b"MP43", true),
        (*b"Div4", true),
        (*b"div5", true),
        (*b"DIV6", true),
        (*b"DIVX", false),
        (*b"xvid", false),
        (*b"mp42", false),
        (*b"\0\0\0\0", false),
    ] {
        let items: Vec<_> = (0..5).map(|i| chunk(b"00dc", &noise(i, 100))).collect();
        let file = avi(&[S::Video(h, 64, 48)], &[], &items, &[]);
        assert_eq!(formula::video(&file).unwrap().div3, div3, "{h:?}");
        // video of another handler alone is not taken (no audio to take)
        assert_eq!(formula::starts(&file), div3, "{h:?}");
    }
}

#[test]
fn frames_that_do_not_parse_are_kept_in_the_coded_stream() {
    for n in [4, 5, 31] {
        let file = noisy(n);
        let f = roundtrip(&file).expect("a formula");
        assert_eq!(f.streams.iter().filter(|s| &*s.class == formula::CODED).count(), 1);
    }
    // under 4 frames: none
    assert!(formula::encode_unchecked(&noisy(3), 1).is_none());
    assert!(formula::encode_unchecked(&noisy(0), 1).is_none());
    // noise does not come to under 97%
    assert!(formula::encode(&noisy(8), 1).is_none());
}

#[test]
fn a_frame_over_the_most_is_not_taken() {
    let mut items: Vec<_> = (0..4).map(|i| chunk(b"00dc", &noise(i, 100))).collect();
    let file = avi(&[S::Video(*b"div3", 64, 48)], &[], &items, &[]);
    assert!(formula::encode_unchecked(&file, 1).is_some());
    items.push(chunk(b"00dc", &vec![0u8; module::FRAME_MAX + 1]));
    let file = avi(&[S::Video(*b"div3", 64, 48)], &[], &items, &[]);
    assert!(formula::encode_unchecked(&file, 1).is_none());
    // one of exactly the most
    items.pop();
    items.push(chunk(b"00dc", &vec![0u8; module::FRAME_MAX]));
    let file = avi(&[S::Video(*b"div3", 64, 48)], &[], &items, &[]);
    assert!(roundtrip(&file).is_some());
}

#[test]
fn movi_in_a_header_string() {
    let items: Vec<_> = (0..6).map(|i| chunk(b"00dc", &noise(i, 120 + i))).collect();
    let plain = avi(&[S::Video(*b"div3", 64, 48)], &[], &items, &[]);
    let n = formula::video(&plain).unwrap().frames.len();
    assert_eq!(n, 6);
    // a stream's name saying movi and hdrl: the data list is still the list
    for name in [&b"movi"[..], b"a movie called movi", b"hdrl movi", b"LIST\0\0\0\0movi"] {
        let file = avi(&[S::Video(*b"div3", 64, 48)], name, &items, &[]);
        let v = formula::video(&file).unwrap_or_else(|| panic!("{name:?}: no video"));
        assert_eq!(v.frames.len(), n, "{name:?}");
        roundtrip(&file).unwrap_or_else(|| panic!("{name:?}: no formula"));
    }
}

#[test]
fn ix00_and_avix_end_the_walk() {
    let frame = |i| chunk(b"00dc", &noise(i, 150));
    let mut items: Vec<_> = (0..5).map(frame).collect();
    items.push(chunk(b"ix00", &[0; 32]));
    items.extend((5..9).map(frame));
    let file = avi(&[S::Video(*b"div3", 64, 48)], &[], &items, &[]);
    assert_eq!(formula::video(&file).unwrap().frames.len(), 5);
    roundtrip(&file).unwrap();
    // an AVIX riff after the first, with frames of its own
    let first = avi(&[S::Video(*b"div3", 64, 48)], &[], &(0..5).map(frame).collect::<Vec<_>>(), &[]);
    let mut second = b"AVIX".to_vec();
    second.extend(list(b"movi", &(5..9).map(frame).collect::<Vec<_>>().concat()));
    let mut file = first;
    file.extend(b"RIFF");
    file.extend((second.len() as u32).to_le_bytes());
    file.extend(second);
    assert_eq!(formula::video(&file).unwrap().frames.len(), 5);
    roundtrip(&file).unwrap();
}

#[test]
fn lists_junk_and_odd_sizes_inside_the_data_list() {
    let mut items = vec![chunk(b"JUNK", &[7; 13])];
    for i in 0..6 {
        items.push(chunk(b"00dc", &noise(i, 101 + 2 * i)));
        items.push(chunk(b"01wb", &noise(100 + i, 33)));
        if i == 2 {
            items.push(list(b"rec ", &[chunk(b"00dc", &noise(50, 77)), chunk(b"01wb", &noise(51, 9))].concat()));
        }
        if i == 4 {
            items.push(chunk(b"00db", &noise(52, 20)));
            items.push(chunk(b"JUNK", &[]));
        }
    }
    let streams = [S::Video(*b"div3", 64, 48), S::Audio(0x2000)];
    let file = avi(&streams, &[], &items, &chunk(b"idx1", &[1, 2, 3, 4, 5]));
    let v = formula::video(&file).unwrap();
    assert_eq!((v.frames.len(), v.audio.len()), (7, 7));
    roundtrip(&file).unwrap();
    // the audio first: the video's chunks are 01dc
    let swapped: Vec<Vec<u8>> = items
        .iter()
        .map(|c| match &c[..4] {
            b"00dc" => [&b"01dc"[..], &c[4..]].concat(),
            b"01wb" => [&b"00wb"[..], &c[4..]].concat(),
            _ => c.clone(),
        })
        .collect();
    let streams = [S::Audio(0x55), S::Video(*b"div3", 64, 48)];
    let file = avi(&streams, &[], &swapped, &[]);
    let v = formula::video(&file).unwrap();
    assert!(v.mp3 && v.div3);
    assert_eq!((v.frames.len(), v.audio.len()), (6, 6));
    roundtrip(&file).unwrap();
}

#[test]
fn audio_that_is_not_ac3_or_mp3_is_left_alone() {
    let mut items = Vec::new();
    for i in 0..6 {
        items.push(chunk(b"00dc", &noise(i, 100)));
        items.push(chunk(b"01wb", &noise(100 + i, 40)));
    }
    for tag in [0x1u16, 0x2000, 0x55, 0xffff] {
        let file = avi(&[S::Video(*b"div3", 64, 48), S::Audio(tag)], &[], &items, &[]);
        roundtrip(&file).unwrap();
    }
    // AC-3 or MP3 audio that is noise comes to no less: a formula only for the frames
    let file = avi(&[S::Video(*b"xvid", 64, 48), S::Audio(0x2000)], &[], &items, &[]);
    assert!(formula::starts(&file));
    assert!(formula::encode(&file, 1).is_none());
}
