//! aqr-wrt as a module of entry formulas (see aqr-formula): the text rebuilt from its coded
//! text, for the archive to compress, with the dictionary as the module's resource (FORMAT.md,
//! "The module"). The texts archived together share one dictionary ([`Shared`]), so that text
//! that holds another (a file and the same in an archive, a file and its start) codes to what
//! holds the other's code, and the archive stores what they share once.

use aqr_formula::{Formula, Kind, Module, Node, Resource};

use crate::words::{self, Dictionary};
use crate::{Result, corrupt};

pub const MODULE: Module = Module { name: "wrt-recons", version: 1 };

/// The names of the inputs' streams, in order.
pub const STREAM_NAMES: [&str; 1] = ["text"];

/// Text shorter than this is not taken. Whether text holds other text archived with it is told
/// from this long on (aqr-formula's `Held`), and text that does is coded alike there.
pub const MIN_LEN: usize = 256 << 10;

/// Text is sampled in this many windows of [`WINDOW`] bytes, evenly spread, to tell whether
/// coding it pays ([`pays`]).
const WINDOWS: usize = 8;
const WINDOW: usize = 128 << 10;

/// A dictionary of texts, and its resource.
#[derive(Debug, Clone)]
pub struct Shared {
    pub dict: Dictionary,
    pub resource: Resource,
}

impl Shared {
    /// The dictionary of `datas` ([`Dictionary::of`]).
    pub fn of(datas: &[&[u8]], threads: usize) -> Option<Shared> {
        let dict = Dictionary::of(datas, threads)?;
        let resource = Resource::new(dict.to_bytes());
        Some(Shared { dict, resource })
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
fn pays(d: &[u8], dict: &Dictionary, own: usize) -> bool {
    let samples: Vec<&[u8]> = match d.len() <= WINDOWS * WINDOW {
        true => vec![d],
        false => (0..WINDOWS).map(|i| &d[i * (d.len() - WINDOW) / (WINDOWS - 1)..][..WINDOW]).collect(),
    };
    let z = |s: &[u8]| zstd::bulk::compress(s, 3).map_or(s.len(), |c| c.len());
    let (mut raw, mut coded, mut n) = (0, 0, 0);
    for s in samples {
        (raw, coded, n) = (raw + z(s), coded + z(&dict.encode(s, 1)), n + s.len());
    }
    coded * 32 <= raw * 31 && ((raw - coded) as f64 * d.len() as f64 / n as f64) > own as f64
}

/// The formula of text `input` with a dictionary of its own, or `None` where it is shorter than
/// [`MIN_LEN`], is not [`mostly_text`], has no words or markup often enough, or coding it does
/// not pay ([`pays`]). It is the same whatever `threads` is.
pub fn encode(input: &[u8], threads: usize) -> Option<Formula> {
    if input.len() < MIN_LEN || !mostly_text(input) {
        return None;
    }
    let shared = Shared::of(&[input], threads)?;
    let own = zstd::bulk::compress(&shared.resource, 3).map_or(shared.resource.len(), |c| c.len());
    pays(input, &shared.dict, own).then(|| coded(input, &shared, threads))
}

/// The formula of text `input` with dictionary `shared`, or `None` where it is shorter than
/// [`MIN_LEN`], is not [`mostly_text`] or coding it does not pay ([`pays`]). It is the same
/// whatever `threads` is.
pub fn encode_with(input: &[u8], shared: &Shared, threads: usize) -> Option<Formula> {
    if input.len() < MIN_LEN || !mostly_text(input) {
        return None;
    }
    pays(input, &shared.dict, 0).then(|| coded(input, shared, threads))
}

fn coded(input: &[u8], shared: &Shared, threads: usize) -> Formula {
    let text = shared.dict.encode(input, threads);
    let mut f = Formula::default();
    let inputs = vec![f.push_as("wrt/text", Kind::Text, text)];
    f.root = Node::apply_with(MODULE, vec![shared.resource.clone()], Vec::new(), inputs, input.len() as u64);
    f
}

/// The module: the text of `len` bytes from `resources` (the dictionary) and `inputs` (the
/// coded text); no parameters.
pub fn decode(params: &[u8], resources: &[&[u8]], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let (&[], &[dict], &[text]) = (params, resources, inputs) else { return Err(corrupt()) };
    let len = usize::try_from(len).map_err(|_| corrupt())?;
    words::decode(dict, text, len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(f: &Formula, len: usize) -> Result<Vec<u8>> {
        let Node::Apply(a) = &f.root else { panic!() };
        let resources: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
        let inputs: Vec<&[u8]> = f.streams.iter().map(|s| &s.data[..]).collect();
        decode(&a.params, &resources, &inputs, len as u64, 1)
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
}
