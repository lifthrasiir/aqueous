# aqr-bvx

Apple's LZFSE and LZVN streams (blocks of magic `bvx2`, `bvxn` and `bvx-`, ending with `bvx$`)
split into their data and a small record, and rebuilt bit for bit. The archive then stores the
data, which aqr-compress recompresses in turn and which other files may share. LZVN comes
along with LZFSE: libcompression's LZFSE streams of small data are LZVN blocks.

No compressor is reproduced. The record codes the stream's parse given the data, under
contexts of what a match finder sees there; in LZFSE blocks that includes the compressor's own
table of candidates (a 14-bit hash of 4 bytes, buckets of 4), from which it nearly always takes
its matches. Everything else (frequencies, coder states, padding, how runs and matches are cut
into triples and instructions) is what the compressor does, which the record only notes where
a stream differs. There is no published specification of either format: this crate was
written from what libcompression outputs and accepts, used as a black box. The streams and
the record are in [FORMAT.md](FORMAT.md).

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
recons::join_onto(&sp.data, &sp.record, &mut out)?;
assert_eq!(out, stream[..sp.used]);
```

## Where it is

- Asset catalogs (`Assets.car`): renditions of images are LZFSE or LZVN streams, one per
  image (app icons are most of them). aqr-compress finds them in the catalog by a scan, and
  takes a stream only where its data fits in what is left of the budget that DEFLATE streams
  inflate within (`Options::expansion`: 32 times the file and 64 MiB).
- `compression_tool`, `.lzfse` files, and LZFSE in other data found by the scan.
- Not taken yet: APFS and HFS+ transparent compression (decmpfs: in a resource fork or an
  extended attribute, raw LZVN or LZFSE chunks behind a table), DMG (ULFO, chunks of LZFSE
  behind a block table), Apple Archive (`.aar`; its LZFSE blocks are found by the scan where
  they are whole streams), and raw LZVN streams (the kernel's).

## Results

Measured on 2026-10-30 on an Apple M4.

The 36 asset catalogs over 100 KB of `/Applications` (272.5 MB; the seven apps of
`docs/macos-app-todo.md` and others), holding 4,781 LZFSE streams (236 MB, 3.7 GB of data) and
10,200 LZVN streams (10.4 MB): `aqr -c` (level mid, all threads) gives 191.11 MB, against
220.47 MB before (−13.3%) and 232.63 MB by `zstd -3`. Compressing took 55.5 s (1.7 s before:
the data is 14 times the catalogs) with a peak of 7.8 GB; testing (`aqr -t`) 6.3 s, and 8.0 s
with the modules as Wasm. The data of some catalogs (photos and gradients, not icons with
borders) compresses worse with zstd than LZFSE did, and those streams are kept as they are.

A sample of the same catalogs (corpus/, 756 streams, through `aqr-bvx-check`; every stream
rebuilt):

| streams | streams MB | data MB | records KB | of the streams |
|---|---|---|---|---|
| LZFSE, 213 (18 of them of libcompression on text and code) | 6.02 | 77.66 | 90.5 | 1.50% |
| LZVN, 543 (13 of them likewise) | 0.57 | 0.86 | 14.1 | 2.49% |

- In LZFSE records, 85% is whether a match starts at a position. The compressor's choice
  between its table's best match there and one at the next position is modelled only through
  the lengths of both (a match of 4 bytes with nothing after it is taken half the time).
- Rebuilding runs at about 170 MB/s of data on one thread, most of it the walk over the data.
  `compression_tool`'s streams (of 73 KB blocks) cost 1.16%.
- Of the data, zstd -9 makes 4.62 MB (5.07 MB at -3) of the 5.04 MB of the LZFSE streams of the
  sample, so the gain is a few percent where the data is not recompressed further. The data is
  pixels of 4 bytes, alpha last, the transparent ones zeros: an image model on it is where the
  rest is (`docs/macos-app-todo.md` §4).

The module `bvx-recons` is 77 KB of Wasm (a block of 21 KB in an archive).

## Tests

```sh
cargo rt -p aqr-bvx         # fixtures of libcompression, random parses and blocks, damaged records
```

`cargo test` (debug) catches an overflow from a damaged record as a panic. aqr-compress
`tests/compress.rs` (`bvx_streams`) takes the fixtures alone and found in other data.

## Status

The module `bvx-recons` (natively and as a Wasm module of aqr-modules) and streams in
aqr-compress (at the start of a file or found in other data) are done.

Not yet: the containers above; LZFSE's first block version (`bvx1`); a model of LZVN's
compressor of its own (LZVN segments use the generic match finder alone); the start of
matches in LZFSE beyond the lengths at a position and the next.
