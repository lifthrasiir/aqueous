//! XML taken by aqr-xml where it pays (a test binary of its own: it sets the process's `-Z`, a
//! segment of 64 KiB so that a text of a few segments is not 48 MiB).

use aqr_archive::Evaluate;
use aqr_common::rng::Rng;
use aqr_compress::{Formula, Options, recompress};
use aqr_decompress::{LOG, Native, XML, decompress};

fn modules(f: &Formula) -> Vec<&'static str> {
    f.applies().iter().map(|a| a.module.name).collect()
}

/// Checks that formula `f` evaluates back to `d`, also through the archive (natively and as
/// WebAssembly), and returns its archive's bytes.
fn check(d: &[u8], f: &Formula) -> usize {
    assert_eq!(f.len(), d.len() as u64);
    assert!(f.eval(&Native::default()).unwrap() == d, "formula does not evaluate to the input:\n{f}");
    let enc = aqr_archive::Encoder::new(aqr_archive::Level::MID).with_modules(aqr_decompress::wasm_of);
    let c = enc.encode(f, Some(aqr_archive::checksum(d))).unwrap();
    assert!(decompress(&c, 1).unwrap() == d, "archive does not decompress to the input:\n{f}");
    let w = aqr_archive::decode(&c, &aqr_decompress::Wasm);
    assert!(w.as_ref().is_ok_and(|w| w == d), "archive does not decompress as WebAssembly: {:?}", w.err());
    c.len()
}

fn roundtrip(d: &[u8]) -> Formula {
    static SEGMENTS: std::sync::Once = std::sync::Once::new();
    SEGMENTS.call_once(|| drop(aqr_common::zopt::apply(&["xml:segment=64K"]).unwrap()));
    let f = recompress(d, &Options::default());
    check(d, &f);
    f
}

const WORDS: [&str; 24] = [
    "the", "of", "river", "stone", "market", "garden", "quiet", "over", "under", "bridge", "winter", "letter", "after",
    "song", "iron", "paper", "light", "north", "village", "story", "ocean", "morning", "copper", "window",
];

fn sentence(rng: &mut Rng, n: u64) -> String {
    (0..n).map(|_| WORDS[rng.below(WORDS.len() as u64) as usize]).collect::<Vec<_>>().join(" ")
}

/// Records of elements of text and of a few fields, as a bibliography is.
fn elements(n: usize) -> Vec<u8> {
    let mut rng = Rng::new(11);
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<library>\n");
    for i in 0..n {
        s += &format!(
            "  <book id=\"b{i}\" lang=\"{}\">\n    <title>{}</title>\n    <author>{}</author>\n    <year>{}</year>\n    <abstract>{}</abstract>\n  </book>\n",
            ["en", "ko", "fr"][i % 3],
            sentence(&mut rng, 4),
            sentence(&mut rng, 2),
            1900 + rng.below(120),
            sentence(&mut rng, 30)
        );
    }
    s += "</library>\n";
    s.into_bytes()
}

/// Rows of numbers as attributes, one to a line (Stack Exchange's dumps).
fn rows(n: usize) -> Vec<u8> {
    let mut rng = Rng::new(5);
    let mut s = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<votes>\n");
    for i in 0..n {
        s += &format!(
            "  <row Id=\"{i}\" PostId=\"{}\" VoteTypeId=\"{}\" CreationDate=\"2020-{:02}-{:02}T00:00:00.000\" />\n",
            1000 + rng.below(50) + i as u64 / 3,
            2 + rng.below(3),
            1 + i / 3000 % 12,
            1 + i / 100 % 28
        );
    }
    s += "</votes>\n";
    s.into_bytes()
}

fn html(n: usize) -> Vec<u8> {
    let mut rng = Rng::new(3);
    let mut s = String::from("<!DOCTYPE html>\n<html><head><title>t</title></head><body>\n");
    for _ in 0..n {
        s += &format!("<p>{}<br>{}</p>\n", sentence(&mut rng, 12), sentence(&mut rng, 9));
    }
    s += "</body></html>\n";
    s.into_bytes()
}

#[test]
fn elements_of_text_are_xml() {
    let d = elements(1500);
    assert!(d.len() > 300_000);
    let f = roundtrip(&d);
    assert!(modules(&f).contains(&XML.name), "{:?}", modules(&f));
}

#[test]
fn rows_of_attributes_go_where_they_are_smaller() {
    let d = rows(12_000);
    assert!(d.len() > 1_000_000);
    let f = roundtrip(&d);
    assert!(modules(&f).contains(&LOG.name) && !modules(&f).contains(&XML.name), "{:?}", modules(&f));
}

#[test]
fn html_is_not_xml() {
    let d = html(4000);
    assert!(d.len() > 100_000);
    let f = roundtrip(&d);
    assert!(!modules(&f).contains(&XML.name), "{:?}", modules(&f));
    // (nor as XHTML, whose void elements are closed or not alike)
    let x = [&b"<?xml version=\"1.0\"?>\n"[..], &d[..]].concat();
    assert!(!modules(&roundtrip(&x)).contains(&XML.name));
}

#[test]
fn small_xml_is_not_made_larger() {
    // (under the least length: not taken; and what is just over it archives no larger than
    // as raw text: either the formula is smaller or it is not kept)
    let d = elements(100);
    assert!(d.len() < aqr_xml::formula::MIN_LEN);
    let f = roundtrip(&d);
    assert!(!modules(&f).contains(&XML.name));
    for n in [150, 250, 400] {
        let d = elements(n);
        let f = roundtrip(&d);
        let plain = recompress(&d, &Options { depth: 0, ..Options::default() });
        assert!(check(&d, &f) <= check(&d, &plain), "{n}: {:?}", modules(&f));
    }
}

#[test]
fn segments_round_trip() {
    let d = elements(4000);
    assert!(d.len() > 1_000_000);
    let f = roundtrip(&d);
    let xml = modules(&f).iter().filter(|&&m| m == XML.name).count();
    assert!(xml >= 8, "{:?}", modules(&f));
}
