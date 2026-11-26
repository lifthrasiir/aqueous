# aqr-bvx

Apple's LZFSE and LZVN streams (blocks of magic `bvx2`, `bvxn` and `bvx-`, ending with `bvx$`)
split into their data and a small record, and rebuilt bit for bit. The archive then stores the
data, which aqr-compress recompresses in turn and which other files may share. LZVN comes
along with LZFSE: libcompression's LZFSE streams of small data are LZVN blocks.

No compressor is reproduced: any stream has a record, small for libcompression's. It was written from
what libcompression outputs and accepts, used as a black box (no published specification exists).
`FORMAT.md` specifies the streams, the record, the module `bvx-recons`, and how it works.

```sh
cargo run --release -p aqr-bvx --example aqr-bvx-check -- FILE_OR_DIR...   # .lzfse files: split, rebuilt, what the records cost
python3 -I codecs/aqr-bvx/scripts/gen-fixtures.py                          # tests/data from libcompression (macOS)
```

```rust
use aqr_bvx::formula;
// None where `file` does not start with a stream of bvx blocks, or it does not split.
let f = formula::encode(&file, 1).unwrap();
// `f` rebuilds `file` when evaluated with `formula::decode` as the module `formula::MODULE`.

use aqr_bvx::recons;
let sp = recons::split(&stream, limit)?;
let mut out = Vec::new();
recons::join_onto(&sp.data, &sp.record, &sp.counts, &mut out)?;
assert_eq!(out, stream[..sp.used]);
```

## What is taken

- Asset catalogs (`Assets.car`): renditions of images are LZFSE or LZVN streams, one per image (app
  icons are most). aqr-compress finds them in the catalog by a scan, at level 9 only (the data is many
  times the streams and taking them is slow), and takes a stream only where its data fits in what is
  left of the DEFLATE streams' expansion budget (`Options::expansion`: 32 times the file and 64 MiB).
- `compression_tool`, `.lzfse` files, and LZFSE in other data found by the scan.
- DMG (ULFO): each chunk an LZFSE stream, taken by aqr-compress's `dmg` at every level (records 0.8%
  of the streams).
- Not taken: APFS and HFS+ transparent compression (decmpfs), Apple Archive's (`.aar`) blocks other
  than whole streams, raw LZVN streams (the kernel's), LZFSE's first block version (`bvx1`).

## Results

Apple M4, 2026-10-30. The 36 asset catalogs over 100 KB of `/Applications` (272.5 MB), holding 4,781
LZFSE streams (236 MB, 3.7 GB of data) and 10,200 LZVN streams (10.4 MB), `aqr -c` on all threads:

| | archive | time |
|---|---|---|
| `zstd -3` | 232.63 MB | |
| level mid (streams not taken) | 219.06 MB | 1.2 s |
| level 8, streams taken | 186.53 MB | 34 s |
| level 9, streams taken | 170.57 MB (−22% of mid) | 74 s |

Testing takes 6.3 s (8.0 s with the modules as Wasm). The data of some catalogs (photos and
gradients, not icons with borders) compresses worse with zstd than LZFSE did, and those streams are
kept as they are.

Records, a sample of the same catalogs (756 streams, `aqr-bvx-check`; every stream rebuilt):

| streams | streams MB | data MB | records KB | of the streams |
|---|---|---|---|---|
| LZFSE, 213 (18 of them of libcompression on text and code) | 6.02 | 77.66 | 90.5 | 1.50% |
| LZVN, 543 (13 of them likewise) | 0.57 | 0.86 | 14.1 | 2.49% |

`compression_tool`'s streams (of 73 KB blocks) cost 1.16%. Rebuilding runs at about 170 MB/s of data
on one thread; splitting a DMG's chunks about 37 MB/s. The module `bvx-recons` is 78 KB of Wasm (a
block of 20 KB in an archive). Of the data, zstd -9 makes 4.62 MB (5.07 MB at -3) of the sample's 5.04 MB
of LZFSE streams, so the gain is a few percent where the data is not recompressed further.

## Tests

```sh
cargo rt -p aqr-bvx         # fixtures of libcompression, random parses and blocks, damaged records
```

`cargo test` (debug) catches an overflow from a damaged record as a panic. aqr-compress
`tests/compress.rs` (`bvx_streams`) takes the fixtures alone and found in other data.

## Left

- **Containers.** decmpfs of APFS and HFS+ (a resource fork or an extended attribute, raw LZVN or LZFSE
  chunks behind a table), Apple Archive's blocks, and raw LZVN streams (the kernel's).
- **`bvx1`.** LZFSE's first block version is not taken.
- **LZVN's compressor.** LZVN segments use the generic match finder alone, without a model of its own.
- **Match starts in LZFSE.** 85% of the records is whether a match starts at a position; it is
  modelled only through the lengths there and at the next position (a match of 4 bytes with nothing
  after it is taken half the time).
- **Image model.** The data is pixels of 4 bytes, alpha last, the transparent ones zeros: a model of
  images on it is where the rest of the gain is (`docs/macos-app-todo.md` section 4).
