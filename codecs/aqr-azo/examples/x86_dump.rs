//! Dumps the x86-flagged stream of an EGG archive as its blocks decode (the filter's output):
//! `FILE.egg OUT.bin`. A scratch tool of the search for the x86 filter.
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let d = std::fs::read(&a[0]).unwrap();
    let k = d.windows(4).position(|w| w == 0x02B5_0C13u32.to_le_bytes()).unwrap();
    let s = &d[k + 4 + 18..];
    let st = aqr_azo::azo::parse(s).unwrap();
    let mut out = Vec::new();
    for b in &st.blocks {
        let c = &s[b.code.clone()];
        out.extend(if b.stored() {
            c.to_vec()
        } else {
            aqr_azo::azo::decode_block(c, b.size, &mut Default::default()).unwrap().data
        });
    }
    std::fs::write(&a[1], out).unwrap();
}
