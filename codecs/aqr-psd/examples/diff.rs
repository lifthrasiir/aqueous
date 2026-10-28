// Prints a sample of composite pixels where the rendering differs (debugging aid).
use aqr_psd::{composite, pixels};
fn main() {
    let f = std::env::args().nth(1).unwrap();
    let d = std::fs::read(&f).unwrap();
    let p = aqr_psd::parse(&d).unwrap();
    let comp = pixels::composite(&p, &d).unwrap();
    let n = (p.width * p.height) as usize;
    let wa = p.channels >= 4 && p.layer_info.merged_alpha;
    let nc = if wa { 4 } else { 3 };
    println!("channels {} merged_alpha {} layers {}", p.channels, p.layer_info.merged_alpha, p.layer_info.layers.len());
    for (i, l) in p.layer_info.layers.iter().enumerate() {
        let ids: Vec<i16> = l.channels.iter().map(|c| c.id).collect();
        println!(
            "L{i} {:?} {} op {} clip {} flags {:x} ch {:?} mask {:?} tags {:?}",
            String::from_utf8_lossy(&l.name),
            String::from_utf8_lossy(&l.blend),
            l.opacity,
            l.clipping,
            l.flags,
            ids,
            l.mask,
            l.tags.iter().map(|t| String::from_utf8_lossy(&t.key).into_owned()).collect::<Vec<_>>()
        );
    }
    let r = composite::render(&p, &d, wa, &mut Default::default()).unwrap();
    let mut shown = 0;
    let mut hist = std::collections::BTreeMap::new();
    for i in 0..n {
        let c: Vec<u8> = (0..p.channels as usize).map(|k| comp[k * n + i]).collect();
        let q = &r[i * nc..i * nc + nc];
        if c[..nc] != *q {
            *hist.entry((c.get(3).copied(), q.get(3).copied())).or_insert(0) += 1;
            if shown < 12 && i % 7 == 0 {
                println!("({},{}) stored {:?} rendered {:?}", i % p.width as usize, i / p.width as usize, c, q);
                shown += 1;
            }
        }
    }
    let mut v: Vec<_> = hist.into_iter().collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.1));
    println!("by (stored alpha, rendered alpha): {:?}", &v[..v.len().min(10)]);
}
