//! An application run within another's call on the same thread: a decoder's chain of part
//! applications, the one consuming pulling its input from the one producing it (which runs as
//! the input is pulled). The emulation's state is the thread's, and is shared by every module
//! where they are built as libraries linking against one runtime (crates/aqr-dll's).

use aqr_api::emu::{Drain, Feed, apply, apply_streaming};
use aqr_api::{Input, Memory, Output};

aqr_api::entry! {
    // the bytes of `input0` at even offsets, then those at odd ones, streamed
    part fn alternate(params_len, in0_len, size0, size1) {
        let w = Input::<0>::size();
        let mut b = [0];
        for o in 0..in0_len {
            Input::<0>::request(o + 1);
            Input::<0>::read(o % w, &mut b);
            Input::<0>::release(o + 1);
            if o % 2 == 0 {
                Output::<0>::write(o / 2 % Output::<0>::size(), &b);
                Output::<0>::commit(o / 2 + 1);
            } else {
                Output::<1>::write(o / 2 % Output::<1>::size(), &b);
                Output::<1>::commit(o / 2 + 1);
            }
        }
    }
}

/// An input made by another application of `alternate` (its first output), run as it is pulled.
struct Produced {
    source: Vec<u8>,
    made: Option<Vec<u8>>,
    at: u64,
}

impl Feed for Produced {
    fn len(&self) -> u64 {
        self.source.len().div_ceil(2) as u64
    }

    fn pull(&mut self, hi: u64, put: &mut dyn FnMut(&[u8])) -> Result<(), String> {
        if self.made.is_none() {
            let n = self.source.len() as u64;
            let out =
                apply(&alternate::ENTRY, &[&self.source], &[], &[], &[n.div_ceil(2), n / 2]).map_err(|f| f.what)?;
            self.made = Some(out[0].clone());
        }
        put(&self.made.as_ref().unwrap()[self.at as usize..hi as usize]);
        self.at = hi;
        Ok(())
    }
}

struct Kept(Vec<u8>);

impl Drain for Kept {
    fn push(&mut self, d: &[u8]) -> Result<(), String> {
        self.0.extend_from_slice(d);
        Ok(())
    }
}

#[test]
fn nested_on_one_thread() {
    let source: Vec<u8> = (0..10_000u32).map(|i| (i * 13) as u8).collect();
    let mut input = Produced { source: source.clone(), made: None, at: 0 };
    let n = input.len();
    let (mut even, mut odd) = (Kept(Vec::new()), Kept(Vec::new()));
    apply_streaming(
        &alternate::ENTRY,
        &mut [&mut input],
        &[],
        &[],
        &mut [&mut even, &mut odd],
        &[n.div_ceil(2), n / 2],
    )
    .unwrap();
    // offsets 0 mod 4 of the source, and 2 mod 4
    let at = |r: usize| source.iter().skip(r).step_by(4).copied().collect::<Vec<u8>>();
    assert!(even.0 == at(0) && odd.0 == at(2));
}
