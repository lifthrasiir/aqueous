//! The code of the module (see `formula`, and `FORMAT.md`), which needs no aqr-formula, so that
//! aqr-modules builds it into its Wasm module.

use crate::layout::{self};
use crate::{Result, corrupt};

/// Pages of the windows of the module as a part module ([`decode_part`]): of each input and of
/// the output.
pub const WINDOW: u32 = 16;

/// Bytes of the pieces in which [`decode_part`] takes its inputs.
const PIECE: usize = 1 << 16;

/// The memory plan of the module as a part module ([`aqr_api::stack`]): what [`decode_part`] takes
/// beyond its initial memory at most, whatever the lengths: a piece for each input.
pub fn plan() -> u64 {
    2 * aqr_api::stack::block(PIECE as u64)
}

/// The `K`-th input of a part module read in order, through its window, in pieces.
struct In<const K: u32> {
    at: u64,
    len: u64,
    buf: Vec<u8>,
}

impl<const K: u32> In<K>
where
    aqr_api::Input<K>: aqr_api::Memory,
{
    fn new(len: u64) -> In<K> {
        In { at: 0, len, buf: vec![0; PIECE] }
    }

    /// Its next `n` bytes, given to `f` in pieces.
    fn each(&mut self, n: usize, f: &mut dyn FnMut(&[u8])) -> Result<()> {
        use aqr_api::{Input, Memory};
        let end = self.at.checked_add(n as u64).filter(|&e| e <= self.len).ok_or_else(corrupt)?;
        let w = Input::<K>::size();
        while self.at < end {
            let k = (end - self.at).min(PIECE as u64).min(w - self.at % w);
            let piece = &mut self.buf[..k as usize];
            Input::<K>::request(self.at + k);
            Input::<K>::read(self.at % w, piece);
            Input::<K>::release(self.at + k);
            self.at += k;
            f(piece);
        }
        Ok(())
    }
}

/// The layout and the data of [`decode_part`].
struct Pulled {
    data: In<0>,
    layout: In<1>,
}

impl layout::Source for Pulled {
    fn layout(&mut self, n: usize, f: &mut dyn FnMut(&[u8])) -> Result<()> {
        self.layout.each(n, f)
    }

    fn data(&mut self, n: usize, f: &mut dyn FnMut(&[u8])) -> Result<()> {
        self.data.each(n, f)
    }

    fn more(&self) -> bool {
        self.layout.at < self.layout.len
    }

    fn done(&self) -> bool {
        self.data.at == self.data.len
    }
}

/// The output of [`decode_part`], written through its window: `size` bytes in all.
struct Written {
    at: usize,
    size: usize,
    /// Whether more was put than `size`.
    failed: bool,
}

impl layout::Sink for Written {
    fn len(&self) -> usize {
        self.at
    }

    fn put(&mut self, mut d: &[u8]) {
        use aqr_api::{Memory, Output};
        if d.len() > self.size - self.at {
            self.failed = true;
            return;
        }
        let w = Output::<0>::size() as usize;
        while !d.is_empty() {
            let k = d.len().min(w - self.at % w);
            Output::<0>::write((self.at % w) as u64, &d[..k]);
            self.at += k;
            Output::<0>::commit(self.at as u64);
            d = &d[k..];
        }
    }
}

/// The module as a part module (its entry point, of `aqr_api::entry!`): what [`decode`] gives,
/// its inputs taken and its output put in order through windows ([`WINDOW`]), in pieces:
/// nothing is held, whatever the lengths.
pub fn decode_part(params_len: u64, data_len: u64, layout_len: u64, size: u64) {
    aqr_api::finish(bundle_part(params_len, data_len, layout_len, size));
}

fn bundle_part(params_len: u64, data_len: u64, layout_len: u64, size: u64) -> Result<()> {
    if params_len != 0 {
        return Err(corrupt());
    }
    let mut src = Pulled { data: In::new(data_len), layout: In::new(layout_len) };
    let mut out = Written { at: 0, size: usize::try_from(size).map_err(|_| corrupt())?, failed: false };
    layout::build_into(&mut src, &mut out, size, &mut |_| {})?;
    match out.failed {
        true => Err(corrupt()),
        false => Ok(()),
    }
}

/// The module: the archive of `len` bytes rebuilt from `inputs`, the contents and the layout.
pub fn decode(params: &[u8], inputs: &[&[u8]], len: u64, _threads: usize) -> Result<Vec<u8>> {
    let (&[], &[data, layout]) = (params, inputs) else { return Err(corrupt()) };
    layout::build(layout, data, len)
}
