# aqr-lznt1

LZNT1 split into its data and a record of its parse, and rebuilt bit for bit. The archive then
stores the data, which compresses better than the chunks, which aqr-compress recompresses in
turn (a PE file by aqr-exe, and so on) and which other files may share. Windows writes LZNT1 as
NTFS's compression of files (`compact /c`, the "compress contents" attribute: units of 16
clusters, 64 KiB of data with clusters of 4 KiB), in `RtlCompressBuffer` buffers and in a few
file formats; a container (NTFS in aqr-disk) takes the units apart and makes the applications.

No encoder is reproduced (Windows' is closed): any stream has a record, 0.35% of the compressed
bytes for Windows'. It was written from the public description of the algorithm ([MS-XCA], 2.5), without
any implementation's code. `FORMAT.md` specifies the streams, the record, the module `lznt1-recons`,
and how Windows' encoder behaves.

```sh
cargo run --release -p aqr-lznt1 --example aqr-lznt1-check -- [-n K] FILE...   # split, rebuilt; record share and speeds
```

```rust
use aqr_lznt1::recons;
// a file's units, each its allocated bytes (none: a hole) and its data's length
let sp = recons::split(&units)?;
assert_eq!(recons::join(&sp.data, &sp.record)?, units.concat());
```

A buffer of `RtlCompressBuffer` is one unit of the buffer's whole data length.

## What is taken

- NTFS compressed units (`compact /c`: 16 clusters, 64 KiB of data at 4 KiB clusters; any cluster size
  and unit length is modelled), taken apart by aqr-disk from NTFS volumes in disk images (stored,
  holes, compressed; aqr-disk's README has the figures), and `RtlCompressBuffer` buffers
  (`COMPRESSION_FORMAT_LZNT1`, the standard engine: one unit of the buffer's whole length).
- Not taken: the maximum engine's output, other systems' NTFS drivers, ESE databases and hibernation
  files (no sample here).

## Results

Windows 11 samples, 2026-11-09 (`aqr-lznt1-check -n 3`, an M-series Mac): NTFS on a 64 MiB volume
(`compact /c` files) and `RtlCompressBuffer`, on text (712,895 bytes of Rust source), a repeating
byte pattern (`gen`, 1 MiB), text, pattern and text again (`mixed`, 2 MiB) and noise (256 KiB). Every
stream was rebuilt. Record share of the compressed bytes:

| sample | NTFS units | `RtlCompressBuffer` |
|---|---|---|
| text | 0.48% | 0.52% |
| gen | 0.12% | 0.15% |
| mixed | 0.38% | 0.42% |
| noise | | 0.00% (7 bytes: all chunks stored) |
| all | | 0.35% (9,700 of 2,758,564 bytes, the 7 files) |

Speed, one thread, MB/s of data: splitting 26 to 43 (text), up to 210 (repeats); rebuilding 130
(text), up to 680 (repeats). The module `lznt1-recons` is 29,399 bytes of Wasm (9,108 bytes as its
module block).

## Tests

```sh
cargo rt -p aqr-lznt1
```

Windows' units and buffers in `tests/data` (zstd'd: units of a text file and a mixed one, the
last unit of 57,535 bytes with its zeros past the end, buffers of text, a pattern, noise and
the join of text and pattern): read, written back, split and rebuilt. Streams of a small encoder
of the tests (greedy matches from all positions or from the tokens' starts, random literals and
shorter matches, stored chunks, unused flag bits set, chunks of other lengths, holes, stored
units, end headers or not, padding of zeros or of other bytes, clusters of 1, 512 and 4096
bytes, unit lengths that are not a multiple of 4096): the same. Damaged records, data and units
do not panic. aqr-decompress runs the module as WebAssembly on `ntfs.units` and `buf-gen.units`
(with a hole and a stored unit added) and checks its memory plan.

## Left

- **Windows' candidate set.** The decisions of a literal where Windows has a candidate are most of the
  record (0.35% of the compressed bytes on text); its hash table is not known.
- **Other cluster sizes.** Clusters of 512 bytes to 64 KiB make units of 16 clusters; the model takes
  any, but only 4 KiB has a Windows sample.
- **Other writers.** The maximum engine, other systems' NTFS drivers, ESE databases and hibernation
  files have no sample here.
- **Threads.** A large buffer is split on one thread.
