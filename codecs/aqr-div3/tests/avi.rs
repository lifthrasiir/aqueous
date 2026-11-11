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

fn avi(w: u32, h: u32, frames: &[&[u8]], sound: &[u8]) -> Vec<u8> {
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
    hdrl.extend(list(b"strl", &[chunk(b"strh", &strh2), chunk(b"strf", &[0; 18])].concat()));
    let mut movi = Vec::new();
    for f in frames {
        movi.extend(chunk(b"01wb", sound));
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
    let sound: Vec<u8> = (0..2001u32).map(|i| (i * 7) as u8).collect();
    let file = avi(v.width as u32, v.height as u32, &frames, &sound);
    let f = formula::encode(&file, 1).expect("a formula");
    let none = |a: &aqr_formula::Apply, i: &[&[u8]]| -> aqr_formula::Result<Vec<u8>> {
        assert_eq!(a.module.name, "div3-recons");
        formula::decode(&a.params, i, a.len, 1).map_err(|e| aqr_formula::Error(e.to_string()))
    };
    let back = f.eval(&none).unwrap();
    assert!(back == file);
    let coded = f.streams[0].data.len();
    let total: usize = frames.iter().map(|f| f.len()).sum();
    println!("{} frames: {} -> {} bytes ({:.1}%)", frames.len(), total, coded, 100.0 * coded as f64 / total as f64);
}
