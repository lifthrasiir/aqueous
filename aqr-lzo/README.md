# aqr-lzo

LZO1X streams (lzop files) split into their data and a small record of the parse, and rebuilt
bit for bit. The archive then stores the data, which zstd compresses better than the stream,
which aqr-compress recompresses in turn, and which other files may share.

No compressor is reproduced. LZO has many (LZO1X-1 and its 11-, 12- and 15-bit variants,
LZO1X-999 at nine levels, the Linux kernel's own), and LZO is GPL, so this crate holds none of
its code. The record codes any compressor's parse given the data, under contexts of what a
match finder sees there. It was written from the kernel's description of the stream format
(`Documentation/staging/lzo.rst`) and from what liblzo2 and lzop output, used as black boxes.
The record's format is in [FORMAT.md](FORMAT.md).

```sh
cargo build --release
aqr-lzo bench [-v] FILE...    # lzop files (their compressed blocks), or raw LZO1X streams
```

```rust
use aqr_lzo::formula;
// None where `file` is not an lzop file, or none of its streams splits.
let f = formula::encode(&file, 1 /* threads */).unwrap();
// `f` rebuilds `file` when evaluated with `formula::decode` as the module `formula::MODULE`.

use aqr_lzo::recons;
let sp = recons::split(&stream, limit)?;
assert_eq!(recons::join(&sp.data, &sp.record).unwrap(), stream[..sp.used]);
```

## Results

Measured on 2026-10-28 on an Apple M4, one thread, with liblzo2 2.10 and lzop 1.04 (Homebrew).
The Silesia corpus (12 files, 211.9 MB) through lzop, every compressed block of 256 KiB a
stream; and the first MiB of each file through each of liblzo2's LZO1X compressors.

| streams | streams MB | records MB | of the streams | split | rebuild |
|---|---|---|---|---|---|
| `lzop -1` (LZO1X-1(15)), 780 blocks | 92.91 | 4.27 | 4.60% | 28 MB/s | 78 MB/s |
| `lzop -3` (LZO1X-1, the default), 780 blocks | 92.03 | 4.14 | 4.50% | 28 MB/s | 78 MB/s |
| `lzop -7` (LZO1X-999 level 7), 815 blocks | 77.01 | 4.00 | 5.20% | 29 MB/s | 67 MB/s |
| `lzop -9` (LZO1X-999 level 9), 815 blocks | 76.31 | 4.65 | 6.09% | 26 MB/s | 58 MB/s |
| `lzo1x_1_compress`, 12 MiB | 7.14 | 0.25 | 3.53% | 28 MB/s | 70 MB/s |
| `lzo1x_1_11_compress` | 7.54 | 0.27 | 3.61% | 26 MB/s | 69 MB/s |
| `lzo1x_999_compress_level` 1 | 6.17 | 0.19 | 3.10% | 39 MB/s | 59 MB/s |
| ditto, level 5 | 5.67 | 0.13 | 2.25% | 44 MB/s | 70 MB/s |
| ditto, level 9 | 5.51 | 0.32 | 5.83% | 33 MB/s | 46 MB/s |

Speeds are of the data. Every stream was rebuilt.

- LZO1X-1's matches are, 99.9% of the time, the nearest ones of their length among the
  positions outside earlier matches (its hash table never sees those). The match finder's
  *skip* mode leaves them out too, which brought its records from 17% of the streams to 3.5%.
  What is left is mostly where matches start and which of the candidates is taken, which
  depend on its hash table's collisions and its skipping over literals.
- LZO1X-999's matches are the nearest ones of their lengths; what is left is its parse.

Through `aqr` (10 threads, level mid, with its check), the Silesia files as `lzop -3` files
(101.0 MB) archive to 62.84 MB in 6.5 s, and as `lzop -9` files (76.3 MB) to 62.62 MB in 7.8 s;
`zstd -3` of the same files gives 84.85 and 70.44 MB, and the original files archive to 57.83 MB.
Testing the archives (`aqr -t`) takes 1.1 s, and 1.9 s for the second with the modules as Wasm.

## Tests

```sh
cargo test -r                             # fixtures, random parses, damaged records
python3 -I scripts/gen-fixtures.py        # rewrites tests/data (needs liblzo2 and lzop)
```

`cargo test` (debug) catches an overflow from a damaged record as a panic.

## Status

The module `lzo-recons` (natively and as a Wasm module of aqr-modules) and lzop files in
aqr-compress (at the start of a file or found in other data) are done.

Not yet: LZO1X streams in other containers (the Linux kernel's images and initramfs are lzop
files; squashfs, UBIFS, btrfs and zram hold raw streams), LZO-RLE, and the other LZO formats
(LZO1, LZO1A-1C, LZO1F, LZO1Y, LZO1Z, LZO2A).
