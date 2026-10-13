// Prints the channels of each layer: id, size, span, compression, first bytes (debugging aid).
fn main() {
    let f = std::env::args().nth(1).unwrap();
    let d = std::fs::read(&f).unwrap();
    let p = psd_recons::parse(&d).unwrap();
    println!("{}x{}x{} depth {} mode {} composite {:?}", p.width, p.height, p.channels, p.depth, p.mode, p.composite);
    for (i, l) in p.layer_info.layers.iter().enumerate() {
        for c in &l.channels {
            let head = &d[c.span.start..c.span.end.min(c.span.start + 12)];
            println!("L{i} ch {} {}x{} {:?} {:?} {:02x?}", c.id, c.width, c.height, c.span, c.compression(&d), head);
        }
    }
}
