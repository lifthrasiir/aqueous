//! aqr-fasta as a module of entry formulas (see aqr-formula): the file rebuilt from its parts
//! (FORMAT.md, "The module"), the headers, the layout, the case and the other letters as they
//! are, for the archive to compress, and the bases coded ([`crate::bases`]).

use aqr_common::varint;
use aqr_formula::{Formula, Kind, Module, Node};

use crate::bases;
use crate::fasta::{self};

pub use crate::module::decode;

pub const MODULE: Module = Module { name: "fasta-recons", version: 1 };

/// The names of the inputs' streams, in order.
pub const STREAM_NAMES: [&str; 5] = ["headers", "layout", "case", "other", "bases"];

/// Whether `d` starts like a FASTA file of nucleotides: a header line, then a line of letters.
pub fn starts(d: &[u8]) -> bool {
    let Some(rest) = d.strip_prefix(b">") else { return false };
    let Some(eol) = rest.iter().take(4096).position(|&b| b == b'\n') else { return false };
    let seq = &rest[eol + 1..];
    let line = &seq[..seq.iter().take(1024).position(|&b| b == b'\n').unwrap_or(seq.len().min(1024))];
    !line.is_empty() && line.iter().all(|c| b"ACGTNacgtn".contains(c))
}

/// The formula of FASTA file `input`, or `None` where it is not one this crate takes
/// ([`fasta`]). It is the same whatever `threads` is (one is used).
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    let p = fasta::parse(input)?;
    let coded = bases::encode(&p.bases);
    let mut params = Vec::new();
    varint::put(&mut params, p.width);
    params.push(p.final_newline as u8);
    let mut f = Formula::default();
    let inputs = vec![
        f.push_as("fasta/headers", Kind::Text, p.headers),
        f.push("fasta/layout", p.layout),
        f.push("fasta/case", p.case),
        f.push("fasta/other", p.other),
        f.push_as("fasta/bases", Kind::Compressed, coded),
    ];
    f.root = Node::apply(MODULE, params, inputs, input.len() as u64);
    Some(f)
}
