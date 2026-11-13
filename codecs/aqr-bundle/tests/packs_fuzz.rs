use aqr_bundle::packs::*;

#[test]
fn garbage_does_not_panic() {
    let mut x = 0x9e3779b97f4a7c15u64;
    let mut rnd = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let heads: [&[u8]; 9] = [
        b"BSA\0\x67\0\0\0",
        b"BSA\0\x69\0\0\0",
        b"\0\x01\0\0",
        b"BTDX\x01\0\0\0GNRL",
        b"BTDX\x03\0\0\0DX10",
        b"BIGF",
        b"RPA-3.0 0000000000000020 00000001\n",
        b"XP3\r\n \n\x1a\x8b\x67\x01",
        b"PSARxxxxzlib",
    ];
    for _ in 0..20000 {
        let h = heads[(rnd() % 9) as usize];
        let mut d = h.to_vec();
        let n = rnd() % 300;
        for _ in 0..n {
            let r = rnd();
            d.push(if r % 3 == 0 {
                0
            } else if r % 3 == 1 {
                (r >> 8) as u8 % 4
            } else {
                (r >> 8) as u8
            });
        }
        let _ = bethesda_bsa::files(&d);
        let _ = bethesda_ba2::files(&d);
        let _ = ea_big::files(&d);
        let _ = kirikiri_xp3::files(&d);
        let _ = mtf_arc::files(&d);
        let _ = renpy_rpa::files(&d);
        let _ = sony_psarc::files(&d);
        let mut m = b"ARC\0\x07\0".to_vec();
        m.extend_from_slice(&d);
        let _ = mtf_arc::files(&m);
        let mut p = b"PSAR\0\x01\0\x04zlib".to_vec();
        p.extend_from_slice(&d);
        let _ = sony_psarc::files(&p);
    }
}
