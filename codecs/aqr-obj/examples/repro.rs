//! Reproduces a fuzz mismatch: FILE BLOCK FORCE_WHOLE(hex); prints where the output differs.
use aqr_formula::Apply;
use aqr_obj::codec::Options;
use aqr_obj::formula;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let src = std::fs::read(&a[0]).unwrap();
    let opt = Options {
        verify: false,
        block: a[1].parse().unwrap(),
        force_whole: u64::from_str_radix(&a[2], 16).unwrap(),
        threads: 1,
    };
    let Some(f) = formula::encode_with(&src, &opt) else { return println!("not coded") };
    let eval = |a: &Apply, inputs: &[&[u8]]| {
        formula::decode(&a.params, inputs, a.len, 1).map_err(|e| aqr_formula::Error::new(e.to_string()))
    };
    match f.eval(&eval) {
        Ok(d) => {
            let i = d.iter().zip(&src).position(|(x, y)| x != y).unwrap_or(d.len().min(src.len()));
            println!("lengths {} {}, first difference at {i}", d.len(), src.len());
            let s = i.saturating_sub(120);
            println!("orig: {:?}", String::from_utf8_lossy(&src[s..(i + 80).min(src.len())]));
            println!("got:  {:?}", String::from_utf8_lossy(&d[s..(i + 80).min(d.len())]));
        }
        Err(e) => println!("error {e}"),
    }
}
