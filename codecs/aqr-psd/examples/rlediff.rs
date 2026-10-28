// Shows rows the identified PackBits encoder does not reproduce: first differing tokens.
use aqr_psd::packbits;
use aqr_psd::pixels::unpackbits;
use aqr_psd::psd::Compression;

fn tokens(src: &[u8]) -> Vec<String> {
    let mut t = 0;
    let mut v = Vec::new();
    while t + 1 < src.len() {
        let h = src[t] as i8;
        if h >= 0 {
            v.push(format!("L{}", h as i32 + 1));
            t += 2 + h as usize;
        } else {
            v.push(format!("R{}", 1 - h as i32));
            t += 2;
        }
    }
    v
}

fn main() {
    let f = std::env::args().nth(1).unwrap();
    let limit: usize = std::env::args().nth(2).map_or(10, |v| v.parse().unwrap());
    let d = std::fs::read(&f).unwrap();
    let p = aqr_psd::parse(&d).unwrap();
    let cb = if p.psb { 4 } else { 2 };
    let mut shown = 0;
    for (li, l) in p.layer_info.layers.iter().enumerate() {
        for c in &l.channels {
            if c.compression(&d) != Some(Compression::Rle) {
                continue;
            }
            let rows = packbits::rows(&d[c.span.start + 2..c.span.end], c.height as usize, cb).unwrap();
            let (enc, _) = packbits::identify(&rows);
            for (ri, r) in rows.iter().enumerate() {
                let mut row = Vec::new();
                unpackbits(r, &mut row);
                let mut e = Vec::new();
                packbits::encode(enc, &row, &mut e);
                if e != *r && shown < limit {
                    shown += 1;
                    let (a, b) = (tokens(r), tokens(&e));
                    let k = a.iter().zip(&b).position(|(x, y)| x != y).unwrap_or(0);
                    let off: usize = a[..k].iter().map(|t| t[1..].parse::<usize>().unwrap()).sum();
                    let lo = k.saturating_sub(3);
                    println!(
                        "L{li} ch {} row {ri} len {} {enc:?} at {off}: {:?}\n  orig {}\n  enc  {}",
                        c.id,
                        row.len(),
                        &row[off.saturating_sub(3)..(off + 10).min(row.len())],
                        a[lo..(k + 4).min(a.len())].join(" "),
                        b[lo..(k + 4).min(b.len())].join(" ")
                    );
                }
            }
        }
    }
}
