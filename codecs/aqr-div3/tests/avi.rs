//! The whole way on an AVI made of real frames: the formula, evaluated by the module, gives the file back.
//! The frames come from a corpus file named by AQR_DIV3_AVI (measurements only: the test is skipped without it).
use aqr_div3::formula;

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

fn avi(w: u32, h: u32, frames: &[&[u8]], sound: &[&[u8]]) -> Vec<u8> {
    avi_of(b"div3", 0x2000, w, h, frames, sound)
}

/// An AVI of video `handler` and audio of format `tag`, the audio chunks before the frames in turn.
fn avi_of(handler: &[u8; 4], tag: u16, w: u32, h: u32, frames: &[&[u8]], sound: &[&[u8]]) -> Vec<u8> {
    let mut strh = b"vids".to_vec();
    strh.extend_from_slice(handler);
    strh.extend_from_slice(&[0; 48]);
    let mut strf = Vec::new();
    for v in [40u32, w, h, 0x0018_0001, u32::from_le_bytes(*b"DIV3")] {
        strf.extend_from_slice(&v.to_le_bytes());
    }
    strf.extend_from_slice(&[0; 20]);
    let mut strh2 = b"auds\0\0\0\0".to_vec();
    strh2.extend_from_slice(&[0; 48]);
    let mut hdrl = chunk(b"avih", &[0; 56]);
    hdrl.extend(list(b"strl", &[chunk(b"strh", &strh), chunk(b"strf", &strf)].concat()));
    hdrl.extend(list(
        b"strl",
        &[chunk(b"strh", &strh2), chunk(b"strf", &[&tag.to_le_bytes()[..], &[0; 16]].concat())].concat(),
    ));
    let mut movi = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        movi.extend(chunk(b"01wb", sound[i % sound.len()]));
        movi.extend(chunk(b"00dc", f));
    }
    let mut body = b"AVI ".to_vec();
    body.extend(list(b"hdrl", &hdrl));
    body.extend(list(b"movi", &movi));
    body.extend(chunk(b"idx1", &[1, 2, 3, 4, 5]));
    let mut v = b"RIFF".to_vec();
    v.extend_from_slice(&(body.len() as u32).to_le_bytes());
    v.extend(body);
    v
}

#[test]
fn roundtrip_real_frames() {
    let Ok(path) = std::env::var("AQR_DIV3_AVI") else { return };
    let src = std::fs::read(path).unwrap();
    let v = formula::video(&src).expect("a DivX 3 AVI");
    let frames: Vec<&[u8]> = v.frames.iter().take(300).map(|r| &src[r.clone()]).collect();
    // the audio of the source: its chunks cut across the AC-3 frames, with the zeros before the first one
    let sound: Vec<&[u8]> = v.audio.iter().take(300).map(|r| &src[r.clone()]).collect();
    let file = avi(v.width as u32, v.height as u32, &frames, &sound);
    let f = formula::encode(&file, 1).expect("a formula");
    let none = |a: &aqr_formula::Apply, i: &[&[u8]]| -> aqr_formula::Result<Vec<u8>> {
        match a.module.name {
            "div3-recons" => formula::decode(&a.params, i, a.len, 1).map_err(|e| aqr_formula::Error(e.to_string())),
            "ac3-recons" => {
                aqr_ac3::formula::decode(&a.params, i, a.len, 2).map_err(|e| aqr_formula::Error(e.to_string()))
            }
            m => panic!("{m}"),
        }
    };
    let back = f.eval(&none).unwrap();
    assert!(back == file);
    let serial = |a: &aqr_formula::Apply, i: &[&[u8]]| -> aqr_formula::Result<Vec<u8>> {
        match a.module.name {
            "div3-recons" => {
                aqr_div3::module::decode_serial(&a.params, i, a.len).map_err(|e| aqr_formula::Error(e.to_string()))
            }
            "ac3-recons" => {
                aqr_ac3::module::decode_serial(&a.params, i, a.len, 1).map_err(|e| aqr_formula::Error(e.to_string()))
            }
            m => panic!("{m}"),
        }
    };
    assert!(f.eval(&serial).unwrap() == file, "serial decoding differs");
    let total: usize = frames.iter().map(|f| f.len()).sum();
    let audio: usize = sound.iter().map(|s| s.len()).sum::<usize>();
    let names: Vec<String> = f.streams.iter().map(|s| s.class.to_string()).collect();
    let size = |c: &str| f.streams.iter().find(|s| &*s.class == c).map_or(0, |s| s.data.len());
    println!("{names:?}");
    println!(
        "{} frames: {} -> {} bytes; audio {} -> {} bytes",
        frames.len(),
        total,
        size("div3/frames"),
        audio,
        size("ac3/frames") + size("div3/audio")
    );
}

/// An AVI whose video is not DivX 3 (noise as XviD frames, kept as they are) and whose audio is MP3 (a fixture of
/// aqr-mp3 cut into chunks across its frames): taken for the audio alone, rebuilt by the module from the kept frames
/// and mp3-recons's output, the audio coded smaller.
#[test]
fn kept_video_mp3_audio() {
    let mp3 = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../aqr-mp3/tests/data/cbr128.mp3")).unwrap();
    let noise: Vec<Vec<u8>> = (0..40).map(|i| aqr_common::rng::noise(500 + 37 * i)).collect();
    let frames: Vec<&[u8]> = noise.iter().map(|f| &f[..]).collect();
    let sound: Vec<&[u8]> = mp3.chunks(mp3.len().div_ceil(frames.len())).collect();
    let file = avi_of(b"XVID", 0x55, 320, 240, &frames, &sound);
    let f = formula::encode(&file, 2).expect("a formula for the audio");
    let modules = |a: &aqr_formula::Apply, i: &[&[u8]], serial: bool| -> aqr_formula::Result<Vec<u8>> {
        let err = |e: &dyn std::fmt::Display| aqr_formula::Error(e.to_string());
        match a.module.name {
            "div3-recons" if serial => aqr_div3::module::decode_serial(&a.params, i, a.len).map_err(|e| err(&e)),
            "div3-recons" => formula::decode(&a.params, i, a.len, 2).map_err(|e| err(&e)),
            "mp3-recons" => {
                let r: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
                aqr_mp3::formula::decode(&a.params, &r, i, a.len, 2).map_err(|e| err(&e))
            }
            m => panic!("{m}"),
        }
    };
    assert!(f.eval(&|a: &aqr_formula::Apply, i: &[&[u8]]| modules(a, i, false)).unwrap() == file);
    assert!(f.eval(&|a: &aqr_formula::Apply, i: &[&[u8]]| modules(a, i, true)).unwrap() == file, "serial differs");
    let size = |c: &str| f.streams.iter().filter(|s| s.class.starts_with(c)).map(|s| s.data.len()).sum::<usize>();
    assert_eq!(size("div3/kept"), noise.iter().map(Vec::len).sum::<usize>());
    assert_eq!(size("div3/frames") + size("div3/audio"), 0);
    assert!(size("mp3/") < mp3.len(), "{} -> {}", mp3.len(), size("mp3/"));
}
