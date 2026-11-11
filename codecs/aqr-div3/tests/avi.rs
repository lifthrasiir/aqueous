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
    let mut strh = b"vidsdiv3".to_vec();
    strh.extend_from_slice(&[0; 48]);
    let mut strf = Vec::new();
    for v in [40u32, w, h, 0x0018_0001, u32::from_le_bytes(*b"DIV3")] {
        strf.extend_from_slice(&v.to_le_bytes());
    }
    strf.extend_from_slice(&[0; 20]);
    let mut strh2 = b"audsac3 ".to_vec();
    strh2.extend_from_slice(&[0; 48]);
    let mut hdrl = chunk(b"avih", &[0; 56]);
    hdrl.extend(list(b"strl", &[chunk(b"strh", &strh), chunk(b"strf", &strf)].concat()));
    hdrl.extend(list(b"strl", &[chunk(b"strh", &strh2), chunk(b"strf", &[0x00, 0x20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])].concat()));
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
            "ac3-recons" => aqr_ac3::module::decode(&a.params, i, a.len, 1).map_err(|e| aqr_formula::Error(e.to_string())),
            m => panic!("{m}"),
        }
    };
    let back = f.eval(&none).unwrap();
    assert!(back == file);
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
