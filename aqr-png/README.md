# aqr-png

Lossless PNG recompression, as a module of entry formulas ([aqr-formula](../aqr-formula)):
a PNG file is rebuilt bit for bit from a smaller representation. Speed comes first: the
target is 100 MB/s of PNG file single-threaded on an Apple M4, both ways. DEFLATE parsing,
Huffman header coding and writing come from [aqr-flate](../aqr-flate). The format is
specified in [FORMAT.md](FORMAT.md).

```sh
cargo build --release
pngr bench [-t N] [-n ITERS] [-v] [-c | -d] FILE...   # round trip check through an archive, ratio, throughput (default 1 thread)
```

```rust
let f = aqr_png::formula::encode(&png, 8);   // None where there is nothing to code
assert!(f.unwrap().eval(&modules)? == png);  // `modules` runs `formula::decode` for `formula::MODULE`
```

## Results and comparison

The sizes and speeds measured for earlier versions of this crate were of its own file
format, which is gone; they need re-measuring on archives (`pngr bench`, and `bench/`).

`bench/` (`bench/fetch.sh` builds the tools, then `cargo run --release -- DIR...` in
`bench/`) compares aqr-png with other lossless recompressors of PNG files and with generic
compressors, on sets of PNG files from [codec-corpus](https://github.com/imazen/codec-corpus).
Every output is round-tripped; times are CPU times on one thread (libraries: the thread's,
best of two runs; tools run as processes: the children's).

- [preflate-rs](https://github.com/microsoft/preflate-rs) 0.7.6 (`3bcd334`), its container
  with default settings: preflate reconstruction data, and RGB/RGBA images re-encoded as
  lossless WebP (libwebp), the rest Zstandard.
- [precomp](https://github.com/schnaader/precomp-cpp) 0.4.8 dev (`a11b077`), `-cl -lt1`:
  preflate 0.3.5, then LZMA2 over the inflated data (no image model).
- [paq8px](https://github.com/hxim/paq8px) (`c84f576`), `-8`: context mixing with image
  models; it recompresses DEFLATE only when its own zlib reproduces the stream. It is too
  slow for whole sets (about 7 KB/s both ways).
- zstd 1.5.7 `-19 --single-thread`, xz 5.8.3 `-9 -T1`, on the PNG file as it is.

The sizes of aqr-png in `bench/` are those of the archive of its formula (the skeleton and
the records, compressed by `aqr-formula`'s archive coder at its middle level).

## Design

### Why the LZ77 parse is kept

The obvious design inflates the image data, codes the pixels with a strong image model, and
re-creates the DEFLATE stream on the way back by reproducing the encoder's choices
(aqr-flate's job). Measured on these sets, it cannot be fast:

- Most PNG files here are exact zlib output (libpng's level 6, or level 9). aqr-flate's
  zlib emulation reproduces them for about 10 bytes, but at zlib's own speed: 1-20 MB/s of
  PNG file. Its fast, rule-based representation is 5-25 MB/s of PNG here, and its
  corrections cost 3-30% of the PNG (photo streams have long literal codes and short,
  unpredictable matches, and oxipng/zopfli-style streams follow no simple rule).
- PNG data expands 2-30× when inflated, so even 100 MB/s of decompressed data is only
  3-50 MB/s of PNG.

So aqr-png keeps the stream's own tokens (literals and matches) and codes *them* against
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
- **Entropy coding** (`src/rans.rs`, on `aqr_common::rans32`): static per-image tables (two passes when compressing),
  12-bit probabilities, four interleaved 32-bit rANS states with 16-bit renormalisation.
  Tables are stored as quantised log-frequencies (4 steps per octave, differences coded
  with Elias-gamma). The encoder divides by multiplying with packed reciprocals; the decoder
  looks symbols up in a full table per context and renormalises without a branch.
- **Other pixel formats** (bit depths other than 8, palettes, 16-bit) are coded byte-wise
  in the filtered domain, by byte position and the filtered neighbours' magnitudes.
- Interlaced images are handled as their seven passes, each with its own rows.

### Representation (`src/codec.rs`)

The complete format is specified in [FORMAT.md](FORMAT.md). The formula has three inputs:
the skeleton, an index, and the records of the images.

```text
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
  matches of length 258 sent as 284 + 31. **Huffman headers** go through aqr-flate's
  header coder, against the frequencies of the block's tokens: zlib's own trees cost a bit.
- An image is kept as it is if its record would not be smaller. Anything unexpected (not a
  PNG, broken streams, invalid headers, truncation) is kept, or gives no formula; a panic
  while encoding gives no formula either.

### Speed

Measured with `pngr bench`, `samply`, and retired instruction counts
(`/usr/bin/time -l`). The decoder runs at about 6 instructions per cycle, so what helped
was doing less:

- **Decoding**: the rANS states live in a local copy of the decoder (in registers, not
  behind a pointer); literals are decoded in runs within a row, in a loop specialised by
  the row's filter type, without bounds checks on the offsets the buffer layout keeps in
  range (the data has a row of padding in front, the raw image a zero row and zero left
  margin per pass). Symbol frequencies for the Huffman headers are counted while decoding.
- **Encoding**: the data is parsed with aqr-flate, unfiltered, and every pixel byte's
  context and residual computed row by row (NEON, 16 bytes at a time); then the tokens are
  walked. Streams that expand more than 4× (mostly long matches) are modelled as the parser
  hands over the tokens instead, skipping the bytes inside matches.
- **aqr-flate** got two-level Huffman decoding tables (photo streams' 15-bit literal
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

The `threads` of `formula::encode` and `formula::decode`: the output is the same whatever the
number of threads, and one thread runs the code above unchanged. One image is coded by one
rANS stream whose contexts come from the decoded pixels, so decoding an image stays serial;
threads overlap what is independent around it:

- An APNG (or any file with several image runs) has its images coded, and rebuilt, in
  parallel: each image makes a record of its own, concatenated in order.
  At most `N` images are in flight.
- A large image (1 MB of data or more) has its symbols computed by stripes of rows on
  several threads, after the serial unfilter pass, and the symbol histograms counted by
  chunks and summed.
- Decompressing a payload of several zlib streams writes the streams in parallel.

Files under 64 KB are handled on the calling thread. A panic in a worker is a panic of the
call, so `formula::encode` gives no formula. Not done: parsing the zlib streams of a
multi-stream image in parallel (a stream's end is known once it is parsed).

## Testing

```sh
cargo test --release
```

- `tests/roundtrip.rs`, `tests/formula.rs`: synthetic PNG files through every DEFLATE
  encoder of [flate-zoo](../flate-zoo) (zlib and its forks, libdeflate, zopfli, miniz, ...),
  every pixel format with and without interlacing, several zlib streams in one image, APNG
  frames, malformed files (junk after the stream, wrong CRCs, trailers, truncations, too
  little or too much image data, broken streams, non-PNG input), each through
  `formula::encode` and the evaluation of the formula, and damaged inputs of the module
  (errors, never panics). The `threads_*` tests check the formula is identical for 1, 2, 3
  and 8 threads, and that evaluation gives the same file or the same Ok/Err, on large
  images, several zlib streams, APNGs, malformed and corrupted input.
- Unit tests: the rANS coder, the code tables, Paeth against the specification for all
  inputs, unfiltering in pieces against the reference.

## Layout

| File | Contents |
|---|---|
| `src/png.rs` | Chunk parsing, IHDR |
| `src/filter.rs` | PNG filters (reference unfilter, branch-free Paeth) |
| `src/model.rs` | Geometry, the token model, encoder and decoder loops, NEON row model |
| `src/rans.rs` | Static rANS: tables, encoder, decoder, bit I/O |
| `src/codec.rs` | Skeleton, image records, rebuilding the file |
| `src/formula.rs` | The formula module |
| `src/bin/pngr.rs` | Benchmark tool |
| `src/bin/probe.rs`, `src/bin/ransbench.rs` | Research tools: token statistics, rANS speed |
| `bench/` | Comparison with other tools (`bench/fetch.sh` builds them into the workspace's `third_party/`) |

## Next steps

- Speed: keep only the matches when decoding (literals are implied by their positions),
  sparing a 6-byte token per literal on both sides; this needs a aqr-flate writer that
  takes that form.
- Ratio on photos: a pixel-domain mode for streams aqr-flate reproduces exactly (zlib),
  where a stronger image model would replace the LZ77 parse, at zlib's speed.
- More pixel formats in the raw-domain model (16-bit, palettes by index similarity).
