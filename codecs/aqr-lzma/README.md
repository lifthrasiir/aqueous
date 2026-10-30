# aqr-lzma

LZMA streams (LZMA1: `.lzma` files, 7z folders) and LZMA2 streams (`.xz` files, 7z folders)
split into their data and a small record of the parse, and rebuilt bit for bit. Meant for archives that hold such streams whose data is
also elsewhere (a `.tar.lzma` of files installed beside it, a 7z of a tree), so that the data
is stored once and the stream costs only its record. No encoder is reproduced: the record
codes the packets given the data, so any encoder's stream has one, and those of encoders
with a binary-tree match finder and an optimal parser (XZ Utils, 7-Zip) are small. The
record's format is in [FORMAT.md](FORMAT.md).

```sh
cargo build --release
aqr-lzma bench [-t THREADS] [-v] FILE...    # .lzma or .xz files, or raw LZMA streams as FILE@OFFSET+LENGTH:PROPS:DICT:SIZE
```

```rust
use aqr_lzma::formula;
// None where `file` is not a `.lzma` file, or its stream does not decode.
let f = formula::encode(&file, 1 /* threads */).unwrap();
// `f` rebuilds `file` when evaluated with `formula::decode` as the module `formula::MODULE`.

use aqr_lzma::recons;
let sp = recons::split(&stream, props, dict, size, limit)?; // size: None where it ends with the end marker
assert_eq!(recons::join(&sp.data, &sp.record).unwrap(), stream[..sp.used]);

use aqr_lzma::lzma2;
// a split for each segment (from a chunk resetting the dictionary), each rebuilt on its own
let segs = lzma2::split(&stream, dict, limit, threads)?;
let f = formula::encode_xz(&file, threads).unwrap(); // an .xz file: `formula::LZMA2` on its blocks
```

## Results

Measured on 2026-10-27 on an Apple M4 (4 performance and 6 efficiency cores), with another
build running beside (load 2–5). Speeds are of the data, per thread, with 4 threads (the
performance cores); with 10, the per-thread figures halve and the wall time stays.

| streams | streams MB | data MB | records MB | of the streams | split | rebuild |
|---|---|---|---|---|---|---|
| the 123 `.tar.lzma` of the 10gb benchmark (XZ Utils `-6`, 8 MiB dictionary) | 73.08 | 428.6 | 9.84 | 13.5% | 15 MB/s | 13 MB/s |
| folder 0 of its `mingw480/*.7z` (7-Zip, 64 MiB dictionary) | 23.43 | 249.4 | 4.31 | 18.4% | 20 MB/s | 19 MB/s |
| its 6 `.tar.xz` (LZMA2; the records with the rest of the files) | 0.465 | 1.331 | 0.047 | 10.1% | 15 MB/s | 17 MB/s |
| a tar of its `mingw480/libexec` (58.0 MB), XZ Utils `-6 -T1` (one block) | 19.29 | 58.0 | 3.07 | 15.9% | 10 MB/s | 9 MB/s |
| the same, `-6 -T4` (blocks of 24 MiB) | 19.49 | 58.0 | 3.03 | 15.5% | 10 MB/s | 9 MB/s |

The `.xz` rows were measured on 2026-10-30 likewise, with 2 threads.

- Decoding and encoding the packets alone run at 110–230 MB/s. The rest is the match finder,
  bound by its cache misses (three chains and a hash of 3 bytes, a few per byte).
- The record is mostly the packet types (36%) and the lengths of matches (42%); distances are
  16%, as a match's distance is almost always the nearest one matching for its length. Deeper
  search lowers lengths and distances, slowly: 16 candidates per chain rather than 4 make the
  records about 6% smaller on the `.tar.lzma`.
- Every member of those `.tar.lzma` (421 MB) and the data of that 7z folder are files of the
  benchmark, so with the data shared the streams cost about 14 MB instead of 96.5 MB.
- Memory: the data, the stream and `12 × S` bytes of chains, `S` the dictionary size rounded
  up to a power of 2 (at most the data's), and 12.5 MiB. The bench on the 7z folder (data,
  packets, stream and record held at once) peaked at 2.1 GB.

## Tests

```sh
cargo rt                           # fixtures of XZ Utils, streams without end marker, damaged records
python3 -I scripts/gen-fixtures.py # rewrites tests/data (needs xz)
```

The tests of damaged and random records catch an overflow only in a debug build
(`cargo test`, about a minute), where it panics; in release it gives a record that fails.

## Status

The module `lzma-recons` (natively and as a Wasm module of aqr-modules) and `.lzma` files in
aqr-compress are done: in an archive of several files, a `.lzma` file is put in a batch with
the files its stream holds (the members of a tar archive, or the whole), so that their data is
stored once. So are the module `lzma2-recons` and `.xz` files (their blocks of LZMA2 alone),
which aqr-compress takes alike, anywhere in data too. 7z archives are aqr-7z's, on these modules.
Still to come: `.xz` blocks of other filters (BCJ, delta), and 7z's BCJ2, which the other folder
of that 7z goes through.
