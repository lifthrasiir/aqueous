# aqr-png

Lossless PNG recompression, as a module of entry formulas ([aqr-formula](../../crates/aqr-formula)):
a PNG file is rebuilt bit for bit from a smaller representation. Speed comes first: the
target is 100 MB/s of PNG file single-threaded on an Apple M4, both ways. `FORMAT.md` specifies
the modules `png-recons`, `png-against` and `png-set`, and how it works (why the stream's own
LZ77 parse is kept and coded against the image, the model, the memory plan, the threads).

```sh
cargo build --release --example aqr-png   # target/release/examples/aqr-png
aqr-png bench [-t N] [-n ITERS] [-v] [-c | -d] FILE...   # round trip check through an archive, ratio, throughput (default 1 thread)
```

```rust
let f = aqr_png::formula::encode(&png, 8);   // None where there is nothing to code
assert!(f.unwrap().eval(&modules)? == png);  // `modules` runs `formula::decode` for `formula::MODULE`
```

## What is taken

- PNG files and APNG, every pixel format, interlaced or not; an image whose payload holds several
  zlib streams; files of any DEFLATE encoder (zlib and its forks, libdeflate, zopfli, miniz, ...).
  The image's own parse is coded, so no encoder is emulated.
- 8-bit images by a pixel model (MED, channels decorrelated); other depths, palettes and 16-bit
  byte-wise in the filtered domain.
- A file coded against a similar one (`formula::encode_against`), and a set of such files
  (`png-set`, `formula::encode_set`): variants of a picture with layers shown or hidden.
- An image whose record would not be smaller is kept as it is; anything unexpected (not a PNG,
  broken streams, invalid headers, truncation) is kept or gives no formula.
- Threads (`threads` of `formula::encode` and `decode`): the output is the same whatever the
  number; an APNG's images and a large image's windows run in parallel, and the DEFLATE streams
  are written on a thread of their own (`aqr-png bench -t 4` decompresses at 104 MB/s against
  88 on one thread).
- Peak heap of encoding about 4.2 MB + 3.7x the file, of evaluation 2.9 MB + 1.1x (364 files).

## Results

Apple M4, one thread, 2026-10-24, through `aqr-show` (see the
[file formats overview](../../docs/file-formats.md#how-it-was-measured)): each file archived at
level `mid` with its round-trip check, then extracted; sets of
[codec-corpus](https://github.com/imazen/codec-corpus). Ratios are tentative, since the decoder
modules were not in the archives then.

| set | files | MB | ratio | zstd -3 | compress | decompress |
|---|---|---|---|---|---|---|
| clic2025 | 62 | 228.3 | 0.816 | 1.000 | 45.2 MB/s | 92.4 MB/s |
| CID22 | 208 | 82.8 | 0.855 | 1.000 | 39.3 MB/s | 84.9 MB/s |
| kadid10k | 66 | 22.7 | 0.774 | 1.000 | 47.0 MB/s | 101.7 MB/s |
| qoi-benchmark | 14 | 40.3 | 0.601 | 0.984 | 45.7 MB/s | 104.4 MB/s |
| all (with 6 small sets) | 379 | 391 | **0.802** | 0.997 | **42 MB/s** | **90 MB/s** |

Process memory is about 7 MB + 10x the file (75 MB for a 6.4 MB photo), most of it `aqr-show`'s
own.

Against a reference: seven ZIP archives of Clip Studio Paint variant sets (684 MB of PNG files
in groups of 13 to 88 of the same size), each file coded against a parent along a maximum
spanning tree: 85% of the tokens are the parent's, and the records come to 109.5 MB against
491.5 MB alone (4.5x); coding takes 36 s and decoding 18 s of one thread.

`bench/` (`bench/fetch.sh` builds the tools, then `cargo run --release -- DIR...` there)
compares aqr-png with [preflate-rs](https://github.com/microsoft/preflate-rs) 0.7.6,
[precomp](https://github.com/schnaader/precomp-cpp) 0.4.8 (`-cl -lt1`),
[paq8px](https://github.com/hxim/paq8px) (`-8`, about 7 KB/s both ways: too slow for sets),
zstd 1.5.7 `-19` and xz 5.8.3 `-9` on the PNG file, on the same sets. Every output is
round-tripped; times are CPU times on one thread.

## Tests

```sh
cargo test --release
```

- `tests/roundtrip.rs`, `tests/formula.rs`: synthetic PNG files through every DEFLATE
  encoder of [flate-zoo](../../flate-zoo) (zlib and its forks, libdeflate, zopfli, miniz, ...),
  every pixel format with and without interlacing, several zlib streams in one image, APNG
  frames, malformed files (junk after the stream, wrong CRCs, trailers, truncations, too
  little or too much image data, broken streams, non-PNG input), each through
  `formula::encode` and the evaluation of the formula, and damaged inputs of the module
  (errors, never panics). The `threads_*` tests check the formula is identical for 1, 2, 3
  and 8 threads, and that evaluation gives the same file or the same Ok/Err, on large
  images, several zlib streams, APNGs, malformed and corrupted input.
- `tests/memory.rs`: the peak heap of coding a 2048 × 1536 photo-like image stays within
  the symbols and a few times the file (before the windows: 16× and 6.5× the data).
- Unit tests: the rANS coder, the code tables, Paeth against the specification for all
  inputs, unfiltering in pieces against the reference.

`aqr -Z png:window=N` moves the windows on every `N` bytes, whatever the image's size, so that
the tests (and `aqr-png bench` on the corpus) run through them on small images too.

## Layout

| File | Contents |
|---|---|
| `src/png.rs` | Chunk parsing, IHDR |
| `src/filter.rs` | PNG filters (reference unfilter, branch-free Paeth) |
| `src/model.rs` | Geometry, the token model, encoder and decoder loops, NEON row model (the pixel model's pieces are aqr-raw's) |
| `src/rans.rs` | Static rANS: tables, encoder, decoder, bit I/O |
| `src/codec.rs` | Skeleton, image records, rebuilding the file |
| `src/formula.rs` | The formula module |
| `src/part.rs`, `src/plan.rs` | png-recons as a part module; the modules' memory plans |
| `examples/aqr-png.rs` | Benchmark tool |
| `src/bin/probe.rs`, `src/bin/ransbench.rs` | Research tools: token statistics, rANS speed |
| `bench/` | Comparison with other tools (`bench/fetch.sh` builds them into the workspace's `third_party/`) |

## Left

- **Decoding speed.** Keep only the matches when decoding (literals are implied by their
  positions), as the encoder does now, sparing a 6-byte token per literal; needs an aqr-flate
  writer that takes that form.
- **Encoder memory.** The encoder's symbols (2 bytes per byte of data on photos) are all that
  still follows the image; dropping them needs the model run twice (counting, then the symbols
  again, window by window from the last, for the backward rANS coder), or tables per segment
  of the image (a format change).
- **Ratio on photos.** An LZ77 parse spends many bits on short matches that an image codec
  would not need; a pixel-domain mode for streams aqr-flate reproduces exactly (zlib) would
  replace it with a stronger image model, at zlib's speed.
- **More pixel formats** in the raw-domain model (16-bit, palettes by index similarity).
- **Multi-stream images.** Parsing the zlib streams of one image in parallel (a stream's end is
  known once it is parsed).
