use aqr_bundle::packs::sony_psarc::{files, starts};
use aqr_disk::Compressed;

fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut v = vec![0x78, 0x01, 1];
    v.extend_from_slice(&(data.len() as u16).to_le_bytes());
    v.extend_from_slice(&(!(data.len() as u16)).to_le_bytes());
    v.extend_from_slice(data);
    v.extend_from_slice(&[0; 4]);
    v
}

/// A file: the original size and its blocks (as stored in the archive).
struct F {
    size: u64,
    blocks: Vec<Vec<u8>>,
}

fn stored(data: &[u8], block: usize) -> F {
    F { size: data.len() as u64, blocks: data.chunks(block).map(|c| c.to_vec()).collect() }
}

fn pack(method: &[u8; 4], block: usize, files: &[F]) -> Vec<u8> {
    let width = if block <= 0x10000 { 2 } else { 3 };
    let count = files.len();
    let nblocks: usize = files.iter().map(|f| f.blocks.len()).sum();
    let toc = 32 + count * 30 + nblocks * width;
    let mut v = b"PSAR".to_vec();
    v.extend_from_slice(&[0, 1, 0, 4]);
    v.extend_from_slice(method);
    for x in [toc as u32, 30, count as u32, block as u32, 0] {
        v.extend_from_slice(&x.to_be_bytes());
    }
    let (mut first, mut at) = (0u32, toc as u64);
    let mut table = Vec::new();
    for f in files {
        v.extend_from_slice(&[0; 16]);
        v.extend_from_slice(&first.to_be_bytes());
        v.extend_from_slice(&f.size.to_be_bytes()[3..]);
        v.extend_from_slice(&at.to_be_bytes()[3..]);
        first += f.blocks.len() as u32;
        for b in &f.blocks {
            let s = if b.len() == 65536 { 0 } else { b.len() };
            table.extend_from_slice(&(s as u32).to_be_bytes()[4 - width..]);
            at += b.len() as u64;
        }
    }
    v.extend_from_slice(&table);
    assert_eq!(v.len(), toc);
    for f in files {
        for b in &f.blocks {
            v.extend_from_slice(b);
        }
    }
    v
}

#[test]
fn variants() {
    let big: Vec<u8> = (0..300).map(|i| i as u8).collect();
    let z = zlib_stored(b"hello");
    for method in [b"zlib", b"lzma"] {
        for block in [128, 65536] {
            let manifest = b"/dir/a.bin\r\nb.txt\nc.dat\nd.dat\n";
            let m = if method == b"zlib" {
                F { size: manifest.len() as u64, blocks: vec![zlib_stored(manifest)] }
            } else {
                stored(manifest, block)
            };
            let fs = [
                m,
                stored(&big, block),
                F { size: 5, blocks: vec![z.clone()] },
                F {
                    size: 200,
                    blocks: if block == 128 { vec![vec![0xaa; 100], vec![0xbb; 100]] } else { vec![vec![0xaa; 150]] },
                },
                F { size: 0, blocks: vec![] },
            ];
            let d = pack(method, block, &fs);
            assert!(starts(&d));
            let f = files(&d).unwrap_or_else(|| panic!("{method:?} {block}"));
            let names: Vec<_> = f.iter().map(|f| String::from_utf8_lossy(&f.name).into_owned()).collect();
            assert_eq!(names[0], "dir/a.bin");
            assert_eq!(&d[f[0].extents[0].clone()], &big[..]);
            assert!(f[0].compressed.is_none());
            // The compressed single block: zlib plain; lzma Opaque.
            assert_eq!(&d[f[1].extents[0].clone()], &z[..]);
            let lz = method == b"lzma";
            assert_eq!(f[1].compressed.is_some(), lz);
            if lz {
                assert!(matches!(f[1].compressed, Some(Compressed::Opaque { len: 5 })));
            }
            // 200 bytes in compressed blocks: several blocks of zlib are left out.
            let rest = f.len() - 2;
            assert_eq!(rest, if lz || block != 128 { 1 } else { 0 }, "{method:?} {block}");
        }
    }
}

#[test]
fn cut_and_not_a_pack() {
    let fs = [stored(b"a.txt\nb\n", 16), stored(b"0123456789abcdefghij", 16), stored(b"xy", 16)];
    let d = pack(b"zlib", 16, &fs);
    for n in 0..d.len() {
        if let Some(f) = files(&d[..n]) {
            for file in f {
                assert!(file.extents.iter().all(|e| e.end <= n));
            }
        }
    }
    let f = files(&d).unwrap();
    assert_eq!(f[0].name, b"a.txt");
    assert_eq!(&d[f[0].extents[0].clone()], b"0123456789abcdefghij");
    for i in 0..d.len() {
        for v in [0u8, 1, 0x7f, 0xff] {
            let mut m = d.clone();
            m[i] = v;
            let _ = files(&m);
        }
    }
    assert!(!starts(b"junk"));
    assert!(files(b"junk").is_none());
}
