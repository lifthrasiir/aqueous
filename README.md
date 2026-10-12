# png-recons (`pngr`)

A lossless PNG recompressor: a PNG file is stored smaller, and restored bit for bit. Speed
comes first: the target is 100 MB/s of PNG file single-threaded on an Apple M4, both ways.
DEFLATE parsing, Huffman header coding and writing come from
[flate-recons](../flate-recons).

```sh
cargo build --release
pngr c [-t N] in.png out.pngr  # compress (N threads, default: the available cores)
pngr d [-t N] out.pngr back.png # decompress (same)
pngr bench [-t N] [-n ITERS] [-v] FILE...   # round trip check, ratio, throughput, phase times (default 1 thread)
```

```rust
let packed = png_recons::compress(&png);              // never fails: stores what it cannot code
assert_eq!(png_recons::decompress(&packed)?, png);    // checked against an xxh3 of the original
let packed = png_recons::compress_threads(&png, 8);   // the same bytes; decompress_threads likewise
```

## Results

Apple M4 (10 cores, 32 GB), one thread, CPU time (best of two runs, from `bench/`); MB/s of
PNG file, both ways. Sets from [codec-corpus](https://github.com/imazen/codec-corpus);
every file round-trips.

| set | files | PNG | output / PNG | compress | decompress |
|---|---|---|---|---|---|
| CID22 (512 px photos, graphics) | 250 | 88.9 MB | 0.857 | 84.2 MB/s | 82.9 MB/s |
| clic2025 (2048 px photos) | 62 | 228.3 MB | 0.816 | 91.8 MB/s | 89.4 MB/s |
| kadid10k (photos) | 81 | 25.9 MB | 0.778 | 93.9 MB/s | 97.4 MB/s |
| gb82 (photos, oxipng'd) | 25 | 10.0 MB | 0.928 | 76.0 MB/s | 76.1 MB/s |
| gb82-sc (screenshots, oxipng'd) | 10 | 3.0 MB | 0.883 | 40.6 MB/s | 51.9 MB/s |
| qoi screenshot_web | 14 | 40.3 MB | 0.601 | 89.8 MB/s | 102.2 MB/s |
| **all** | 442 | 396.3 MB | **0.804** | **88.6 MB/s** | **88.6 MB/s** |

The 100 MB/s target is not reached: only decompression of screenshot_web gets there (see
[Speed](#speed)). Measured
in decompressed image data instead of PNG file, the same runs are about 190 MB/s on photos
and over 1 GB/s on screenshots.

## Comparison

Other lossless recompressors of PNG files, and generic compressors, on the same sets
(`bench/`: `bench/fetch.sh` builds the tools, then `cargo run --release -- DIR...` in
`bench/`). Every output is round-tripped; times are CPU times on one thread (libraries:
the thread's, best of two runs; tools run as processes: the children's).

- [preflate-rs](https://github.com/microsoft/preflate-rs) 0.7.6 (`3bcd334`), its container
  with default settings: preflate reconstruction data, and RGB/RGBA images re-encoded as
  lossless WebP (libwebp), the rest Zstandard.
- [precomp](https://github.com/schnaader/precomp-cpp) 0.4.8 dev (`a11b077`), `-cl -lt1`:
  preflate 0.3.5, then LZMA2 over the inflated data (no image model).
- [paq8px](https://github.com/hxim/paq8px) (`c84f576`), `-8`: context mixing with image
  models; it recompresses DEFLATE only when its own zlib reproduces the stream.
- zstd 1.5.7 `-19 --single-thread`, xz 5.8.3 `-9 -T1`, on the PNG file as it is.

Output size / PNG size, and MB/s of PNG file:

| set | png-recons | preflate-rs | precomp | zstd -19 | xz -9 |
|---|---|---|---|---|---|
| CID22 | 0.857 | **0.804** | 0.950 | 0.998 | 0.997 |
| clic2025 | 0.816 | **0.792** | 0.914 | 0.994 | 0.990 |
| kadid10k | 0.778 | **0.751** | 0.926 | 0.999 | 0.996 |
| gb82 | **0.928** | 0.934 | 1.042 | 1.000 | 1.000 |
| gb82-sc | 0.883 | 0.978 | **0.841** | 0.966 | 0.969 |
| qoi screenshot_web | 0.601 | **0.532** | 0.637 | 0.952 | 0.950 |
| **all (442 files, 396 MB)** | 0.804 | **0.771** | 0.897 | 0.991 | 0.988 |
| compress | **88.6 MB/s** | 2.5 MB/s | 1.1 MB/s | 27.1 MB/s | 10.2 MB/s |
| decompress | 88.6 MB/s | 13.2 MB/s | 2.9 MB/s | **408.8 MB/s** | 57.1 MB/s |

paq8px is too slow for the whole sets (about 7 KB/s both ways); on 9 files (the first 3 of
kadid10k, CID22's training set and gb82-sc; 2.7 MB) it gives 0.962, against 0.812 for
png-recons, 0.802 for preflate-rs and 0.930 for precomp: none of these zlib streams is one
its zlib reproduces, so it models the compressed bytes.

- **preflate-rs** is 4% smaller overall: WebP lossless codes the pixels, where png-recons
  keeps the stream's LZ77 parse. It is 35× slower to compress and 6.7× slower to
  decompress, and it gains little on oxipng/zopfli-style streams (gb82, gb82-sc: its
  reconstruction data is large, or the stream is left as it is).
- **precomp** recovers the pixel data with preflate but only LZMA-compresses it: 11% larger
  than png-recons overall, and 80× / 30× slower. It is best on gb82-sc (where long matches
  make LZMA's job easy) and expands gb82 (preflate's reconstruction data for those streams
  outweighs LZMA's gain).
- **Generic compressors** gain 1% on PNG files (5% on screenshots).

## Design

### Why the LZ77 parse is kept

The obvious design inflates the image data, codes the pixels with a strong image model, and
re-creates the DEFLATE stream on the way back by reproducing the encoder's choices
(flate-recons' job). Measured on these sets, it cannot be fast:

- Most PNG files here are exact zlib output (libpng's level 6, or level 9). flate-recons'
  zlib emulation reproduces them for about 10 bytes, but at zlib's own speed: 1-20 MB/s of
  PNG file. Its fast, rule-based representation is 5-25 MB/s of PNG here, and its
  corrections cost 3-30% of the PNG (photo streams have long literal codes and short,
  unpredictable matches, and oxipng/zopfli-style streams follow no simple rule).
- PNG data expands 2-30× when inflated, so even 100 MB/s of decompressed data is only
  3-50 MB/s of PNG.

So png-recons keeps the stream's own tokens (literals and matches) and codes *them* against
the image: decompression decodes tokens and Huffman-codes them again, with no match search.
The price is ratio on photos, where an LZ77 parse spends many bits on short matches that an
image codec would not need (`src/bin/probe.rs`: 1.1 literals and 0.1 matches per PNG byte
on the photo sets).

### Model (`src/model.rs`)

- **Literals** are coded as the residual of the raw (unfiltered) byte against LOCO-I's MED
  predictor of its left, upper and upper-left neighbours. Green and blue add the previous
  channel's residual (inter-channel decorrelation). The row's PNG filter is then reapplied
  to give the byte the stream holds, so the PNG filter choice does not matter.
- **Contexts**: channel × 12 buckets of activity (`|a - c| + |b - c|` of the raw neighbours
  plus the magnitudes of the filtered bytes left and above) × whether a match came just
  before; filter bytes have a table of their own.
- **Matches** start as an escape symbol in the literal alphabet (255, then a binary decision
  against a literal residual of 255), so a literal costs a single symbol. Lengths are coded
  by activity, distances by length bucket, with DEFLATE's extra bits raw.
- **Entropy coding** (`src/rans.rs`, on `recons_common::rans32`): static per-image tables (two passes when compressing),
  12-bit probabilities, four interleaved 32-bit rANS states with 16-bit renormalisation.
  Tables are stored as quantised log-frequencies (4 steps per octave, differences coded
  with Elias-gamma). The encoder divides by multiplying with packed reciprocals; the decoder
  looks symbols up in a full table per context and renormalises without a branch.
- **Other pixel formats** (bit depths other than 8, palettes, 16-bit) are coded byte-wise
  in the filtered domain, by byte position and the filtered neighbours' magnitudes.
- Interlaced images are handled as their seven passes, each with its own rows.

### Container (`src/container.rs`)

```text
"PNGR" version mode
mode 0: the file as it is (when coding does not make it smaller)
mode 1: xxh3 of the file | zstd(skeleton) | per image: u32 length + record
record: 0 + payload (kept as it is) | 1 + layout + Huffman headers + model payload
```

- The **skeleton** holds every chunk without its image data, CRCs only where they are wrong,
  and anything after IEND.
- An **image** is a run of IDAT chunks, or in an APNG a run of fdAT chunks (the frame's size
  comes from its fcTL; fdAT sequence numbers stay in the skeleton). Its payload may hold
  several zlib streams one after the other; Apple's iDOT files use one stream with a full
  flush, which needs nothing special.
- The **layout** lists each stream's blocks (type, token count, stored lengths and padding,
  last-block flags), tail padding and trailing bytes (the Adler-32, and any junk), and the
  matches of length 258 sent as 284 + 31. **Huffman headers** go through flate-recons'
  header coder, against the frequencies of the block's tokens: zlib's own trees cost a bit.
- An image is kept as it is if its record would not be smaller, and the whole file is
  stored if the coded form would not be smaller. Anything unexpected (not a PNG, broken
  streams, invalid headers, truncation) is stored or kept; a panic while compressing also
  falls back to storing.

### Speed

Measured with `pngr bench`, `samply`, and retired instruction counts
(`/usr/bin/time -l`). The decoder runs at about 6 instructions per cycle, so what helped
was doing less:

- **Decoding**: the rANS states live in a local copy of the decoder (in registers, not
  behind a pointer); literals are decoded in runs within a row, in a loop specialised by
  the row's filter type, without bounds checks on the offsets the buffer layout keeps in
  range (the data has a row of padding in front, the raw image a zero row and zero left
  margin per pass). Symbol frequencies for the Huffman headers are counted while decoding.
- **Encoding**: the data is parsed with flate-recons, unfiltered, and every pixel byte's
  context and residual computed row by row (NEON, 16 bytes at a time); then the tokens are
  walked. Streams that expand more than 4× (mostly long matches) are modelled as the parser
  hands over the tokens instead, skipping the bytes inside matches.
- **flate-recons** got two-level Huffman decoding tables (photo streams' 15-bit literal
  codes made its single-level tables 128 KB), pattern copies for short match distances, a
  branch-free token writer and a header reader that takes counted frequencies.
- Tried and dropped: two rANS states (latency-bound), padding the rANS stream to drop a
  bounds check (the clamp lengthened the dependency chain), passing literals to the model
  in batches (it serialised the parser's and the model's dependency chains, which ran
  side by side before), tracking the row position incrementally, smaller (11-bit) tables
  (not cache-bound; 0.2-0.6% larger output).

Where the time goes (kadid10k): decompression is ~75% token decoding (about 210
instructions per token), ~15% Huffman writing, ~6% Huffman headers, ~3% tables;
compression is ~39% parsing, ~16% unfiltering and the row model, ~19% the token walk,
~20% rANS tables and encoding, ~6% block layout and Huffman headers. Photos average
1.1 literals per PNG byte, so 100 MB/s means about 9 ns per literal end to end. gb82-sc
expands 30× when inflated; copying and unfiltering 93 MB of image data for 3 MB of PNG is
already over budget.

### Threads

`-t N` (`compress_threads`, `decompress_threads`): the output is the same bytes whatever the
number of threads, and one thread runs the code above unchanged. One image is coded by one
rANS stream whose contexts come from the decoded pixels, so decoding an image stays serial;
threads overlap what is independent around it:

- An APNG (or any file with several image runs) has its images coded, and rebuilt, in
  parallel: each image makes a record of its own (length-prefixed), concatenated in order.
  At most `N` images are in flight.
- Compressing, the skeleton's zstd and the checksum run beside the images.
- A large image (1 MB of data or more) has its symbols computed by stripes of rows on
  several threads, after the serial unfilter pass, and the symbol histograms counted by
  chunks and summed.
- Decompressing a payload of several zlib streams writes the streams in parallel.

Files under 64 KB are handled on the calling thread. A panic in a worker is a panic of the
call, so `compress_threads` stores the file as `compress` does. Not done: parsing the zlib
streams of a multi-stream image in parallel (a stream's end is known once it is parsed),
decompressing the skeleton beside the first image (the image's geometry comes from it).

## Testing

```sh
cargo test --release
```

- `tests/roundtrip.rs`: synthetic PNG files through every DEFLATE encoder of
  [flate-zoo](../flate-zoo) (zlib and its forks, libdeflate, zopfli, miniz, ...), every
  pixel format with and without interlacing, several zlib streams in one image, APNG frames,
  malformed files (junk after the stream, wrong CRCs, trailers, truncations, too little or
  too much image data, broken streams, non-PNG input) and 2,000 corrupted containers
  (errors, never panics). These use `container::try_compress`, which does not catch panics.
  The `threads_*` tests check the compressed bytes are identical for 1, 2, 3 and 8 threads, and
  that decompression gives the same file or the same Ok/Err, on large images, several zlib
  streams, APNGs, malformed and corrupted input.
- Unit tests: the rANS coder, the code tables, Paeth against the specification for all
  inputs, unfiltering in pieces against the reference.
- Every PNG in codec-corpus (733 files) and zune's fuzz corpus (1,563 files) round-trips.

## Layout

| File | Contents |
|---|---|
| `src/png.rs` | Chunk parsing, IHDR |
| `src/filter.rs` | PNG filters (reference unfilter, branch-free Paeth) |
| `src/model.rs` | Geometry, the token model, encoder and decoder loops, NEON row model |
| `src/rans.rs` | Static rANS: tables, encoder, decoder, bit I/O |
| `src/container.rs` | File format, compression, decompression |
| `src/bin/pngr.rs` | Command-line tool |
| `src/bin/probe.rs`, `src/bin/ransbench.rs` | Research tools: token statistics, rANS speed |
| `bench/` | Comparison with other tools (`bench/fetch.sh` builds them into `third_party/`) |

## Next steps

- Speed: keep only the matches when decoding (literals are implied by their positions),
  sparing a 6-byte token per literal on both sides; this needs a flate-recons writer that
  takes that form.
- Ratio on photos: a pixel-domain mode for streams flate-recons reproduces exactly (zlib),
  where a stronger image model would replace the LZ77 parse, at zlib's speed.
- More pixel formats in the raw-domain model (16-bit, palettes by index similarity).
