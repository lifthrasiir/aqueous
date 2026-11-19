//! `mszip-recons` as a part module (windows of four pages, the inputs taken and the output put
//! in order) against the whole decoding, on inputs made for the purpose: folders of blocks of
//! stored and of Huffman DEFLATE blocks past the windows, stored blocks of the raw input, bytes
//! kept between them and after them.

use aqr_api::Window;
use aqr_api::emu::{self, Entry, Kind};
use aqr_mszip::module::{self, Block};
use aqr_mszip::mszip::{self, BitW};

const ENTRY: Entry = Entry {
    kind: Kind::Part,
    windows: &[
        Window::Input(0, module::WINDOW),
        Window::Input(1, module::WINDOW),
        Window::Input(2, module::WINDOW),
        Window::Input(3, module::WINDOW),
        Window::Output(0, module::WINDOW),
    ],
    arity: 6,
    call: |a| module::decode_part(a[0], a[1], a[2], a[3], a[4], a[5]),
};

/// The four inputs of the module and its output.
struct Case {
    inputs: [Vec<u8>; 4],
    want: Vec<u8>,
}

/// Noise, from `seed`.
fn noise(n: usize, seed: &mut u64) -> Vec<u8> {
    (0..n)
        .map(|_| {
            *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (*seed >> 33) as u8
        })
        .collect()
}

/// Text of about `n` bytes.
fn text(n: usize, seed: u64) -> Vec<u8> {
    (0..n / 12 + 1)
        .flat_map(|i| format!("w{:05} {:03}\n", (i as u64).wrapping_mul(seed) % 9973, i % 211).into_bytes())
        .collect()
}

/// A MSZIP block (with its signature) of a DEFLATE stream of `data`: stored (a final block of its
/// own), or of Huffman blocks by zlib's strategies.
fn block(data: &[u8], kind: usize) -> Vec<u8> {
    let mut b = b"CK".to_vec();
    match kind {
        0 => {
            b.push(1);
            b.extend((data.len() as u16).to_le_bytes());
            b.extend((!(data.len() as u16)).to_le_bytes());
            b.extend(data);
        }
        _ => {
            let strategy =
                if kind == 1 { aqr_flate::zlib::Strategy::Default } else { aqr_flate::zlib::Strategy::Fixed };
            let p = aqr_flate::zlib::Params { level: 6, strategy, ..Default::default() };
            b.extend(aqr_flate::zlib::compress(data, p).unwrap());
        }
    }
    b
}

/// `blocks` MSZIP blocks in two folders (kinds of DEFLATE blocks by turns), a raw block now and
/// then, 8 bytes kept before each block and 5 after the last.
fn case(blocks: usize, seed: u64) -> Case {
    let mut seed = seed;
    let (mut w, mut raw, mut kept, mut index, mut want) = (BitW::new(Vec::new()), vec![], vec![], vec![], vec![]);
    let mut entries = Vec::new();
    for folder in [0, 1] {
        let mut folder_blocks = Vec::new();
        for i in 0..blocks / 2 {
            let kind = match i % 7 {
                3 => 1,
                5 => 2,
                _ => 0,
            };
            let data = if kind == 0 { noise(32768, &mut seed) } else { text(20_000 + 100 * i, seed | 1) };
            folder_blocks.push(block(&data, kind));
        }
        let refs: Vec<&[u8]> = folder_blocks.iter().map(|b| &b[..]).collect();
        let joined = mszip::join(&refs, &mut w).unwrap();
        for (i, (j, b)) in joined.iter().zip(&folder_blocks).enumerate() {
            let header = [folder as u8 + 1, i as u8, 3, 4, 5, 6, 7, 8];
            kept.extend(header);
            want.extend(header);
            entries.push(Block::Mszip { kept: 8, blocks: j.blocks });
            want.extend(b);
            if i % 5 == 4 {
                let r = noise(3000 + i, &mut seed);
                kept.extend(header);
                want.extend(header);
                entries.push(Block::Raw { kept: 8, len: r.len() });
                raw.extend(&r);
                want.extend(&r);
            }
        }
    }
    kept.extend([9, 9, 9, 9, 9]);
    want.extend([9, 9, 9, 9, 9]);
    index.extend(module::index(&entries));
    w.align();
    Case { inputs: [w.out, raw, kept, index], want }
}

fn part(c: &Case, size: u64) -> Result<Vec<u8>, String> {
    let refs: Vec<&[u8]> = c.inputs.iter().map(|i| &i[..]).collect();
    emu::apply(&ENTRY, &refs, &[], &[], &[size]).map(|mut o| o.pop().unwrap()).map_err(|f| f.what)
}

fn whole(inputs: &[Vec<u8>; 4], size: u64) -> Result<Vec<u8>, String> {
    let refs: Vec<&[u8]> = inputs.iter().map(|i| &i[..]).collect();
    module::decode(&[], &refs, size).map_err(|e| e.to_string())
}

#[test]
fn part_same_as_whole() {
    for blocks in [2, 6, 40, 200] {
        let c = case(blocks, blocks as u64);
        let size = c.want.len() as u64;
        assert_eq!(whole(&c.inputs, size).unwrap(), c.want, "{blocks}");
        assert!(part(&c, size).unwrap() == c.want, "{blocks}");
        if blocks >= 200 {
            // (past the windows: the output and the joined stream)
            assert!(size > 4 * 65536 * 2 && c.inputs[0].len() > 4 * 65536 * 2);
        }
    }
}

/// Inputs that do not make the output asked for, or are not used up: both fail alike.
#[test]
fn part_fails_as_whole() {
    let c = case(40, 5);
    let size = c.want.len() as u64;
    for (what, size) in [("longer", size + 1), ("shorter", size - 1), ("none", 0)] {
        assert!(whole(&c.inputs, size).is_err(), "{what}");
        assert!(part(&c, size).is_err(), "{what}");
    }
    for i in 0..4 {
        // (a byte short; a byte more; cut in two)
        for edit in 0..3 {
            let mut d = Case { inputs: c.inputs.clone(), want: Vec::new() };
            match edit {
                0 => drop(d.inputs[i].pop()),
                1 => d.inputs[i].push(0),
                _ => d.inputs[i].truncate(d.inputs[i].len() / 2),
            }
            let (a, b) = (whole(&d.inputs, size), part(&d, size));
            assert_eq!(a.is_ok(), b.is_ok(), "input {i} edit {edit}: {a:?} {b:?}");
            if let (Ok(a), Ok(b)) = (a, b) {
                assert!(a == b, "input {i} edit {edit}");
            }
        }
    }
    // a bit of the joined stream flipped in turn: the same, whether it fails or not
    for k in 0..24 {
        let mut d = Case { inputs: c.inputs.clone(), want: Vec::new() };
        let at = d.inputs[0].len() * k / 24;
        d.inputs[0][at] ^= 1 << (k % 8);
        let (a, b) = (whole(&d.inputs, size), part(&d, size));
        assert_eq!(a.is_ok(), b.is_ok(), "flip {k}: {a:?} {b:?}");
        if let (Ok(a), Ok(b)) = (a, b) {
            assert!(a == b, "flip {k}");
        }
    }
    assert!(emu::apply(&ENTRY, &[&c.inputs[0], &c.inputs[1], &c.inputs[2], &c.inputs[3]], &[], &[1], &[size]).is_err());
}

/// An MSZIP block of stored DEFLATE blocks of `sizes` bytes each (the last final): a block
/// spanning as much of the joined stream as it takes.
fn stored_run(sizes: &[usize], fill: u8) -> Vec<u8> {
    let mut b = b"CK".to_vec();
    for (i, &n) in sizes.iter().enumerate() {
        b.push((i + 1 == sizes.len()) as u8);
        b.extend((n as u16).to_le_bytes());
        b.extend((!(n as u16)).to_le_bytes());
        b.extend(std::iter::repeat_n(fill, n));
    }
    b
}

/// A case of the MSZIP blocks `blocks` in one folder, nothing kept.
fn case_of(blocks: &[Vec<u8>]) -> Case {
    let refs: Vec<&[u8]> = blocks.iter().map(|b| &b[..]).collect();
    let mut w = BitW::new(Vec::new());
    let joined = mszip::join(&refs, &mut w).unwrap();
    let entries: Vec<Block> = joined.iter().map(|j| Block::Mszip { kept: 0, blocks: j.blocks }).collect();
    Case { inputs: [w.out, vec![], vec![], module::index(&entries)], want: blocks.concat() }
}

/// A block spanning the joined stream beyond what the part module holds (`SPAN`, `JOINED`) is
/// refused by it, and where it is within, in whatever place of the buffer it lies, the output is
/// the whole decoding's.
#[test]
fn part_near_the_buffer() {
    for lead in [0usize, 1, 1000, 40_000, 70_000] {
        for span in [60_000usize, 65_000, 96 << 10, 100_000, 131_000, 150_000, (160 << 10) - 100] {
            let first = if lead > 0 { vec![stored_run(&[lead.min(60_000)], 1)] } else { vec![] };
            let sizes: Vec<usize> =
                (0..span.div_ceil(65_535)).map(|i| span.min(65_535 * (i + 1)) - 65_535 * i).collect();
            let mut blocks = first;
            blocks.push(stored_run(&sizes, 2));
            blocks.push(stored_run(&[10], 3));
            let c = case_of(&blocks);
            let size = c.want.len() as u64;
            assert!(whole(&c.inputs, size).unwrap() == c.want, "whole {lead} {span}");
            // (a CAB's block spans 78639 bytes of the stream at most: what is within is exact, what
            // is past may be refused, but never misread)
            match part(&c, size) {
                Ok(o) => assert!(o == c.want, "part {lead} {span}"),
                Err(e) => assert!(span > 78_639, "part {lead} {span}: {e}"),
            }
        }
    }
    // far past the buffer: refused, not misread
    let c = case_of(&[stored_run(&[65_535, 65_535, 65_535, 65_535], 5), stored_run(&[10], 3)]);
    let size = c.want.len() as u64;
    assert!(whole(&c.inputs, size).is_ok());
    if let Ok(o) = part(&c, size) {
        assert!(o == c.want);
    }
}

/// Inputs of length 0 with an index asking for blocks, and the reverse: errors, not panics.
#[test]
fn part_empty_inputs() {
    let index = module::index(&[Block::Mszip { kept: 0, blocks: 1 }]);
    for size in [0, 5] {
        let c = Case { inputs: [vec![], vec![], vec![], index.clone()], want: vec![] };
        assert!(whole(&c.inputs, size).is_err());
        assert!(part(&c, size).is_err());
    }
    let c = Case { inputs: [vec![], vec![], vec![], vec![]], want: vec![] };
    assert!(whole(&c.inputs, 0).unwrap().is_empty());
    assert!(part(&c, 0).unwrap().is_empty());
    // an index of many blocks over a stream of few
    let many = module::index(&vec![Block::Mszip { kept: 0, blocks: 1 }; 1000]);
    let mut c = case_of(&[stored_run(&[10], 1)]);
    c.inputs[3] = many;
    assert!(whole(&c.inputs, 2000).is_err());
    assert!(part(&c, 2000).is_err());
}
