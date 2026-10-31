# aqr-bz2

bzip2 recompression. A bzip2 stream is rebuilt bit for bit from the data it holds, so the
archive stores the data instead. Its compressor is libbzip2's (1.0.3 and later, the default
work factor), reimplemented to give the same bits: the `bzip2` command line, Python's `bz2`,
`tarfile` and most libraries write it, and pbzip2 writes a libbzip2 stream per piece. BWT is
already a strong model of text, so text alone gains little or nothing; what the data holds
(the files of a `.tar.bz2`, recompressed by their formats and shared with the same files
archived along) is what pays.

```sh
cargo run --release -p aqr-bz2 --example aqr-bz2-check -- [-t THREADS] FILE_OR_DIR...   # rebuilt, speed
python -I codecs/aqr-bz2/scripts/gen-fixtures.py                                        # tests/data from libbzip2
```

```rust
use aqr_bz2::formula::{self, MODULE};
// None where the file does not start with a stream libbzip2 gives
let f = formula::encode(&file, threads).unwrap();
// `f` rebuilds `file` when evaluated with `formula::decode` as the module `MODULE`
```

`FORMAT.md` specifies the module `bz2-recons`, with the compressor in full. The code was written
from memory of libbzip2's behaviour and checked against libbzip2 1.0.8 as a black box, without
reading its code (BSD-style, not public domain).

## How

- The blocks are cut where libbzip2 cuts them, from the level in the header alone.
- The BWT is the suffix array (SA-IS) of the block written twice. Rotations then sort as
  libbzip2 sorts them, but for equal rotations of a periodic block (data of one repeated
  pattern, such as zeros through RLE1), where only the rank `origPtr` can differ: the params
  note it for those blocks (3-4 bytes each).
- The tables, selectors and code lengths are libbzip2's `sendMTFValues` and
  `BZ2_hbMakeCodeLengths` (FORMAT.md §4.3, §5).
- Streams one after another (pbzip2, `cat a.bz2 b.bz2`) are one application; what follows
  them, or a stream not reproduced, is sliced as it is.
- Blocks are compressed in parallel natively; the Wasm module (46 KB) runs on one thread.

## Status

Not reproduced, kept as they are: other compressors (lbzip2, 7-Zip's, Dart's), libbzip2 before
1.0.3 where a code would be over 17 bits, randomised blocks (bzip2 0.9.0), streams flushed
early (`BZ_FLUSH`), data over 1 GiB. Left: bzip2 inside ZIP (method 12), 7z, DMG (UDBZ);
tasks in the Wasm module; a stream not reproduced could keep its tables and selectors as a
record (lbzip2) rather than be kept whole.

## Results

Measured on 2026-10-31 on Windows (x86-64, 16 threads), `aqr -c` at level mid against the
files stored as they are (no gain at all before aqr-bz2):

| files | bzip2 | `aqr -c` |
|---|---|---|
| this repository's sources as a tar (`bzip2 -9`) | 6.63 MB | 5.83 MB (−12.1%) |
| a Rust stage0 snapshot (a tar of Windows executables) | 25.70 MB | 22.63 MB (−11.9%) |
| emsdk's `wasm-binaries.tbz2` | 122.13 MB | 103.24 MB (−15.5%) |
| 40 files: 14 Rust snapshots, emsdk's, Python packages' data | 127.97 MB | 84.77 MB (−33.8%; the snapshots share files) |
| all 79 public files of the machine (disk images of e2fsprogs' tests, pickles and the above) | 352.77 MB | 256.09 MB (−27.4%; 70 s, 3.2 GB heap peak) |
| 20 MB of Korean JSON text | 5.09 MB | 5.09 MB (kept: its data codes larger, 5.50 MB) |

`aqr-bz2-check` rebuilt 149 of the 153 bzip2 files found on that machine (1.88 GB of streams,
6.8 GB of data): of the others, two hold over 1 GiB of data, one is Go's test of a damaged
stream, and one (813 bytes) is a test file of Dart's archive package, of another compressor.
Python's `bz2`, the `bzip2` command line (MSYS2's 1.0.8), `tarfile` and the Rust project's
snapshot tool all gave libbzip2's streams.

The compressor runs at 11.9 MB/s of data on one thread and 43.6 MB/s on 8 (`aqr-bz2-check`,
51 MB of data); libbzip2 itself is in the same class. A block's work is about 30 bytes per
symbol (27 MB for a block of 900 kB), on each thread. Small files pay the module (18 KB
compressed) where no other file of the archive does: a 1 KB `.pkl.bz2` alone archives to
19.5 KB.

## Without undoing the BWT

Considered (2026-10-31): storing a stream as the symbols after its BWT, coded by a model of
their own, where its data would not pay (no duplicates, nothing recompressed in it), and
undoing the BWT only where it does. The stream would be rebuilt by MTF and libbzip2's tables
alone, without sorting. `aqr-bz2-bwt-model` gives what those symbols cost under two adaptive
models (ideal code lengths, the models' state kept across blocks), against bzip2's own
coding:

| stream | bzip2 | MTF ranks (QLFC-like) | the symbols directly (orders 0-2, a run context, mixed, SSE; bcm-like) |
|---|---|---|---|
| 20 MB of Korean JSON | 5.09 MB | −0.4% | −2.2% |
| this repository's sources as a tar | 6.63 MB | +0.3% | −0.5% |
| `spec/index.bs` (0.9 MB) | 56.0 KB | +1.6% | +8.3% |
| a Rust stage0 snapshot | 25.70 MB | −0.6% | −2.3% |

With blocks of at most 900 kB, bzip2's Huffman coding of MTF ranks is within a few percent of
these models; most of what stronger BWT compressors gain comes from larger blocks, which a
stream to rebuild cannot have. The duplicates are taken already: aqr-compress takes the data
where another file holds it (`n.json` and `n.json.bz2` archive to 5.52 MB, as `n.json` alone)
and keeps the stream otherwise. So a hybrid would save about 2% of the streams kept, for a
second module (a context-mixing coder, a few MB/s) and a choice in aqr-compress; not done. A
better model of text in the archive (the JSON's data codes to 5.50 MB, the stream is 5.09 MB)
would serve those streams and every other text alike.

## Tests

- `tests/formula.rs`: the fixtures of `tests/data` (levels 1, 2, 5 and 9; empty, one to four
  bytes, text over several blocks, runs of every length across the cuts, zeros and a periodic
  block, random bytes, skewed symbols whose codes are cut down to 17 bits) reproduced; several
  streams and what follows; damaged streams and params turned down.
- `tests/formula.rs` (`code_lengths_of_libbzip2`): `tests/data/code-lengths.txt`, the code
  lengths libbzip2 1.0.8's `BZ2_hbMakeCodeLengths` gives for 68 sets of frequencies (some of
  Fibonacci's, cut down to 17 bits), matched.
- aqr-compress `tests/compress.rs` (`bz2_nested`): bzip2 around gzip around FLAC, at the start
  of a file and found in data; aqr-decompress `tests/wasm.rs`: the module natively and as Wasm.
