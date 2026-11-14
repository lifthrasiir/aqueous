//! A chunk's NBT (FORMAT.md, §4) parsed as it is made again: its *skeleton*, the NBT without the
//! payloads of its *holes* (a section's block states, its block light and sky light), read into
//! the chunk's buffer with the holes left zero, and the sections found on the way, which the
//! model ([`crate::model`]) fills the holes of.

use crate::{Result, corrupt};

/// NBT's tag types.
pub const END: u8 = 0;
pub const BYTE: u8 = 1;
pub const SHORT: u8 = 2;
pub const INT: u8 = 3;
pub const LONG: u8 = 4;
pub const FLOAT: u8 = 5;
pub const DOUBLE: u8 = 6;
pub const BYTE_ARRAY: u8 = 7;
pub const STRING: u8 = 8;
pub const LIST: u8 = 9;
pub const COMPOUND: u8 = 10;
pub const INT_ARRAY: u8 = 11;
pub const LONG_ARRAY: u8 = 12;

/// The kinds of holes: a section's block states (the `data` of its `block_states`), its block
/// light and its sky light (`BlockLight` and `SkyLight`, of 2048 bytes).
pub const BLOCKS: usize = 0;
pub const BLOCK_LIGHT: usize = 1;
pub const SKY_LIGHT: usize = 2;

/// Bytes of a light array: a nibble for each of a section's 4096 blocks.
pub const LIGHT: usize = 2048;

/// The most nested lists and compounds, and sections, of a chunk.
pub const DEPTH: usize = 512;
pub const SECTIONS: usize = 1024;

/// A list in the buffer: the offset of its first element, their type and how many.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct List {
    pub at: u32,
    pub ty: u8,
    pub count: u32,
}

/// An array in the buffer: the offset of its payload (after its length), its elements, and
/// whether it is a hole.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Array {
    pub at: u32,
    pub count: u32,
    pub hole: bool,
}

/// A section: a compound holding a `Y` byte, a `block_states` compound or light arrays. Of its
/// `block_states`, the `palette` list and the `data` long array.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Section {
    pub y: Option<i8>,
    pub palette: Option<List>,
    pub data: Option<Array>,
    /// Block light, sky light.
    pub light: [Option<Array>; 2],
    /// Whether any field was found twice.
    pub twice: bool,
}

/// What a frame of the parse is in: a compound (its section, if one was made of it, and of a
/// `block_states` compound the section of the compound holding it), or a list (its type and the
/// elements left).
#[derive(Clone, Copy)]
pub enum Frame {
    Compound { section: Option<u16>, states: Option<u16>, palette: bool },
    List { ty: u8, left: u32, palette: bool },
}

/// What gives the bytes of the skeleton: read into the buffer in order (the decoder's), or
/// there already (the encoder's).
pub trait Fill {
    fn fill(&mut self, d: &mut [u8]) -> Result<()>;
}

/// The bytes there already.
pub struct There;

impl Fill for There {
    fn fill(&mut self, _: &mut [u8]) -> Result<()> {
        Ok(())
    }
}

/// A hole found: its kind, its section (an index of the sections) and where its payload is.
#[derive(Debug, Clone, Copy)]
pub struct Candidate {
    pub kind: usize,
    pub section: u16,
    pub at: u32,
    pub len: u32,
}

struct Parse<'a> {
    d: &'a mut [u8],
    at: usize,
    src: &'a mut dyn Fill,
    sections: &'a mut Vec<Section>,
}

impl Parse<'_> {
    /// The next `n` bytes, filled.
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.d.len()).ok_or_else(corrupt)?;
        self.src.fill(&mut self.d[self.at..end])?;
        let s = &self.d[self.at..end];
        self.at = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    /// A length (a non-negative int).
    fn len(&mut self) -> Result<u32> {
        let n = i32::from_be_bytes(self.take(4)?.try_into().unwrap());
        u32::try_from(n).map_err(|_| corrupt())
    }

    /// `n` bytes not filled: a hole, left as it is.
    fn skip(&mut self, n: usize) -> Result<()> {
        self.at = self.at.checked_add(n).filter(|&e| e <= self.d.len()).ok_or_else(corrupt)?;
        Ok(())
    }

    /// The section of the compound of `frame`, made where it has none.
    fn section(&mut self, frame: &mut Frame) -> Result<u16> {
        let Frame::Compound { section, .. } = frame else { unreachable!() };
        if let Some(s) = *section {
            return Ok(s);
        }
        if self.sections.len() >= SECTIONS {
            return Err(corrupt());
        }
        self.sections.push(Section::default());
        let s = (self.sections.len() - 1) as u16;
        *section = Some(s);
        Ok(s)
    }
}

/// The payload of a scalar or array tag of type `ty` (not a list or compound), filled.
fn scalar(p: &mut Parse, ty: u8) -> Result<()> {
    let n = match ty {
        BYTE => 1,
        SHORT => 2,
        INT | FLOAT => 4,
        LONG | DOUBLE => 8,
        BYTE_ARRAY => p.len()? as usize,
        STRING => p.u16()? as usize,
        INT_ARRAY => 4 * p.len()? as usize,
        LONG_ARRAY => 8 * p.len()? as usize,
        _ => return Err(corrupt()),
    };
    p.take(n)?;
    Ok(())
}

/// A chunk's NBT parsed into `d` (of its length), the skeleton's bytes given by `src`: the
/// sections into `sections` (cleared first), and for each hole candidate, in order, `hole` told
/// it and answering whether it is one (its payload then not filled). The NBT is one compound,
/// named, and ends where `d` does.
pub fn parse(
    d: &mut [u8],
    src: &mut dyn Fill,
    sections: &mut Vec<Section>,
    frames: &mut Vec<Frame>,
    hole: &mut dyn FnMut(Candidate) -> Result<bool>,
) -> Result<()> {
    sections.clear();
    frames.clear();
    let mut p = Parse { d, at: 0, src, sections };
    if p.u8()? != COMPOUND {
        return Err(corrupt());
    }
    let n = p.u16()? as usize;
    p.take(n)?;
    frames.push(Frame::Compound { section: None, states: None, palette: false });
    while let Some(top) = frames.last_mut() {
        let mut frame = *top;
        // the next tag of the frame: its type, and its name (in a compound)
        let (ty, name, palette) = match &mut frame {
            Frame::Compound { palette, .. } => {
                let ty = p.u8()?;
                if ty == END {
                    frames.pop();
                    continue;
                }
                let n = p.u16()? as usize;
                let at = p.at;
                p.take(n)?;
                (ty, at..at + n, *palette)
            }
            Frame::List { left, .. } => {
                if *left == 0 {
                    frames.pop();
                    continue;
                }
                *left -= 1;
                *frames.last_mut().unwrap() = frame;
                let Frame::List { ty, palette, .. } = frame else { unreachable!() };
                (ty, 0..0, palette)
            }
        };
        let named = |p: &Parse, s: &[u8]| p.d[name.clone()] == *s;
        let in_compound = matches!(frame, Frame::Compound { .. });
        match ty {
            COMPOUND => {
                let mut child = Frame::Compound { section: None, states: None, palette };
                if in_compound && !palette && named(&p, b"block_states") {
                    let s = p.section(&mut frame)?;
                    child = Frame::Compound { section: None, states: Some(s), palette };
                }
                *frames.last_mut().unwrap() = frame;
                if frames.len() >= DEPTH {
                    return Err(corrupt());
                }
                frames.push(child);
            }
            LIST => {
                let ety = p.u8()?;
                let count = p.len()?;
                let mut in_palette = palette;
                if let Frame::Compound { states: Some(s), .. } = frame
                    && !palette
                    && named(&p, b"palette")
                {
                    let sec = &mut p.sections[s as usize];
                    sec.twice |= sec.palette.is_some();
                    sec.palette = Some(List { at: p.at as u32, ty: ety, count });
                    in_palette = true;
                }
                if frames.len() >= DEPTH {
                    return Err(corrupt());
                }
                frames.push(Frame::List { ty: ety, left: count, palette: in_palette });
            }
            LONG_ARRAY
                if in_compound
                    && !palette
                    && matches!(frame, Frame::Compound { states: Some(_), .. })
                    && named(&p, b"data") =>
            {
                let Frame::Compound { states: Some(s), .. } = frame else { unreachable!() };
                let count = p.len()?;
                let at = p.at as u32;
                let len = count.checked_mul(8).ok_or_else(corrupt)?;
                let h = hole(Candidate { kind: BLOCKS, section: s, at, len })?;
                let sec = &mut p.sections[s as usize];
                sec.twice |= sec.data.is_some();
                sec.data = Some(Array { at, count, hole: h });
                match h {
                    true => p.skip(len as usize)?,
                    false => drop(p.take(len as usize)?),
                }
            }
            BYTE_ARRAY if in_compound && !palette && (named(&p, b"BlockLight") || named(&p, b"SkyLight")) => {
                let kind = if named(&p, b"BlockLight") { BLOCK_LIGHT } else { SKY_LIGHT };
                let count = p.len()?;
                let at = p.at as u32;
                let s = p.section(&mut frame)?;
                *frames.last_mut().unwrap() = frame;
                let h = count as usize == LIGHT && hole(Candidate { kind, section: s, at, len: count })?;
                let sec = &mut p.sections[s as usize];
                let l = &mut sec.light[kind - BLOCK_LIGHT];
                sec.twice |= l.is_some();
                *l = Some(Array { at, count, hole: h });
                match h {
                    true => p.skip(count as usize)?,
                    false => drop(p.take(count as usize)?),
                }
            }
            BYTE if in_compound && !palette && named(&p, b"Y") => {
                let y = p.u8()? as i8;
                let s = p.section(&mut frame)?;
                *frames.last_mut().unwrap() = frame;
                let sec = &mut p.sections[s as usize];
                sec.twice |= sec.y.is_some();
                sec.y = Some(y);
            }
            _ => scalar(&mut p, ty)?,
        }
    }
    match p.at == p.d.len() {
        true => Ok(()),
        false => Err(corrupt()),
    }
}

/// The frames' stack of [`parse`], to be reused.
pub fn frames() -> Vec<Frame> {
    Vec::with_capacity(DEPTH)
}

/// The end of the payload of type `ty` at `at` in `d` (complete there), lists and compounds
/// nested at most `depth` deep.
pub fn skip(d: &[u8], at: usize, ty: u8, depth: usize) -> Result<usize> {
    let get = |at: usize, n: usize| at.checked_add(n).and_then(|e| d.get(at..e)).ok_or_else(corrupt);
    let len = |at: usize| -> Result<usize> {
        let n = i32::from_be_bytes(get(at, 4)?.try_into().unwrap());
        usize::try_from(n).map_err(|_| corrupt())
    };
    let end = match ty {
        BYTE => at + 1,
        SHORT => at + 2,
        INT | FLOAT => at + 4,
        LONG | DOUBLE => at + 8,
        BYTE_ARRAY => at + 4 + len(at)?,
        STRING => at + 2 + u16::from_be_bytes(get(at, 2)?.try_into().unwrap()) as usize,
        INT_ARRAY => at + 4 + 4 * len(at)?,
        LONG_ARRAY => at + 4 + 8 * len(at)?,
        LIST => {
            let ety = *get(at, 1)?.first().unwrap();
            let n = len(at + 1)?;
            let depth = depth.checked_sub(1).ok_or_else(corrupt)?;
            let mut e = at + 5;
            for _ in 0..n {
                e = skip(d, e, ety, depth)?;
            }
            e
        }
        COMPOUND => {
            let depth = depth.checked_sub(1).ok_or_else(corrupt)?;
            let mut e = at;
            loop {
                let t = *get(e, 1)?.first().unwrap();
                e += 1;
                if t == END {
                    break e;
                }
                let n = u16::from_be_bytes(get(e, 2)?.try_into().unwrap()) as usize;
                e = skip(d, e + 2 + n, t, depth)?;
            }
        }
        _ => return Err(corrupt()),
    };
    match end <= d.len() {
        true => Ok(end),
        false => Err(corrupt()),
    }
}
