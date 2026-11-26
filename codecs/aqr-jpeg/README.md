# aqr-jpeg

A lossless JPEG recompressor: files are restored bit-exactly (same family as Lepton and
brunsli). Speed comes first: the goal is at least 100 MB/s single-threaded for both compression and
decompression, with the ratio pursued only where it does not cost performance. The module
`jpeg-recons` takes DCT JPEGs (sequential and progressive, embedded JPEGs of at least 1 KB too,
recursively), and `jpeg-lossless-recons` the lossless JPEGs (SOF3) of raw photographs in CR2, DNG and
the like. `FORMAT.md` specifies both modules, and how they work.

## Usage

```sh
RUSTFLAGS="-C target-cpu=native" cargo build --release --example aqr-jpeg   # target/release/examples/aqr-jpeg

aqr-jpeg bench [-n iters] file...         # roundtrip check (formula, .aqr archive) + ratio / throughput
aqr-jpeg info file...                     # parse and decode, per-scan information
aqr-jpeg pscan [-n iters] file...         # per-scan decode / encode times of progressive files
aqr-jpeg fuzz [-n N] [-s seed] file...    # mutated inputs: roundtrips must stay exact, no panics
aqr-jpeg mutenc [-n N] [-s seed] file...  # roundtrip of valid non-canonical re-encodings
```

Every command takes `-j threads` (default: all cores).

Auxiliary binaries:
- `aqr-jpeg-decbench`: loops only the model decoder, the model encoder (`enc`) or the Huffman
  re-encoder (`huff`).
- `aqr-jpeg-huffbench`: Huffman decoding throughput, and a coefficient comparison against a
  `tools/coefdump.c` (libjpeg) dump.

### Debug / test options

Switches of `aqr -Z` (`-Z jpeg:verify-striped`, `-Z jpeg:no-verify-striped`; `-Z jpeg:help` lists them
and the tunable bounds). Tests set them with `aqr_common::zopt::apply` (once per process).

| Option | Effect |
|---|---|
| `jpeg:verify-progressive` | Re-encodes every progressive scan and checks the inline verdict; panics if a scan judged reproducible re-encodes differently |
| `jpeg:verify-parallel` | Checks parallel Huffman decoding (coefficients and verdicts) against serial decoding; panics on any difference |
| `jpeg:verify-striped` | Checks stripe-wise decoding and modelling (scan info, model) and re-emission (scan bytes) against holding the coefficients whole; panics on any difference |
| `jpeg:debug-canon` | Prints why a progressive scan was judged non-canonical |
| `jpeg:debug-spec` | Prints speculative-decoding phase times and fallback reasons |
| `jpeg:debug-striped` | Prints how a scan was decoded stripe by stripe (serially, by restart intervals, from located starts) and the outcome |
| `jpeg:debug-tables` | Prints the size breakdown of the serialised model tables |

`formula::encode` catches panics and leaves the input uncoded, so an oracle failure shows up
as a file that is not coded.

## What is taken

JPEG files and the JPEGs embedded in other data (Exif and Photoshop thumbnails, MPF and vendor
secondary images, found by their SOI): sequential, progressive, truncated files, trailing RSTs and
fill bytes. A scan that is not canonical (not what libjpeg writes) is kept raw, and so is a
progressive scan script breaking libjpeg's `coef_bits` rule from that scan on; a file whose scans
cannot be modelled, or whose streams are not shorter, is left as it is. The lossless JPEGs of
TIFF-based raw files are found by their SOI. Not taken: arithmetic-coded and hierarchical JPEG (unsupported SOF), lossless JPEG streams in
other containers (not found by their SOI).

## Results

Apple M4 (10 cores), one thread, through `aqr-show` (see the
[file formats overview](../../docs/file-formats.md#how-it-was-measured)): each file archived at level
`mid` with its round-trip check, then extracted; measured 2026-10-24, without `target-cpu=native`.
Ratio is archive size / input size.

| set | files | MB | ratio | zstd -3 | compress | decompress |
|---|---|---|---|---|---|---|
| `samples/` (camera and phone photos) | 13 | 60.0 | 0.752 | 0.908 | 50.1 MB/s | 91.4 MB/s |
| codec-corpus jpeg-conformance | 44 | 74.7 | 0.805 | 0.906 | 52.4 MB/s | 123.5 MB/s |
| codec-corpus ultrahdr-conformance | 14 | 29.7 | 0.788 | 0.919 | 48.7 MB/s | 104.4 MB/s |
| codec-corpus zune | 20 | 18.4 | 0.795 | 0.955 | 12.6 MB/s | 26.7 MB/s |
| all (with imageflow) | 96 | 186 | **0.785** | 0.913 | **39 MB/s** | **82 MB/s** |

zune's large progressive and subsampled test images are the slow ones. Memory is about 28 MB + 8.2x
the file for sequential files (coded a stripe at a time: a 1.3 MB file of 7680x4320 takes 54 MB);
progressive files hold their coefficients whole (a 1.4 MB one of that size takes 259 MB).

Lossless JPEG: 13 files of raw.pixls.us (CC0; `F:\Data\aqr-corpus\aqr-jpeg-lossless`, 272 MB), a
file against its archive: 5D III CR2 0.864, 5D IV 0.826, 1D X II 0.886, 1Ds III 0.953, 5D II 0.958
(sRAW 0.93), Blackmagic DNG 0.948, a small DNG 0.858, Adobe DNG Converter's tiled 0.949. One thread
decodes at about 19 MB/s; a stream in bands decodes at 155 MB/s on 16 threads.

## Tests

- `tests/formula.rs`: round trips through the formula and this crate's module.
- `tests/edge.rs`: frame shapes (sampling, component counts, scan splits, tables and intervals
  redefined, 16-bit quantisation tables, progressive refinement), what surrounds the stream, damaged
  inputs, the extremes of the dimensions, and the non-canonical variants of `variant::reencode`.
- `tests/parallel.rs`: the parallel decoding paths (restart intervals, speculative, stripe-wise,
  progressive) equal the serial ones, with the oracles on.
- `tests/regress.rs`: regression inputs (a progressive scan-script violation found by fuzzing).
- `tests/memory.rs`, `tests/alloc.rs`: peak heap of a large sequential file; no allocation sized by
  an input before it is checked.
- `tests/lossless_edge.rs`: lossless JPEG's edge cases (limits of the headers, frames of one sample
  and of 2 and 16 bits, headers cut, damaged inputs).
- `aqr-jpeg bench` round-trips every file through its formula and an `.aqr` archive; `fuzz` mutates
  inputs (bit flips, truncation, deletion, insertion, marker injection) which must round-trip exactly
  without panics, corrupted archives too; `mutenc` re-encodes the same coefficients in valid
  non-canonical ways (ZRL placement, padding, fill, trailing RSTs, EOBRUN splits). With the oracles
  (`jpeg:verify-progressive`, `jpeg:verify-parallel`) on: 6000 fuzz inputs, 600 mutenc rounds and a
  208 MB corpus, all passing.
- `samples/` holds personal photos (with GPS / Exif) and is excluded from version control.

Lossless JPEG: `aqr-jpeg ljinfo FILE...` finds the streams in any container by their SOI and checks
that each is reproduced exactly (`LJ_MODEL=1` also the model, `LJ_BANDS=n` the model in `n` bands);
`aqr-jpeg ljbench FILE...` takes a file through the formula, an archive and back.

## Left

- **Progressive compression parallelism.** It is capped by the chain of refinement scans of one
  component: decoding a refinement scan needs each block's history mask, which a speculative decoder
  cannot know, so a single scan cannot be split.
- **Progressive re-encoding of one scan.** A scan is encoded serially; it could be split by
  reconstructing the EOBRUN and BE state at chunk boundaries from the blocks before them.
- **Uncoded share.** Fuzz and mutenc leave a large share of inputs uncoded; the cause has not been
  investigated.
- **Lossless JPEG speed.** One thread decodes at 19 MB/s.
- **Lossless JPEG model.** Mixing of models and bias cancellation.
- **Lossless JPEG kinds and containers.** Hierarchical and arithmetic SOF kinds, and lossless JPEGs
  of other containers not found by their SOI (a stream cut in the middle).
- **Lossless JPEG tiles in Wasm.** The groups of tiles as tasks (the bands of a big stream already
  are).
