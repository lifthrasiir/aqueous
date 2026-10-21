//! FlateDecode images by predictor, bits per component and colors: count and bytes.
use aqr_pdf::lex::Obj;
use std::collections::BTreeMap;
fn main() {
    let mut by: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for f in std::env::args().skip(1) {
        let buf = std::fs::read(&f).unwrap();
        for s in aqr_pdf::scan::streams(&buf) {
            if aqr_pdf::kind::filter(&s.dict) != aqr_pdf::kind::Filter::Flate
                || aqr_pdf::kind::class(&s.dict) != aqr_pdf::kind::Class::Image
            {
                continue;
            }
            let d = &s.dict;
            let parms = d.get(b"DecodeParms").map(|p| match p {
                Obj::Array(a) => a.first().cloned().unwrap_or(Obj::Null),
                p => p.clone(),
            });
            let pred = parms.as_ref().and_then(|p| p.get(b"Predictor")).and_then(Obj::as_int).unwrap_or(1);
            let colors = parms.as_ref().and_then(|p| p.get(b"Colors")).and_then(Obj::as_int);
            let bpc = d.get(b"BitsPerComponent").and_then(Obj::as_int).unwrap_or(0);
            let cs = match d.get(b"ColorSpace") {
                Some(Obj::Name(n)) => String::from_utf8_lossy(n).into_owned(),
                Some(Obj::Array(a)) => format!(
                    "[{}]",
                    a.first()
                        .and_then(Obj::as_name)
                        .map(|n| String::from_utf8_lossy(n).into_owned())
                        .unwrap_or_default()
                ),
                Some(Obj::Ref(..)) => "ref".into(),
                None => {
                    if d.get(b"ImageMask").is_some() {
                        "mask".into()
                    } else {
                        "none".into()
                    }
                }
                _ => "?".into(),
            };
            let filters = d.get(b"Filter").map(|f| f.names().len()).unwrap_or(0);
            let e = by
                .entry(format!("pred={pred:<2} bpc={bpc:<2} cs={cs:<12} colors={colors:?} nfilt={filters}"))
                .or_default();
            e.0 += 1;
            e.1 += s.data.len();
        }
    }
    let mut v: Vec<_> = by.into_iter().collect();
    v.sort_by_key(|x| std::cmp::Reverse(x.1.1));
    for (k, (n, b)) in v.iter().take(30) {
        println!("{n:>6} {b:>11}  {k}");
    }
}
