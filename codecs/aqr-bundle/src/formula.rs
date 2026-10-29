//! aqr-bundle as a module of entry formulas (see aqr-formula): an ar, tar or cpio archive
//! rebuilt from the contents of its members and its layout. `FORMAT.md` specifies it. The
//! contents may be recompressed in turn.

use aqr_formula::{Formula, Kind, Module, Node};

use crate::layout::{self, Rec, Role, Writer};
use crate::{ar, cpio, tar};

pub use crate::module::decode;

pub const MODULE: Module = Module::new("bundle-recons", 1).with_memory(memory);

/// The memory of the module: the inputs and the archive, and a header copied out of the layout
/// at a time.
fn memory(u: &aqr_formula::Usage<'_>) -> Option<u64> {
    let &[_, layout] = u.inputs else { return None };
    Some(u.whole(layout, 1))
}

/// The archives this crate takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Ar,
    Tar,
    Cpio,
}

/// The format `d` starts like, if any.
pub fn detect(d: &[u8]) -> Option<Format> {
    if ar::starts(d) {
        Some(Format::Ar)
    } else if cpio::starts(d) {
        Some(Format::Cpio)
    } else if tar::starts(d) {
        Some(Format::Tar)
    } else {
        None
    }
}

/// The records of archive `d` of `format`, if it is one.
pub fn records(format: Format, d: &[u8]) -> Option<Vec<Rec>> {
    match format {
        Format::Ar => ar::records(d),
        Format::Tar => tar::records(d),
        Format::Cpio => cpio::records(d),
    }
}

/// Bytes of the archive of `format` at the start of `d`: up to the end of its last member, and
/// the zeros after it. `None` where no member is found.
pub fn extent(format: Format, d: &[u8]) -> Option<usize> {
    let recs = records(format, d)?;
    let last = recs.iter().rposition(|r| r.body().is_some())?;
    Some(match recs.get(last + 1) {
        Some(Rec::Zeros(z)) => z.end,
        _ => match &recs[last] {
            // (the end of a member's padding: where the next record starts)
            &Rec::Ar { at, pre, ref body, pad } => at + layout::AR_HEADER + pre + body.span.len() + pad as usize,
            Rec::Tar { body, .. } => body.span.end + layout::pad_to(body.span.len(), layout::TAR_BLOCK),
            &Rec::Cpio { form, ref body, .. } => body.span.end + layout::pad_to(body.span.end, form.align()),
            _ => unreachable!("a member"),
        },
    })
}

/// Bodies of at least this many bytes are slices of their own, apart from the others of their
/// kind next to them: so that the same contents elsewhere (the files of a tar archive, archived
/// along with it) are found by their slices. Those shorter (most members of ar archives, of
/// object files) cost less as one slice.
pub const OWN_MIN: usize = 1 << 10;

#[derive(Debug, Clone)]
pub struct Options {
    /// What a member's contents are by its name: the contents of each [`Kind`] are a stream of
    /// their own (those of [`Kind::Unknown`] the stream `raw`, the others after the layout),
    /// sliced in order. By default all are unknown. Symbol tables are [`Kind::Table`].
    pub kind: fn(&[u8]) -> Kind,
    /// Bodies of at least this many bytes are slices of their own; by default [`OWN_MIN`].
    pub own: usize,
}

impl Default for Options {
    fn default() -> Options {
        Options { kind: |_| Kind::Unknown, own: OWN_MIN }
    }
}

/// The formula rebuilding archive `input` (one of [`detect`]'s), or None where it has no
/// member. The same whatever `threads` is (one is used).
pub fn encode(input: &[u8], _threads: usize) -> Option<Formula> {
    encode_with(input, &Options::default())
}

/// [`encode`] with `opt`.
pub fn encode_with(input: &[u8], opt: &Options) -> Option<Formula> {
    let recs = records(detect(input)?, input)?;
    if !recs.iter().any(|r| r.body().is_some()) {
        return None;
    }
    let mut w = Writer::new(input);
    recs.iter().enumerate().for_each(|(i, r)| w.rec(r, i));
    let mut data = Vec::with_capacity(w.data.iter().map(|b| b.0.len()).sum());
    let mut kinds = Vec::with_capacity(w.data.len());
    for (span, role, i) in &w.data {
        data.extend_from_slice(&input[span.clone()]);
        let kind = match role {
            Role::Table => Kind::Table,
            _ => (opt.kind)(&recs[*i].body().expect("a member").name),
        };
        kinds.push((kind, data.len()));
    }
    // The layout must give the archive back: a check of everything, which is cheap.
    if layout::build(&w.layout, &data, input.len() as u64).ok().as_deref() != Some(input) {
        return None;
    }
    let mut f = Formula::default();
    let mut inputs = vec![f.push("raw", Vec::new()), f.push("bundle/layout", w.layout)];
    inputs[0] = f.push_by_kind(0, data, &kinds, "bundle", opt.own);
    f.root = Node::apply(MODULE, Vec::new(), inputs, input.len() as u64);
    Some(f)
}
