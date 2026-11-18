//! Blocks parsed: their frames and types, the Zstandard data they hold decoded.

use super::*;

pub(crate) fn leb(d: &[u8], at: &mut usize) -> Decoded<u64> {
    varint::get(d, at).map_err(|e| Failure::Invalid(e.0.into()))
}

/// A module block's module: its Wasm bytes, or a shorthand block's registered hash, whose module
/// the runner reconstructs from its registry on first use (spec, "Registered dictionaries and
/// modules"). A shorthand block carries no module bytes, so none are decompressed or hashed.
pub(crate) enum ModuleSrc {
    Wasm(Vec<u8>),
    Registered([u8; 32], OnceLock<Decoded<Vec<u8>>>),
}

impl ModuleSrc {
    /// The Wasm module's bytes where they are at hand (not a shorthand block, which carries none
    /// before it is resolved): for inspection, which has no runner.
    pub(crate) fn bytes(&self) -> Option<&[u8]> {
        match self {
            ModuleSrc::Wasm(w) => Some(w),
            ModuleSrc::Registered(..) => None,
        }
    }

    /// The Wasm module, resolving a registered hash through `runner` once and keeping it.
    pub(crate) fn wasm(&self, runner: &dyn Runner) -> Decoded<&[u8]> {
        match self {
            ModuleSrc::Wasm(w) => Ok(w),
            ModuleSrc::Registered(h, cell) => {
                cell.get_or_init(|| runner.registered(h)).as_ref().map(Vec::as_slice).map_err(Failure::clone)
            }
        }
    }
}

/// The blocks of an archive, framed: the data of leaf data blocks and non-leaf blocks as it is
/// in the archive (with whether it is Zstandard data, and checked). The blocks of every segment
/// are indexed together, those of a segment after those of the segments before it; what refers
/// to them within a segment (a formula block to module, resource and non-leaf blocks, a block
/// to its dictionary, a metadata block to its blocks) is taken within that segment's
/// ([`Segmented`]).
pub(crate) struct Blocks<'c> {
    pub(crate) leaves: Vec<LeafBlock<'c>>,
    /// Where the leaf, module and resource blocks start, in the archive.
    pub(crate) leaf_at: Vec<usize>,
    pub(crate) module_at: Vec<usize>,
    pub(crate) resource_at: Vec<usize>,
    pub(crate) dictionary_at: Vec<usize>,
    /// The dictionary (by index) of each leaf block, where it takes one.
    pub(crate) leaf_dictionary: Vec<Option<usize>>,
    /// The segments, in their order: an archive without a metadata block has one, of none.
    pub(crate) segments: Vec<Segmented>,
    /// The formula data of the formula blocks as it is in the archive (Zstandard data, checked),
    /// with its dictionary: decoded as the leaves are evaluated, only those of the leaves taken
    /// (an archive has a formula block a leaf, and opening it to take a few of them should not
    /// take all).
    pub(crate) formulas: Vec<(&'c [u8], Option<usize>)>,
    /// The formula data decoded already, by formula block (none, or shorter: not yet), as the
    /// formula blocks of the leaves taken are where they are read for what they refer to
    /// ([`index::Index::blocks_for`]).
    pub(crate) decoded_formulas: Vec<Option<Vec<u8>>>,
    /// The data of each non-leaf block, whether it is Zstandard data, and its dictionary.
    pub(crate) non_leaf: Vec<(&'c [u8], bool, Option<usize>)>,
    /// Where each non-leaf block starts, in the archive.
    pub(crate) non_leaf_at: Vec<usize>,
    pub(crate) modules: Vec<ModuleSrc>,
    pub(crate) resources: Vec<Resource>,
    /// The data of each module and resource block as stored, for ModuleBytes and ResourceBytes
    /// instructions.
    pub(crate) module_data: Vec<&'c [u8]>,
    pub(crate) resource_data: Vec<&'c [u8]>,
    pub(crate) dictionaries: Vec<dict::Dictionary>,
    /// The bytes each block takes in the archive, framing and padding too ([`listing`]).
    pub(crate) sizes: BlockSizes,
}

impl Blocks<'_> {
    /// What formula block `leaf` (a [`LeafBlock::Formula`]) is evaluated in: the module and
    /// resource blocks of its segment before it (`loaded` the modules', by module block, run by
    /// `runner`), and the non-leaf blocks after it; and the end of its segment's non-leaf blocks.
    pub(crate) fn env<'a, 'n>(
        &'a self,
        leaf: &LeafBlock,
        loaded: &'a [OnceLock<Decoded<Box<dyn Loaded + 'n>>>],
        runner: &'n dyn Runner,
    ) -> (Env<'a, 'n>, usize) {
        let &LeafBlock::Formula { modules, resources, non_leaf, segment, .. } = leaf else {
            unreachable!("a formula block")
        };
        let s = &self.segments[segment];
        let (m, r) = (s.first.modules..modules, s.first.resources..resources);
        let env = Env {
            first: non_leaf,
            modules: &self.modules[m.clone()],
            module_data: &self.module_data[m.clone()],
            resource_data: &self.resource_data[r.clone()],
            loaded: &loaded[m],
            resources: &self.resources[r],
            runner,
        };
        (env, s.end.non_leaf)
    }
}

/// The blocks of each kind before a point of an archive (as [`Blocks`] indexes them): those
/// before a segment are not of it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub(crate) leaves: usize,
    pub(crate) non_leaf: usize,
    pub(crate) modules: usize,
    pub(crate) resources: usize,
    pub(crate) dictionaries: usize,
}

/// A segment of an archive: its blocks (of each kind, those from `first` to `end`), and its
/// metadata block (where it starts in the archive, and its decoded data) where it has one.
pub(crate) struct Segmented {
    pub(crate) first: Counts,
    pub(crate) end: Counts,
    pub(crate) metadata: Option<(usize, Vec<u8>)>,
}

/// The bytes each block takes in the archive, by its index among those of its kind.
#[derive(Default)]
pub(crate) struct BlockSizes {
    pub(crate) leaves: Vec<u64>,
    pub(crate) non_leaf: Vec<u64>,
    pub(crate) modules: Vec<u64>,
    pub(crate) resources: Vec<u64>,
    pub(crate) dictionaries: Vec<u64>,
}

pub(crate) enum LeafBlock<'c> {
    /// Its data, whether it is Zstandard data and whether checked, and its dictionary.
    Stored(&'c [u8], bool, bool, Option<usize>),
    /// The number of outputs and the formula data (by index), with the numbers of module and
    /// resource blocks before it and of non-leaf blocks (of all segments), and its segment.
    Formula { outputs: u64, data: usize, modules: usize, resources: usize, non_leaf: usize, segment: usize },
    /// A leaf block a file is at that is not there (salvaging, [`open_salvaging`]).
    Lost,
    /// A leaf block not read, nor to be evaluated ([`index::Index::blocks_for`]).
    Unread,
    /// A leaf block whose blocks do not read, its failure ([`index::Index::blocks_for`]).
    Bad(Failure),
}

/// The blocks `d` of an archive of `size_hint` with a metadata block or not, the payload of an
/// encrypted archive where `sealed`; `d` starting `base` bytes after the header magic (a
/// segment after the first, [`inspect`]), where the blocks are told to be.
pub(crate) fn parse(d: &[u8], base: usize, size_hint: u64, metadata: bool, sealed: bool) -> Decoded<Blocks<'_>> {
    let mut b = Blocks {
        leaves: Vec::new(),
        leaf_at: Vec::new(),
        module_at: Vec::new(),
        resource_at: Vec::new(),
        dictionary_at: Vec::new(),
        leaf_dictionary: Vec::new(),
        segments: Vec::new(),
        formulas: Vec::new(),
        decoded_formulas: Vec::new(),
        non_leaf: Vec::new(),
        non_leaf_at: Vec::new(),
        modules: Vec::new(),
        resources: Vec::new(),
        module_data: Vec::new(),
        resource_data: Vec::new(),
        dictionaries: Vec::new(),
        sizes: BlockSizes::default(),
    };
    // the generic structure first: a block of an undefined critical type anywhere in it makes
    // the archive unsupported, whatever else is wrong with the other blocks
    let framed = frame(d)?;
    if sealed && framed.first().is_none_or(|b| b.1 != SIZE_HINT) {
        return invalid("an encrypted archive's payload without a size hint block");
    }
    if let Some(&(_, ty, ..)) = framed.iter().find(|&&(_, ty, ..)| critical(ty) && !defined(ty)) {
        return Err(Failure::Unsupported(format!("a block of an undefined critical type {ty:#x}")));
    }
    // the blocks before the segment read
    let mut first = Counts::default();
    let counts = |b: &Blocks| Counts {
        leaves: b.leaves.len(),
        non_leaf: b.non_leaf.len(),
        modules: b.modules.len(),
        resources: b.resources.len(),
        dictionaries: b.dictionaries.len(),
    };
    for (start, ty, data, size) in framed {
        let at = base + start + HEADER_MAGIC.len();
        // (a dictionary of the segment's, by its index in it)
        let dicts = (first.dictionaries, b.dictionaries.len());
        let dictionary_of = |d, has| dictionary_of(d, has, dicts.0, dicts.1);
        match ty {
            // spacer blocks, of any space: only compressors have to zero it
            0x00..=0x03 => {}
            SIZE_HINT if base + start == 0 => {
                let mut p = 0;
                if leb(data, &mut p)? != size_hint || p != data.len() {
                    return invalid("bad size hint block");
                }
            }
            // data blocks: `001k kcss` (leaf) and `0100 kkss` (non-leaf, always checked), of a
            // dictionary where kk is 2
            0x20..=0x37 => {
                let (data, dict) = dictionary_of(data, ty & 0x10 != 0)?;
                b.leaf_at.push(at);
                b.sizes.leaves.push(size);
                b.leaf_dictionary.push(dict);
                b.leaves.push(LeafBlock::Stored(data, ty & 0x18 != 0, ty & 0x04 != 0, dict));
            }
            0x40..=0x4b => {
                let (data, dict) = dictionary_of(data, ty & 0x08 != 0)?;
                b.non_leaf.push((data, ty & 0x0c != 0, dict));
                b.non_leaf_at.push(at);
                b.sizes.non_leaf.push(size);
            }
            // a shorthand block: the hash of a registered dictionary
            0x50 => return resource("registered dictionaries are not supported yet"),
            // dictionary blocks: `0101 0tss`
            0x51..=0x57 => {
                b.dictionary_at.push(at);
                b.sizes.dictionaries.push(size);
                b.dictionaries.push(dict::Dictionary::read(decoded(data, true, true)?, ty & 0x04 != 0)?);
            }
            // a shorthand block: the hash of a registered module (resolved on first use)
            0x60 => {
                let hash: [u8; 32] =
                    data.try_into().map_err(|_| Failure::Invalid("a module shorthand block not 32 bytes".into()))?;
                b.module_at.push(at);
                b.sizes.modules.push(size);
                b.modules.push(ModuleSrc::Registered(hash, OnceLock::new()));
                b.module_data.push(data);
            }
            0x61..=0x63 => {
                b.module_at.push(at);
                b.sizes.modules.push(size);
                b.modules.push(ModuleSrc::Wasm(module_of_decoded(decoded(data, true, true)?)?));
                b.module_data.push(data);
            }
            0x64..=0x67 => {
                b.resource_at.push(at);
                b.sizes.resources.push(size);
                b.resources.push(Resource::new(decoded(data, true, true)?));
                b.resource_data.push(data);
            }
            // formula blocks: `0111 0dss`
            0x70..=0x77 => {
                let (data, dict) = dictionary_of(data, ty & 0x04 != 0)?;
                let mut p = 0;
                let outputs = leb(data, &mut p)?;
                if outputs == 0 {
                    return invalid("a formula block of no outputs");
                }
                b.formulas.push((&data[p..], dict));
                b.leaf_at.push(at);
                b.sizes.leaves.push(size);
                b.leaf_dictionary.push(dict);
                b.leaves.push(LeafBlock::Formula {
                    outputs,
                    data: b.formulas.len() - 1,
                    modules: b.modules.len(),
                    resources: b.resources.len(),
                    non_leaf: b.non_leaf.len(),
                    segment: b.segments.len(),
                });
            }
            // mirror blocks and undefined non-critical blocks, not read
            _ if !critical(ty) => {}
            0x10..=0x17 if metadata => {
                let end = counts(&b);
                let m = Some((at, decoded(data, ty & 0x04 != 0, true)?));
                b.segments.push(Segmented { first, end, metadata: m });
                first = end;
            }
            0x10..=0x17 => return invalid("a metadata block, but none in the footer"),
            _ => return invalid(format!("bad block of type {ty:#x}")),
        }
    }
    let end = counts(&b);
    if metadata && end != first {
        return invalid("content blocks after the last metadata block");
    }
    if !metadata {
        b.segments.push(Segmented { first, end, metadata: None });
    }
    Ok(b)
}

/// A block framed: where it starts, its type, its data and the bytes it takes (with its
/// padding).
type Framed<'d> = (usize, u64, &'d [u8], u64);

/// The blocks `d` framed.
pub(crate) fn frame(d: &[u8]) -> Decoded<Vec<Framed<'_>>> {
    let mut framed = Vec::new();
    let mut at = 0;
    while at < d.len() {
        let start = at;
        let ty = leb(d, &mut at)?;
        if at - start != varint::len(ty) {
            return invalid("a block type not in the shortest encoding");
        }
        let width = 1 << (ty % 4);
        let size = d.get(at..at + width).ok_or_else(|| Failure::Invalid("truncated block".into()))?;
        at += width;
        let size = u64::from_le_bytes(std::array::from_fn(|i| size.get(i).copied().unwrap_or(0)));
        let data = usize::try_from(size)
            .ok()
            .and_then(|n| d.get(at..at.checked_add(n)?))
            .ok_or_else(|| Failure::Invalid("truncated block".into()))?;
        at += data.len();
        // the padding, of any value: only compressors have to zero it
        if (at - start) % 2 == 1 {
            if at == d.len() {
                return invalid("truncated block");
            }
            at += 1;
        }
        framed.push((start, ty, data, (at - start) as u64));
    }
    Ok(framed)
}

/// The leading digit of block type `ty`: the first digit of its hexadecimal representation,
/// padded with zeros to at least 2 digits.
fn leading_digit(ty: u64) -> u64 {
    let digits = (64 - ty.leading_zeros()).div_ceil(4).max(2);
    ty >> (4 * (digits - 1))
}

/// Whether block type `ty` is critical: its leading digit is from 0 to 9.
pub(crate) fn critical(ty: u64) -> bool {
    leading_digit(ty) <= 9
}

/// Whether critical block type `ty` is defined by the spec, block-like structures included.
pub(crate) fn defined(ty: u64) -> bool {
    matches!(
        ty,
        0x00..=0x04
            | 0x10..=0x17
            | 0x20..=0x37
            | 0x40..=0x4b
            | 0x50..=0x57
            | 0x60..=0x67
            | 0x70..=0x77
            | HEADER_TYPE
            | OVERLAY_HEADER_TYPE
            | FOOTER_TYPE
            | OVERLAY_FOOTER_TYPE
            | ENCRYPTED_HEADER_TYPE
    )
}

/// The data of a block's data `d` past its dictionary index where it has one (`has`), and the
/// index: one of the dictionary blocks from `first` to `before` (those of its segment before it),
/// as an index among all.
pub(crate) fn dictionary_of(d: &[u8], has: bool, first: usize, before: usize) -> Decoded<(&[u8], Option<usize>)> {
    if !has {
        return Ok((d, None));
    }
    let mut at = 0;
    let i = leb(d, &mut at)?;
    match usize::try_from(i).ok().filter(|&i| i < before - first) {
        Some(i) => Ok((&d[at..], Some(first + i))),
        None => invalid("a dictionary index of no dictionary block before"),
    }
}

/// The decoded data of a block's data `d`.
pub(crate) fn decoded(d: &[u8], zstd: bool, checked: bool) -> Decoded<Vec<u8>> {
    decoded_with(d, zstd, checked, None)
}

/// The data of block data `d` stored as it is (not Zstandard data), its checksum checked: as
/// [`decoded_with`] gives it, not copied.
pub(crate) fn stored_checked(d: &[u8]) -> Decoded<&[u8]> {
    let end = d.len().checked_sub(8).ok_or_else(|| Failure::Invalid("truncated".into()))?;
    if checksum(&d[..end]).to_le_bytes() != d[end..] {
        return invalid("checksum mismatch");
    }
    Ok(&d[..end])
}

/// [`decoded`], of Zstandard data compressed with dictionary `dict`.
pub(crate) fn decoded_with(d: &[u8], zstd: bool, checked: bool, dict: Option<&dict::Dictionary>) -> Decoded<Vec<u8>> {
    let end = d.len().checked_sub(if checked { 8 } else { 0 }).ok_or_else(|| Failure::Invalid("truncated".into()))?;
    let out = if zstd {
        let mut at = 0;
        // the original size by how much longer it is, or `0` then by how much shorter
        let delta = leb(d, &mut at)?;
        let shrink = if delta == 0 { leb(d, &mut at)? } else { 0 };
        let &window = d.get(at).filter(|_| at < end).ok_or_else(|| Failure::Invalid("truncated".into()))?;
        let blocks = &d[at + 1..end];
        let size = (blocks.len() as u64)
            .checked_add(delta)
            .and_then(|n| n.checked_sub(shrink))
            .ok_or_else(|| Failure::Invalid("bad size".into()))?;
        unzstd(window, blocks, size, dict)?
    } else {
        d[..end].to_vec()
    };
    if checked && checksum(&out).to_le_bytes() != d[end..] {
        return invalid("checksum mismatch");
    }
    Ok(out)
}

fn zstd_failure(e: zstd_safe::ErrorCode) -> Failure {
    Failure::Invalid(format!("zstd: {}", zstd_safe::get_error_name(e)))
}

/// Zstandard `blocks` of `window` making exactly `size` bytes, with dictionary `dict`.
fn unzstd(window: u8, blocks: &[u8], size: u64, dict: Option<&dict::Dictionary>) -> Decoded<Vec<u8>> {
    let _s = aqr_common::trace::span("unzstd");
    // A context of the thread's, kept for the next frame (with its input buffer) where there is
    // no dictionary to refer to: archives have a formula block a leaf, each a frame, and making
    // a context for each was much of opening a big archive.
    thread_local! {
        static DCTX: RefCell<Option<zstd_safe::DCtx<'static>>> = const { RefCell::new(None) };
    }
    match dict {
        None => DCTX.with(|c| match c.try_borrow_mut() {
            Ok(mut c) => {
                let dctx = c.get_or_insert_with(zstd_safe::DCtx::create);
                dctx.reset(ResetDirective::SessionAndParameters).map_err(zstd_failure)?;
                unzstd_with(dctx, window, blocks, size)
            }
            Err(_) => unzstd_with(&mut zstd_safe::DCtx::create(), window, blocks, size),
        }),
        Some(d) => {
            let mut dctx = zstd_safe::DCtx::create();
            match d {
                dict::Dictionary::Raw(d) => dctx.ref_prefix(d).map(drop).map_err(zstd_failure)?,
                dict::Dictionary::Tables(d) => dctx.ref_ddict(d).map(drop).map_err(zstd_failure)?,
            }
            unzstd_with(&mut dctx, window, blocks, size)
        }
    }
}

/// [`unzstd`] with `dctx` (of its dictionary, if any).
fn unzstd_with(dctx: &mut zstd_safe::DCtx, window: u8, blocks: &[u8], size: u64) -> Decoded<Vec<u8>> {
    // the frame: a header, then the blocks (not copied after it)
    let head = [ZSTD_MAGIC[0], ZSTD_MAGIC[1], ZSTD_MAGIC[2], ZSTD_MAGIC[3], 0, window];
    let mut frame = [&head[..], blocks].into_iter();
    let mut input = InBuffer::around(frame.next().unwrap());
    // up to one byte more than `size`, to tell a longer output: as much room as that from the
    // start where it can be had (written into as it is, without a window buffer of the
    // context's), else made as the output comes
    let limit = size.saturating_add(1);
    let mut out: Vec<u8> = Vec::new();
    if let Ok(n) = usize::try_from(limit)
        && out.try_reserve_exact(n).is_ok()
    {
        dctx.set_parameter(zstd_safe::DParameter::StableOutBuffer(true)).map_err(zstd_failure)?;
    }
    loop {
        let Some(room) = limit.checked_sub(out.len() as u64).filter(|&r| r > 0) else {
            return invalid("decompressed data longer than declared");
        };
        out.reserve(room.min(1 << 20) as usize);
        let (pos, before) = (out.len(), input.pos());
        let mut o = OutBuffer::around_pos(&mut out, pos);
        let hint = dctx.decompress_stream(&mut o, &mut input).map_err(zstd_failure)?;
        let written = o.pos() > pos;
        if hint == 0 {
            break;
        }
        if input.pos() == input.src.len()
            && let Some(next) = frame.next()
        {
            input = InBuffer::around(next);
        } else if !written && input.pos() == before {
            return invalid("truncated zstd data");
        }
    }
    if input.pos() != input.src.len() || frame.next().is_some() || out.len() as u64 != size {
        return invalid("zstd data of another size than declared");
    }
    Ok(out)
}
