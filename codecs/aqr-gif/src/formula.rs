//! aqr-gif as a module of entry formulas (see aqr-formula): a GIF file rebuilt from its
//! skeleton and the record of its images. `FORMAT.md` specifies it.

use aqr_formula::{Formula, Kind, Module, Node};

use crate::model::Model;
use crate::{gif, recons};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("gif-recons", 1).with_memory(memory);

/// The memory of the module: the inputs and the output, the model, and the largest image's
/// pixels as it is coded (in rows and in the stream's order, its codes, and its stream).
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let [sk, record] = u.all_contents::<2>()?;
    let l = recons::layout(sk, record).ok()?;
    let fixed = 5 * (crate::lzw::CODES as u64 * 2) + recons::TRIE_BYTES;
    Some(u.whole(Model::bytes(l.area, l.screen) + 6 * l.largest + fixed, 8))
}

/// Whether `d` starts with a GIF file holding an image, as far as its blocks tell.
pub fn starts(d: &[u8]) -> bool {
    gif::header(d) && gif::parse(d).is_some_and(|g| !g.images.is_empty())
}

/// Bytes of the GIF file at the start of `d`, up to its trailer or where its blocks end; None
/// where it holds no image.
pub fn extent(d: &[u8]) -> Option<usize> {
    gif::parse(d).filter(|g| !g.images.is_empty()).map(|g| g.end)
}

/// The formula rebuilding `input`, a GIF file, or None where it is not one or no image of it
/// is coded.
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    let sp = recons::split(input).ok()?;
    let mut f = Formula::default();
    let sk = f.push("gif/skeleton", sp.skeleton);
    let record = f.push_as("gif/record", Kind::Compressed, sp.record);
    f.root = Node::apply(MODULE, vec![], vec![sk, record], input.len() as u64);
    Some(f)
}
