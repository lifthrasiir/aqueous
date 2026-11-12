//! Files of tensors as far as the encoder reads them (FORMAT.md §2): where their tensor data
//! starts, and the tensors of the types this crate codes, of any of the containers it knows
//! (GGUF, PyTorch's `torch.save`, safetensors).

use crate::layout::{Float, Quant};
use crate::{gguf, pth, safetensors};

/// What a tensor's bytes are, where they are coded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Data {
    /// `n` floats.
    Float(Float, u64),
    /// `n` blocks.
    Quant(Quant, u64),
    /// `n` elements of so many bytes (integers, packed FP4, and the like: FORMAT.md §4.4).
    Lanes(u8, u64),
}

impl Data {
    pub fn len(self) -> u64 {
        match self {
            Data::Float(f, n) => n * f.bytes() as u64,
            Data::Quant(q, n) => n * q.block() as u64,
            Data::Lanes(w, n) => n * w as u64,
        }
    }

    /// Its count: of floats, blocks or elements.
    pub fn count(self) -> u64 {
        let (Data::Float(_, n) | Data::Quant(_, n) | Data::Lanes(_, n)) = self;
        n
    }

    /// The same of `n`.
    pub fn with_count(self, n: u64) -> Data {
        match self {
            Data::Float(f, _) => Data::Float(f, n),
            Data::Quant(q, _) => Data::Quant(q, n),
            Data::Lanes(w, _) => Data::Lanes(w, n),
        }
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

/// The containers of tensors this crate takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    /// llama.cpp's GGUF.
    Gguf,
    /// PyTorch's `torch.save` of the ZIP format (since 1.6): a pickle, and each storage an entry.
    Pth,
    /// Hugging Face's safetensors: a header of JSON, then the data.
    Safetensors,
}

impl Container {
    /// The class of the stream of the bytes before the tensor data.
    pub fn header(self) -> &'static str {
        match self {
            Container::Gguf => "gguf/header",
            Container::Pth => "pth/header",
            Container::Safetensors => "safetensors/header",
        }
    }
}

/// A tensor coded: where it is, what it is, and the elements of its rows (its last dimension;
/// 0 where it is not known).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tensor {
    pub start: usize,
    pub data: Data,
    pub row: u64,
}

/// A file's layout: its container, where its tensor data starts (the bytes before it its
/// header), and the tensors of types this crate codes, in the order of their places, apart,
/// from there on within the file.
#[derive(Debug)]
pub struct Tensors {
    pub container: Container,
    pub data: usize,
    pub tensors: Vec<Tensor>,
}

/// The container `d` starts like, if any (its layout may not hold still: [`parse`]).
pub fn starts(d: &[u8]) -> Option<Container> {
    if gguf::starts(d) {
        Some(Container::Gguf)
    } else if pth::starts(d) {
        Some(Container::Pth)
    } else if safetensors::starts(d) {
        Some(Container::Safetensors)
    } else {
        None
    }
}

/// The layout of file `d`, None where it is not one of a container this crate takes (or its
/// header does not hold).
pub fn parse(d: &[u8]) -> Option<Tensors> {
    match starts(d)? {
        Container::Gguf => gguf::parse(d),
        Container::Pth => pth::parse(d),
        Container::Safetensors => safetensors::parse(d),
    }
}

/// The layout of the tensors `coded` (their places in the file, what they are and their rows'
/// elements) from `data` on: those past the file's end, empty or overlapping the one before them
/// (by their places) left out.
pub(crate) fn apart(container: Container, d: &[u8], data: usize, mut coded: Vec<(u64, Data, u64)>) -> Tensors {
    coded.sort_by_key(|t| t.0);
    let mut end = data as u64;
    let mut tensors = Vec::with_capacity(coded.len());
    for (start, what, row) in coded {
        // (as many floats or blocks as there are bytes at most: their length does not overflow)
        if what.count() > d.len() as u64 {
            continue;
        }
        let Some(e) = start.checked_add(what.len()) else { continue };
        if what.is_empty() || e > d.len() as u64 || start < end {
            continue;
        }
        end = e;
        // (a row that does not divide the tensor's elements is not told)
        let row = if row > 0 && what.count() % row == 0 { row } else { 0 };
        tensors.push(Tensor { start: start as usize, data: what, row });
    }
    Tensors { container, data, tensors }
}
