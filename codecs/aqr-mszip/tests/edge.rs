//! Edge cases of CAB structure (PREV/NEXT, signatures, trailing bytes, odd block lists) and of
//! MSZIP's `join` and the module's refusals, on cabinets and streams built here.

use aqr_archive::Evaluate;
use aqr_decompress::Native;
use aqr_mszip::mszip::{self, BitW};
use aqr_mszip::{cab, formula, module};

/// A folder: its type, and its data blocks (the bytes after `cbData`'s header, and `cbUncomp`).
struct Folder {
    type_compress: u16,
    blocks: Vec<(Vec<u8>, u16)>,
}

#[derive(Default)]
struct Builder {
    flags: u16,
    /// The header's reserved field (with `RESERVE` set in the flags).
    reserve: Option<Vec<u8>>,
    folders: Vec<Folder>,
    /// Name, folder index, offset in the folder, size.
    files: Vec<(&'static str, u16, u32, u32)>,
    /// The bytes after the cabinet.
    trailer: Vec<u8>,
    /// Folders all starting where the first one's data does.
    overlap: bool,
}

/// A data block of a stored DEFLATE block of `data` (MSZIP's), `CK` first.
fn mszip_block(data: &[u8]) -> (Vec<u8>, u16) {
    let mut b = b"CK".to_vec();
    b.push(1);
    b.extend((data.len() as u16).to_le_bytes());
    b.extend((!(data.len() as u16)).to_le_bytes());
    b.extend(data);
    (b, data.len() as u16)
}

fn stored(data: &[u8]) -> (Vec<u8>, u16) {
    (data.to_vec(), data.len() as u16)
}

impl Builder {
    fn build(&self) -> Vec<u8> {
        let mut head = 36;
        if let Some(r) = &self.reserve {
            head += 4 + r.len();
        }
        if self.flags & 1 != 0 {
            head += 4;
        }
        if self.flags & 2 != 0 {
            head += 4;
        }
        let files_at = head + 8 * self.folders.len();
        let files_len: usize = self.files.iter().map(|f| 16 + f.0.len() + 1).sum();
        let data_at = files_at + files_len;
        let mut folder_table = Vec::new();
        let mut data = Vec::new();
        for (i, f) in self.folders.iter().enumerate() {
            let start = if self.overlap { data_at } else { data_at + data.len() };
            if !self.overlap || i == 0 {
                for (p, u) in &f.blocks {
                    data.extend(0u32.to_le_bytes());
                    data.extend((p.len() as u16).to_le_bytes());
                    data.extend(u.to_le_bytes());
                    data.extend(p);
                }
            }
            folder_table.extend((start as u32).to_le_bytes());
            folder_table.extend((f.blocks.len() as u16).to_le_bytes());
            folder_table.extend(f.type_compress.to_le_bytes());
        }
        let size = data_at + data.len();
        let mut d = b"MSCF".to_vec();
        d.extend(0u32.to_le_bytes());
        d.extend((size as u32).to_le_bytes());
        d.extend(0u32.to_le_bytes());
        d.extend((files_at as u32).to_le_bytes());
        d.extend(0u32.to_le_bytes());
        d.extend([3, 1]);
        d.extend((self.folders.len() as u16).to_le_bytes());
        d.extend((self.files.len() as u16).to_le_bytes());
        let flags = self.flags | if self.reserve.is_some() { 4 } else { 0 };
        d.extend(flags.to_le_bytes());
        d.extend(0u32.to_le_bytes()); // setID, iCabinet
        if let Some(r) = &self.reserve {
            d.extend((r.len() as u16).to_le_bytes());
            d.extend([0, 0]);
            d.extend(r);
        }
        if self.flags & 1 != 0 {
            d.extend(b"a\0b\0");
        }
        if self.flags & 2 != 0 {
            d.extend(b"c\0d\0");
        }
        assert_eq!(d.len(), head);
        d.extend(folder_table);
        for (name, folder, offset, size) in &self.files {
            d.extend(size.to_le_bytes());
            d.extend(offset.to_le_bytes());
            d.extend(folder.to_le_bytes());
            d.extend([0; 6]);
            d.extend(name.as_bytes());
            d.push(0);
        }
        assert_eq!(d.len(), data_at);
        d.extend(data);
        d.extend(&self.trailer);
        d
    }
}

fn eval(f: &aqr_formula::Formula) -> Vec<u8> {
    f.eval(&Native::default()).unwrap()
}

/// Whether `d` has a formula; where it has, it gives `d` back.
fn round_trips(d: &[u8]) -> bool {
    match formula::encode(d, 1) {
        Some(f) => {
            assert!(eval(&f) == d);
            true
        }
        None => false,
    }
}

fn one_folder(type_compress: u16, blocks: Vec<(Vec<u8>, u16)>) -> Builder {
    let n: u32 = blocks.iter().map(|b| b.1 as u32).sum();
    Builder { folders: vec![Folder { type_compress, blocks }], files: vec![("f.txt", 0, 0, n)], ..Default::default() }
}

#[test]
fn builder_is_a_cab() {
    let d = one_folder(1, vec![mszip_block(b"hello world")]).build();
    let c = cab::parse(&d).unwrap();
    assert_eq!(c.folders[0].size(), 11);
    assert_eq!(c.extent(), d.len());
    assert!(round_trips(&d));
    let d = one_folder(0, vec![stored(b"hello world")]).build();
    assert!(round_trips(&d));
}

/// A folder continued from the cabinet before or into the one after is kept as it is; the
/// others of the cabinet are taken.
#[test]
fn prev_and_next() {
    for flags in [1, 2, 3] {
        let b = Builder {
            flags,
            folders: vec![
                Folder { type_compress: 0, blocks: vec![stored(b"first folder")] },
                Folder { type_compress: 0, blocks: vec![stored(b"middle folder")] },
                Folder { type_compress: 0, blocks: vec![stored(b"last folder")] },
            ],
            files: vec![("a", 0, 0, 12), ("b", 1, 0, 13), ("c", 2, 0, 11)],
            ..Default::default()
        };
        let d = b.build();
        let c = cab::parse(&d).unwrap();
        assert_eq!(c.continued(0), flags & 1 != 0);
        assert!(!c.continued(1));
        assert_eq!(c.continued(2), flags & 2 != 0);
        let f = formula::encode(&d, 1).unwrap();
        assert!(eval(&f) == d, "{flags}");
        // the data taken is the folders not continued
        let taken: usize = f.streams.iter().filter(|s| s.class == "raw").map(|s| s.data.len()).sum::<usize>();
        let want = 13 + if flags & 1 == 0 { 12 } else { 0 } + if flags & 2 == 0 { 11 } else { 0 };
        assert_eq!(taken, want, "{flags}");
    }
    // the only folder, continued both ways: nothing to take
    let mut b = one_folder(0, vec![stored(b"data")]);
    b.flags = 3;
    assert!(!round_trips(&b.build()));
}

/// Files of the continued-folder `iFolder` values.
#[test]
fn continued_file_indexes() {
    let b = Builder {
        flags: 3,
        folders: vec![
            Folder { type_compress: 0, blocks: vec![stored(b"abc")] },
            Folder { type_compress: 0, blocks: vec![stored(b"def")] },
        ],
        files: vec![("a", 0xfffd, 0, 3), ("b", 0xfffe, 0, 3), ("c", 0xffff, 0, 3), ("d", 7, 0, 3)],
        ..Default::default()
    };
    let c = cab::parse(&b.build()).unwrap();
    let names = |i| c.files_of(i).map(|f| f.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(0), [b"a".to_vec()]);
    assert_eq!(names(1), [b"b".to_vec(), b"c".to_vec()]);
}

fn signature(at: u32, n: u32) -> Vec<u8> {
    let mut r = 0x0010_0000u32.to_le_bytes().to_vec();
    r.extend(at.to_le_bytes());
    r.extend(n.to_le_bytes());
    r.extend([0; 8]);
    r
}

/// Authenticode's reserve and signature after the cabinet: the extent covers it and the formula
/// keeps it; a signature inside the cabinet is none, an overflowing one no panic.
#[test]
fn signature_extent() {
    let mut b = one_folder(0, vec![stored(b"signed data")]);
    b.reserve = Some(signature(0, 0));
    let size = b.build().len();
    // after the cabinet, 40 bytes of signature
    b.reserve = Some(signature(size as u32 + 10, 40));
    b.trailer = vec![0xaa; 60];
    let d = b.build();
    let c = cab::parse(&d).unwrap();
    assert_eq!(c.size, d.len() - 60);
    assert_eq!(c.signature, Some(c.size + 10..c.size + 50));
    assert_eq!(c.extent(), c.size + 50);
    assert_eq!(formula::extent(&d), Some(c.size + 50));
    assert!(round_trips(&d));
    // inside the cabinet: no signature
    b.reserve = Some(signature(10, 40));
    let c = cab::parse(&b.build()).unwrap();
    assert_eq!(c.signature, None);
    assert_eq!(c.extent(), c.size);
    // overflowing offset and size: no panic, and an extent of at least the cabinet
    b.reserve = Some(signature(u32::MAX, u32::MAX));
    let d = b.build();
    let c = cab::parse(&d).unwrap();
    assert!(c.extent() >= c.size);
    assert!(round_trips(&d));
    // a signature of no bytes at the very end of the cabinet
    b.reserve = Some(signature(size as u32, 0));
    let c = cab::parse(&b.build()).unwrap();
    assert!(c.extent() >= c.size);
}

/// Bytes after the cabinet are no part of it: the extent is `cbCabinet`, the formula keeps them.
#[test]
fn trailing_bytes() {
    let mut b = one_folder(1, vec![mszip_block(b"some data")]);
    let plain = b.build();
    b.trailer = b"trailing junk".to_vec();
    let d = b.build();
    assert_eq!(cab::parse(&d).unwrap().extent(), plain.len());
    assert_eq!(formula::extent(&d), Some(plain.len()));
    assert!(round_trips(&d));
}

/// Folders without blocks, blocks of no data, stored blocks whose sizes differ: kept, not taken.
#[test]
fn odd_block_lists() {
    assert!(!round_trips(&one_folder(1, vec![]).build()));
    assert!(!round_trips(&one_folder(0, vec![]).build()));
    // a block of no data, MSZIP and stored
    assert!(!round_trips(&one_folder(1, vec![mszip_block(b"abc"), mszip_block(b"")]).build()));
    assert!(!round_trips(&one_folder(0, vec![stored(b"abc"), stored(b"")]).build()));
    // an empty folder next to a good one: the good one is taken
    let mut b = Builder {
        folders: vec![
            Folder { type_compress: 0, blocks: vec![] },
            Folder { type_compress: 0, blocks: vec![stored(b"good folder")] },
        ],
        files: vec![("a", 1, 0, 11)],
        ..Default::default()
    };
    assert!(round_trips(&b.build()));
    // stored folders whose compressed length differs from the data's
    b.folders = vec![Folder { type_compress: 0, blocks: vec![(b"abcdef".to_vec(), 4)] }];
    assert!(!round_trips(&b.build()));
    b.folders = vec![Folder { type_compress: 0, blocks: vec![(b"abc".to_vec(), 9)] }];
    assert!(!round_trips(&b.build()));
    // MSZIP whose blocks hold other data than they say
    let (p, _) = mszip_block(b"abcdef");
    b.folders = vec![Folder { type_compress: 1, blocks: vec![(p.clone(), 5)] }];
    assert!(!round_trips(&b.build()));
    b.folders = vec![Folder { type_compress: 1, blocks: vec![(p, 7)] }];
    assert!(!round_trips(&b.build()));
}

/// Folders whose blocks overlap (a second folder starting at the first's data), or come in
/// another order than in the file: not both taken, and the file given back.
#[test]
fn overlapping_folders() {
    let b = Builder {
        overlap: true,
        folders: vec![
            Folder { type_compress: 0, blocks: vec![stored(b"shared data")] },
            Folder { type_compress: 0, blocks: vec![stored(b"shared data")] },
        ],
        files: vec![("a", 0, 0, 11), ("b", 1, 0, 11)],
        ..Default::default()
    };
    let d = b.build();
    let c = cab::parse(&d).unwrap();
    assert_eq!(c.folders[0].span(), c.folders[1].span());
    let f = formula::encode(&d, 1).unwrap();
    assert!(eval(&f) == d);
    let taken: usize = f.streams.iter().filter(|s| s.class == "raw").map(|s| s.data.len()).sum();
    assert_eq!(taken, 11);
    // the second folder's span before the first's
    let mut d = Builder {
        folders: vec![
            Folder { type_compress: 0, blocks: vec![stored(b"first")] },
            Folder { type_compress: 0, blocks: vec![stored(b"second")] },
        ],
        files: vec![("a", 0, 0, 5), ("b", 1, 0, 6)],
        ..Default::default()
    }
    .build();
    let n = 36 + 8;
    let (s0, s1) = (d[36..40].to_vec(), d[n..n + 4].to_vec());
    d[36..40].copy_from_slice(&s1);
    d[n..n + 4].copy_from_slice(&s0);
    assert!(round_trips(&d));
}

/// A folder's data blocks reaching past the file's end are cut there; no panic.
#[test]
fn cut_everywhere() {
    let d = one_folder(0, vec![stored(b"abcdefgh"), stored(b"ijkl")]).build();
    for cut in 0..d.len() {
        round_trips(&d[..cut]);
    }
}

/// Every damage of a small cabinet: no panic, and a formula gives the input back.
#[test]
fn damaged_small() {
    let d = one_folder(1, vec![mszip_block(b"hello, damaged world"), mszip_block(b"second")]).build();
    aqr_testkit::damaged(&d, 64, 128, std::time::Duration::from_secs(20), |v| {
        round_trips(v);
    });
}

// MSZIP's join.

fn joined(blocks: &[&[u8]]) -> aqr_mszip::Result<Vec<u8>> {
    let mut w = BitW::new(Vec::new());
    mszip::join(blocks, &mut w).map(|_| w.out)
}

fn stored_stream(data: &[u8], fin: bool) -> Vec<u8> {
    let mut b = vec![fin as u8];
    b.extend((data.len() as u16).to_le_bytes());
    b.extend((!(data.len() as u16)).to_le_bytes());
    b.extend(data);
    b
}

fn ck(stream: &[u8]) -> Vec<u8> {
    [b"CK".as_slice(), stream].concat()
}

#[test]
fn join_rejections() {
    let good = ck(&stored_stream(b"abc", true));
    assert!(joined(&[&good]).is_ok());
    // no signature, or another
    assert!(joined(&[&good[2..]]).is_err());
    assert!(joined(&[&[b"CX".as_slice(), &good[2..]].concat()]).is_err());
    assert!(joined(&[b"C"]).is_err());
    assert!(joined(&[b""]).is_err());
    // bytes after the final block
    assert!(joined(&[&[good.clone(), vec![0]].concat()]).is_err());
    assert!(joined(&[&[good.clone(), good[2..].to_vec()].concat()]).is_err());
    // no final flag: the stream ends (cut short)
    assert!(joined(&[&ck(&stored_stream(b"abc", false))]).is_err());
    assert!(joined(&[&ck(&stored_stream(b"abc", false)), &good]).is_err());
    // padding not zero: a stored block's (the five bits after its header)
    let mut bad = good.clone();
    bad[2] |= 0x08;
    assert!(joined(&[&bad]).is_err());
    // a fixed final block of only its end: bits 1 (final) 10 (fixed) then 7 zeros
    let fixed = ck(&[0b0000_0011, 0b0000_0000]);
    assert!(joined(&[&fixed]).is_ok());
    // the padding after the stream's last bit set
    assert!(joined(&[&ck(&[0b0000_0011, 0b1000_0000])]).is_err());
    // block type 3
    assert!(joined(&[&ck(&[0b0000_0111, 0, 0])]).is_err());
    // a stored block with a wrong complement, or cut short
    let mut bad = good.clone();
    bad[6] ^= 1;
    assert!(joined(&[&bad]).is_err());
    assert!(joined(&[&good[..good.len() - 1]]).is_err());
    // no blocks at all joins to nothing
    assert!(joined(&[]).unwrap().is_empty());
}

/// Blocks cut from a joined stream: each ends where it should, and the stream's end is a final
/// flag of the last only.
#[test]
fn join_and_cut_stored() {
    let a = ck(&stored_stream(b"abc", true));
    let b = ck(&stored_stream(b"def", true));
    let j = joined(&[&a, &b]).unwrap();
    let mut r = mszip::BitR::new(&j);
    let mut out = Vec::new();
    mszip::cut_onto(&mut r, 1, &mut out).unwrap();
    assert_eq!(out, a);
    out.clear();
    mszip::cut_onto(&mut r, 1, &mut out).unwrap();
    assert_eq!(out, b);
    // more blocks than there are
    let mut r = mszip::BitR::new(&j);
    assert!(mszip::cut_onto(&mut r, 3, &mut Vec::new()).is_err());
    // none: a signature alone
    let mut r = mszip::BitR::new(&j);
    let mut out = Vec::new();
    mszip::cut_onto(&mut r, 0, &mut out).unwrap();
    assert_eq!(out, b"CK");
}

// The module's refusals.

fn decode(index: &[u8], joined: &[u8], raw: &[u8], kept: &[u8], len: u64) -> aqr_mszip::Result<Vec<u8>> {
    module::decode(&[], &[joined, raw, kept, index], len)
}

/// An index of more blocks than the streams hold, of lengths beyond them, of `len` 0.
#[test]
fn module_index_refusals() {
    use module::Block;
    let a = ck(&stored_stream(b"abc", true));
    let j = joined(&[&a]).unwrap();
    let one = module::index(&[Block::Mszip { kept: 0, blocks: 1 }]);
    assert_eq!(decode(&one, &j, &[], &[], a.len() as u64).unwrap(), a);
    // the output length wrong, 0 included
    assert!(decode(&one, &j, &[], &[], 0).is_err());
    assert!(decode(&one, &j, &[], &[], a.len() as u64 - 1).is_err());
    assert!(decode(&one, &j, &[], &[], a.len() as u64 + 1).is_err());
    // an index of more blocks than the stream holds
    let two = module::index(&[Block::Mszip { kept: 0, blocks: 1 }, Block::Mszip { kept: 0, blocks: 1 }]);
    assert!(decode(&two, &j, &[], &[], 2 * a.len() as u64).is_err());
    let many = module::index(&[Block::Mszip { kept: 0, blocks: 5 }]);
    assert!(decode(&many, &j, &[], &[], a.len() as u64).is_err());
    // kept or raw lengths beyond their inputs
    let kept = module::index(&[Block::Mszip { kept: 9, blocks: 1 }]);
    assert!(decode(&kept, &j, &[], &[0; 3], 100).is_err());
    let raw = module::index(&[Block::Raw { kept: 0, len: usize::MAX / 4 }]);
    assert!(decode(&raw, &j, &[], &[], 10).is_err());
    // an index cut in the middle of a varint, or a lone varint
    assert!(decode(&[0x80], &j, &[], &[], 0).is_err());
    assert!(decode(&[0], &j, &[], &[], 0).is_err());
    // nothing at all
    assert_eq!(decode(&[], &[], &[], &[], 0).unwrap(), b"");
    // a stream not used up
    assert!(decode(&[], &j, &[], &[], 0).is_err());
    // wrong arity and parameters
    assert!(module::decode(&[], &[&j], 0).is_err());
    assert!(module::decode(&[1], &[&[], &[], &[], &[]], 0).is_err());
}

/// Every input damaged in turn: no panic, no hang.
#[test]
fn module_damaged() {
    use module::Block;
    let a = ck(&stored_stream(b"abcdef", true));
    let j = joined(&[&a]).unwrap();
    let index = module::index(&[Block::Raw { kept: 2, len: 3 }, Block::Mszip { kept: 1, blocks: 1 }]);
    let (raw, kept) = (b"xyz".to_vec(), b"123".to_vec());
    let len = (2 + 3 + 1 + a.len()) as u64;
    let all = [j.clone(), raw.clone(), kept.clone(), index.clone()];
    assert!(decode(&index, &j, &raw, &kept, len).is_err() || decode(&index, &j, &raw, &kept, len).is_ok());
    for i in 0..4 {
        aqr_testkit::damaged(&all[i], 64, 64, std::time::Duration::from_secs(10), |v| {
            let mut ins = all.clone();
            ins[i] = v.to_vec();
            let _ = decode(&ins[3], &ins[0], &ins[1], &ins[2], len);
        });
    }
}
