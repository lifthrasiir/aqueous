//! aqr-wrt as a module of entry formulas (see aqr-formula): the text rebuilt from its coded
//! text, for the archive to compress, with the dictionary as the module's resource (FORMAT.md,
//! "The module"). Text of Hangul is coded by `wrt-recons-hangul` instead ([`hangul`]), its
//! syllables in two bytes and its words of Hangul by codes. The texts archived together share
//! dictionaries ([`Shared`]), so that text that holds another (a file and the same in an
//! archive, a file and its start) codes to what holds the other's code, and the archive stores
//! what they share once.

use aqr_formula::{Formula, Kind, Module, Node, Resource};

use crate::hangul;
use crate::words::Dictionary;

pub use crate::module::{decode, decode_hangul};

pub const MODULE: Module = Module::new("wrt-recons", 1);
pub const HANGUL: Module = Module::new("wrt-recons-hangul", 1);

/// The names of the inputs' streams, in order.
pub const STREAM_NAMES: [&str; 1] = ["text"];

/// Text shorter than this is not taken. Whether text holds other text archived with it is told
/// from this long on (aqr-formula's `Held`), and text that does is coded alike there.
pub const MIN_LEN: usize = 256 << 10;

/// Text is sampled in this many windows of [`WINDOW`] bytes, evenly spread, to tell whether
/// coding it pays ([`pays`]).
const WINDOWS: usize = 8;
const WINDOW: usize = 128 << 10;

/// Text is of Hangul ([`korean`]) where syllables are at least this share of the bytes of its
/// samples.
const KOREAN: f64 = 0.25;

/// The dictionaries of texts, and their resources: of words and markup, of the texts not of
/// Hangul ([`korean`]); of Hangul, of those of it.
#[derive(Debug, Clone)]
pub struct Shared {
    pub words: Option<(Dictionary, Resource)>,
    pub hangul: Option<(hangul::Dictionary, Resource)>,
}

impl Shared {
    /// The dictionaries of `datas` ([`Dictionary::of`], [`hangul::Dictionary::of`]); `None`
    /// where there are neither.
    pub fn of(datas: &[&[u8]], threads: usize) -> Option<Shared> {
        let (korean, other): (Vec<&[u8]>, Vec<&[u8]>) = datas.iter().partition(|d| korean(d));
        let words = (!other.is_empty()).then(|| Dictionary::of(&other, threads)).flatten();
        let hangul = (!korean.is_empty()).then(|| hangul::Dictionary::of(&korean, threads)).flatten();
        let shared = Shared {
            words: words.map(|d| {
                let r = Resource::new(d.to_bytes());
                (d, r)
            }),
            hangul: hangul.map(|d| {
                let r = Resource::new(d.to_bytes());
                (d, r)
            }),
        };
        (shared.words.is_some() || shared.hangul.is_some()).then_some(shared)
    }

    /// The resources of the dictionaries.
    pub fn resources(&self) -> impl Iterator<Item = &Resource> {
        self.words.iter().map(|w| &w.1).chain(self.hangul.iter().map(|h| &h.1))
    }
}

/// Whether `c` is a byte of text: printable, or a tab, a line's end or a form feed. Bytes from
/// 0x80 on are taken whatever the encoding (UTF-8, Latin-1).
fn text_byte(c: u8) -> bool {
    c >= 0x20 && c != 0x7f || matches!(c, b'\t' | b'\n' | b'\r' | 0x0c)
}

/// Whether all of `d` is text bytes.
pub fn is_text(d: &[u8]) -> bool {
    d.iter().all(|&c| text_byte(c))
}

/// Whether `d` is text but for at most 1 in 1000 of its bytes (escapes of a terminal, say).
pub fn mostly_text(d: &[u8]) -> bool {
    d.iter().filter(|&&c| !text_byte(c)).count() <= d.len() / 1000
}

/// Whether `d` is a file this crate takes: of at least [`MIN_LEN`] bytes, all of them text
/// bytes (a file of other bytes as well is looked into for files in it).
pub fn starts(d: &[u8]) -> bool {
    d.len() >= MIN_LEN && is_text(d)
}

/// Whether coding text `d` with `dict` pays, told by `d` and `dict` alone (so that the same
/// text is coded alike wherever it is, and its copies share their code): zstd -3 compresses
/// samples of it ([`WINDOWS`]) coded at least 1/32 smaller than as they are, by more than
/// `own` bytes over all of `d` (the dictionary compressed, where it is `d`'s own).
fn pays(d: &[u8], code: impl Fn(&[u8]) -> Vec<u8>, own: usize) -> bool {
    let z = |s: &[u8]| zstd::bulk::compress(s, 3).map_or(s.len(), |c| c.len());
    let (mut raw, mut coded, mut n) = (0, 0, 0);
    for s in samples(d) {
        (raw, coded, n) = (raw + z(s), coded + z(&code(s)), n + s.len());
    }
    coded * 32 <= raw * 31 && ((raw - coded) as f64 * d.len() as f64 / n as f64) > own as f64
}

/// The samples of `d` that [`pays`] and [`korean`] go by: [`WINDOWS`] of [`WINDOW`] bytes,
/// evenly spread, or all of `d` where it is shorter.
fn samples(d: &[u8]) -> Vec<&[u8]> {
    match d.len() <= WINDOWS * WINDOW {
        true => vec![d],
        false => (0..WINDOWS).map(|i| &d[i * (d.len() - WINDOW) / (WINDOWS - 1)..][..WINDOW]).collect(),
    }
}

/// Whether `d` is text of Hangul, coded by [`HANGUL`]: its syllables are at least [`KOREAN`]
/// of the bytes of its samples ([`samples`]).
pub fn korean(d: &[u8]) -> bool {
    let (mut n, mut of) = (0, 0);
    for s in samples(d) {
        (n, of) = (n + hangul::syllable_bytes(s), of + s.len());
    }
    of > 0 && n as f64 >= KOREAN * of as f64
}

/// The formula of text `input` with dictionaries of its own, or `None` where it is shorter than
/// [`MIN_LEN`], is not [`mostly_text`], has no words or markup often enough, or coding it does
/// not pay ([`pays`]). Text of Hangul ([`korean`]) is coded by [`HANGUL`], else by [`MODULE`].
/// It is the same whatever `threads` is.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    if input.len() < MIN_LEN || !mostly_text(input) {
        return None;
    }
    let shared = Shared::of(&[input], threads)?;
    let own = shared.resources().map(|r| zstd::bulk::compress(r, 3).map_or(r.len(), |c| c.len())).sum();
    coded(input, &shared, own, threads)
}

/// The formula of text `input` with dictionaries `shared`, or `None` where it is shorter than
/// [`MIN_LEN`], is not [`mostly_text`], `shared` has no dictionary of its kind ([`korean`]) or
/// coding it does not pay ([`pays`]). It is the same whatever `threads` is.
pub fn encode_with(input: &[u8], shared: &Shared, threads: usize) -> Option<Formula> {
    if input.len() < MIN_LEN || !mostly_text(input) {
        return None;
    }
    coded(input, shared, 0, threads)
}

fn coded(input: &[u8], shared: &Shared, own: usize, threads: usize) -> Option<Formula> {
    let mut f = Formula::default();
    let len = input.len() as u64;
    if korean(input) {
        let (dict, resource) = shared.hangul.as_ref()?;
        if !pays(input, |s| dict.encode_piece(s), own) {
            return None;
        }
        let (text, pieces) = dict.encode(input, threads);
        let inputs = vec![f.push_as("wrt/hangul", Kind::Text, text)];
        f.root = Node::apply_with(HANGUL, vec![resource.clone()], hangul::params(&pieces), inputs, len);
    } else {
        let (dict, resource) = shared.words.as_ref()?;
        if !pays(input, |s| dict.encode(s, 1), own) {
            return None;
        }
        let inputs = vec![f.push_as("wrt/text", Kind::Text, dict.encode(input, threads))];
        f.root = Node::apply_with(MODULE, vec![resource.clone()], Vec::new(), inputs, len);
    }
    Some(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Result;

    fn eval(f: &Formula, len: usize) -> Result<Vec<u8>> {
        let Node::Apply(a) = &f.root else { panic!() };
        let resources: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
        let inputs: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
        match a.module {
            m if m == HANGUL => decode_hangul(&a.params, &resources, &inputs, len as u64, 2),
            _ => decode(&a.params, &resources, &inputs, len as u64, 1),
        }
    }

    #[test]
    fn module() {
        // words in no order zstd would find
        let mut rng = aqr_common::rng::Rng::new(3);
        let words =
            ["The", "quick", "brown", "fox", "jumps", "over", "the", "lazy", "dog;", "<b>THE</b>", "&amp;", "dog.\n"];
        let mut text = Vec::new();
        while text.len() < 300_000 {
            text.extend_from_slice(words[rng.below(words.len() as u64) as usize].as_bytes());
            text.push(b' ');
        }
        let d = &text[..];
        assert!(starts(d) && !starts(&d[..MIN_LEN - 1]));
        assert!(!starts(&[d, b"\0"].concat()));
        let f = encode(d, 1).unwrap();
        assert_eq!(eval(&f, d.len()).unwrap(), d);
        assert!(eval(&f, d.len() - 1).is_err());
        assert!(encode(&d[..MIN_LEN - 1], 1).is_none());
        assert!(encode(&vec![b'.'; MIN_LEN], 1).is_none());
        // a few bytes of others, but not many
        let mut odd = d.to_vec();
        (0..odd.len() / 1000).for_each(|i| odd[i * 1000] = 0x1b);
        assert!(!starts(&odd) && encode(&odd, 1).is_some());
        odd[1] = 0;
        assert!(encode(&odd, 1).is_none());
        // with a dictionary of other text; not where it does not pay
        let shared = Shared::of(&[d], 1).unwrap();
        let other = &d[1000..MIN_LEN + 1000];
        let f = encode_with(other, &shared, 1).unwrap();
        assert_eq!(eval(&f, other.len()).unwrap(), other);
        assert!(encode_with(&d[..MIN_LEN - 1], &shared, 1).is_none());
        let noise: Vec<u8> = aqr_common::rng::Rng::new(1).bytes(MIN_LEN).iter().map(|b| b'a' + b % 26).collect();
        assert!(encode_with(&noise, &shared, 1).is_none());
    }

    /// Text in Korean, `n` bytes of words of Hangul at random, with a few others.
    fn korean_text(n: usize) -> Vec<u8> {
        let mut rng = aqr_common::rng::Rng::new(9);
        let words = ["사람은", "나는", "한국어로", "글을", "쓴다.", "그리고", "(English)", "읽는다,", "말이다.\n"];
        let mut text = Vec::new();
        while text.len() < n {
            text.extend_from_slice(words[rng.below(words.len() as u64) as usize].as_bytes());
            text.push(b' ');
        }
        text
    }

    #[test]
    fn hangul() {
        let d = &korean_text(400_000)[..];
        assert!(korean(d) && starts(d));
        let f = encode(d, 2).unwrap();
        let Node::Apply(a) = &f.root else { panic!() };
        assert_eq!(a.module, HANGUL);
        assert_eq!(eval(&f, d.len()).unwrap(), d);
        assert!(eval(&f, d.len() - 1).is_err());
        // a little Hangul in other text is coded as words
        let mut other = Vec::new();
        while other.len() < 400_000 {
            other.extend_from_slice("the quick brown fox jumps over the lazy dog 한 ".as_bytes());
        }
        assert!(!korean(&other));
        let Node::Apply(a) = &encode(&other, 1).unwrap().root else { panic!() };
        assert_eq!(a.module, MODULE);
        // dictionaries shared by both: each text by its own kind, none where there is none
        let shared = Shared::of(&[d, &other], 1).unwrap();
        assert_eq!(shared.resources().count(), 2);
        for t in [&d[1000..MIN_LEN + 1000], &other[..MIN_LEN]] {
            let f = encode_with(t, &shared, 1).unwrap();
            assert_eq!(eval(&f, t.len()).unwrap(), t);
        }
        let words = Shared::of(&[&other], 1).unwrap();
        assert!(words.hangul.is_none() && encode_with(d, &words, 1).is_none());
    }
}
