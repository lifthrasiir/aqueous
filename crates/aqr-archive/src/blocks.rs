//! Blocks written: the footer, leaf and non-leaf blocks, formula, dictionary and shorthand
//! blocks, to an [`Out`] (bytes, or only their count).

use super::*;

/// Where an archive is written: its bytes, or only how many ([`Count`], to size it).
pub(crate) trait Out {
    fn put(&mut self, b: &[u8]);
    fn len(&self) -> usize;
}

impl Out for Vec<u8> {
    fn put(&mut self, b: &[u8]) {
        self.extend_from_slice(b);
    }

    fn len(&self) -> usize {
        Vec::len(self)
    }
}

/// Bytes written to a writer, up to the first failure (and nothing after it).
pub(crate) struct Writing<'a> {
    pub(crate) out: &'a mut dyn io::Write,
    pub(crate) len: usize,
    pub(crate) failed: Option<io::Error>,
}

impl Out for Writing<'_> {
    fn put(&mut self, b: &[u8]) {
        if self.failed.is_none() {
            match self.out.write_all(b) {
                Ok(()) => self.len += b.len(),
                Err(e) => self.failed = Some(e),
            }
        }
    }

    fn len(&self) -> usize {
        self.len
    }
}

/// Bytes written, counted only.
pub(crate) struct Count(pub(crate) usize);

impl Out for Count {
    fn put(&mut self, b: &[u8]) {
        self.0 += b.len();
    }

    fn len(&self) -> usize {
        self.0
    }
}

/// The end of an archive whose blocks take `blocks` bytes, without a metadata block.
pub(crate) fn footer(out: &mut impl Out, blocks: usize) {
    footer_with(out, blocks, 0);
}

/// The end of an archive whose blocks take `blocks` bytes, its last metadata block `metadata`
/// bytes before the footer's fields (0: none).
pub(crate) fn footer_with(out: &mut impl Out, blocks: usize, metadata: u64) {
    let mut f = Vec::new();
    varint::put(&mut f, blocks as u64);
    varint::put(&mut f, metadata);
    out.put(&FOOTER_MAGIC);
    out.put(&(f.len() as u16).to_le_bytes());
    out.put(&f);
    out.put(&(f.len() as u16).to_le_bytes());
}

/// Appends a block of `ty` (with a u8 size field) and `data`, widening the size field as
/// needed and padding the block to an even length.
pub(crate) fn block(out: &mut impl Out, ty: u64, data: &[u8]) {
    block_of(out, ty, &[data]);
}

/// Appends a formula block of `num_outputs` outputs and the compressed formula data `data`.
pub(crate) fn formula_block(out: &mut impl Out, num_outputs: u64, data: &[u8]) {
    let mut n = Vec::new();
    varint::put(&mut n, num_outputs);
    block_of(out, FORMULA, &[&n, data]);
}

/// A block of the bytes of `parts` one after the other. A dictionary or module block in full
/// takes a 2-byte size at least, as its type with a 1-byte size is a shorthand block's.
pub(crate) fn block_of(out: &mut impl Out, ty: u64, parts: &[&[u8]]) {
    let start = out.len();
    let len: usize = parts.iter().map(|p| p.len()).sum();
    let ss = match len as u64 {
        0..0x100 if ty != DICTIONARY_RAW && ty != MODULE => 0,
        0..0x1_0000 => 1,
        0x1_0000..0x1_0000_0000 => 2,
        _ => 3,
    };
    let mut head = Vec::with_capacity(16);
    varint::put(&mut head, ty + ss);
    head.extend_from_slice(&(len as u64).to_le_bytes()[..1 << ss]);
    out.put(&head);
    parts.iter().for_each(|p| out.put(p));
    if (out.len() - start) % 2 == 1 {
        out.put(&[0]);
    }
}

/// Appends a dictionary block of `data` (with entropy tables), compressed.
pub(crate) fn dictionary_block(out: &mut impl Out, data: &[u8]) {
    block(out, DICTIONARY, data);
}

/// Appends a shorthand block (`DICTIONARY` or `MODULE`) of a 32-byte registered `hash`: the type
/// with a 1-byte size, which [`block_of`] never makes for these (spec, "Registered dictionaries
/// and modules").
pub(crate) fn shorthand_block(out: &mut impl Out, ty: u64, hash: &[u8; 32]) {
    let start = out.len();
    let mut head = Vec::with_capacity(2);
    varint::put(&mut head, ty); // a 1-byte size follows (size class 0)
    head.push(hash.len() as u8);
    out.put(&head);
    out.put(hash);
    if (out.len() - start) % 2 == 1 {
        out.put(&[0]);
    }
}

/// Appends a dictionary block standing in for another, where an archive is to have as many
/// before its own (as a batch alone has, [`SegmentWriter::standalone`]): raw content of 256
/// bytes, uncompressed, so that it is no shorthand block (spec, "Registered dictionaries and
/// modules").
pub(crate) fn dictionary_stand_in(out: &mut impl Out) {
    let data = stored_external(&[&[0; 256]]);
    debug_assert!(data.len() >= 0x100);
    block(out, DICTIONARY_RAW, &data);
}
