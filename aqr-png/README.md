# aqr-png

Lossless PNG recompression, as a module of entry formulas ([aqr-formula](../aqr-formula)):
a PNG file is rebuilt bit for bit from a smaller representation. Speed comes first: the
target is 100 MB/s of PNG file single-threaded on an Apple M4, both ways. DEFLATE parsing,
Huffman header coding and writing come from [aqr-flate](../aqr-flate). The format is
specified in [FORMAT.md](FORMAT.md).

```sh
cargo build --release
aqr-png bench [-t N] [-n ITERS] [-v] [-c | -d] FILE...   # round trip check through an archive, ratio, throughput (default 1 thread)
```

```rust
let f = aqr_png::formula::encode(&png, 8);   // None where there is nothing to code
assert!(f.unwrap().eval(&modules)? == png);  // `modules` runs `formula::decode` for `formula::MODULE`
```

## Results and comparison

Measured on 2026-10-24 on an Apple M4, one thread, through `aqr-show` (see the
[top-level README](../README.md#how-it-was-measured)): each file archived at level `mid` with
its round-trip check, then extracted. Ratios are tentative, since the decoder modules are not
in the archives yet. Memory is the peak RSS of that process (input, archive and output
included) over the input size.

| set (codec-corpus) | files | MB | ratio | zstd -3 | compress | decompress |
|---|---|---|---|---|---|---|
| clic2025 | 62 | 228.3 | 0.816 | 1.000 | 45.2 MB/s | 92.4 MB/s |
| CID22 | 208 | 82.8 | 0.855 | 1.000 | 39.3 MB/s | 84.9 MB/s |
| kadid10k | 66 | 22.7 | 0.774 | 1.000 | 47.0 MB/s | 101.7 MB/s |
| qoi-benchmark | 14 | 40.3 | 0.601 | 0.984 | 45.7 MB/s | 104.4 MB/s |
| all (with 6 small sets) | 379 | 391 | **0.802** | 0.997 | **42 MB/s** | **90 MB/s** |

Memory is about 7 MB + 10× the file (the largest, 75 MB for a 6.4 MB photo), against
6 MB + 32× (334 MB) when the inflated data, the raw image and the tokens were held whole.
Most of it is now `aqr-show`'s own: a file of the same size that is not a PNG takes 5-7× in
it. aqr-png itself peaks at about 4 MB + 3.7× the file encoding and 3 MB + 1.1× decoding
(see *Memory* below).

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

- **Literals** are coded as the residual of the raw (unfiltered) byte against aqr-raw's pixel
  model ([`aqr_raw::image::pixel`](../aqr-raw/src/image/pixel.rs), which also codes
  uncompressed images): LOCO-I's MED predictor of its left, upper and upper-left
  neighbours, green and blue adding the previous channel's residual (inter-channel
  decorrelation). The row's PNG filter is then reapplied to give the byte the stream holds,
  so the PNG filter choice does not matter.
- **Contexts**: channel, local activity and whether a match came just before; filter bytes
  have a table of their own.
- **Matches** start as an escape symbol in the literal alphabet, so a literal costs a
  single symbol. Lengths are coded by activity, distances by length bucket, with DEFLATE's
  extra bits raw.
- **Entropy coding** (`src/rans.rs`, on `aqr_common::rans32`): static per-image tables (two
  passes when compressing), four interleaved 32-bit rANS states. The encoder divides by
  multiplying with packed reciprocals; the decoder looks symbols up in a full table per
  context and renormalises without a branch.
- **Other pixel formats** (bit depths other than 8, palettes, 16-bit) are coded byte-wise
  in the filtered domain, by byte position and the filtered neighbours' magnitudes.
- Interlaced images are handled as their seven passes, each with its own rows.

### Representation (`src/codec.rs`)

- An image's payload may hold several zlib streams one after the other; Apple's iDOT files
  use one stream with a full flush, which needs nothing special.
- Huffman headers go through aqr-flate's header coder, against the frequencies of the
  block's tokens: zlib's own trees cost a bit.
- An image is kept as it is if its record would not be smaller. Anything unexpected (not a
  PNG, broken streams, invalid headers, truncation) is kept, or gives no formula; a panic
  while encoding gives no formula either.

### Against a reference

A PNG file can be coded against a similar one (`formula::encode_against`, the module's
fourth input): sets of variants of a picture, with layers shown or hidden ("差分"), are
mostly the same pixels, and the same writer mostly makes the same parse of them. Each token
where the reference's image has one starting at the same place of the data is first a
decision whether it is that token (by the last decision and whether the reference's is a
match); the tokens that are are coded by nothing else. The reference's tokens come from
parsing its payload again, so the decoder needs its file, not its record.

On seven ZIP archives of such sets (Clip Studio Paint, 684 MB of PNG files in groups of 13
to 88 of the same size), each file coded against a parent along a maximum spanning tree of
how alike their data are (`aqr-png-refprobe --code`): 85% of the tokens are the parent's,
and the records come to 109.5 MB against 491.5 MB alone (4.5×). Files are coded token by
token (the encoder of streams expanding more than 4×), and coded alone too to keep the
smaller record: coding takes 35 s and decoding 17 s of one thread over the 684 MB.

A set of such files is one application of `png-set` (`formula::encode_set`), of an output
per file, each against an earlier one, its parent: the files are decoded in order, a parent
being given to its children as their reference file.

Tried and dropped: predicting the literals that are not the reference's by the reference's
pixel where their three neighbours are the reference's (tables of their own). 2.6% of the
tokens are such literals over the same pixels, but the records came to 108.5 MB only, and
decoding took twice as long (the reference's data unfiltered).

### Speed

Measured with `aqr-png bench`, `samply`, and retired instruction counts
(`/usr/bin/time -l`). The decoder runs at about 6 instructions per cycle, so what helped
was doing less:

- **Decoding**: the rANS states live in a local copy of the decoder (in registers, not
  behind a pointer); literals are decoded in runs within a row, in a loop specialised by
  the row's filter type, without bounds checks on the offsets the buffer layout keeps in
  range (the data has a row of padding in front, the raw image a zero row and zero left
  margin per pass). Symbol frequencies for the Huffman headers are counted while decoding.
- **Encoding**: the data is parsed with aqr-flate; as its rows are complete, a window of
  them at a time, they are unfiltered and every pixel byte's context and residual computed
  (NEON, 16 bytes at a time), then the window's tokens walked. Streams that expand more
  than 4× (mostly long matches) are modelled as the parser hands over the tokens instead,
  skipping the bytes inside matches.
- **aqr-flate** got two-level Huffman decoding tables (photo streams' 15-bit literal
  codes made its single-level tables 128 KB), pattern copies for short match distances, a
  branch-free token writer and a header reader that takes counted frequencies. Its match
  copy (16-byte chunks, no library call) is aqr-png's too: a `copy_within` per match cost
  the parse 15%.
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

### Memory

Neither side holds the inflated data, the raw image or the stream's tokens whole:

- The data and the raw rows are windows: what the parser and the model may still read (the
  farthest match, 32 KiB, and the row above) is kept, and moved down every MiB or so. The
  image encoder computes the symbols of a window of rows (256 KiB per thread) as soon as its
  rows are parsed and walks its tokens; of the parse it keeps only the matches and stored
  blocks not walked yet. The decoder, and the encoder of streams expanding more than 4×,
  move their buffers on the same way (with whole buffers up to 4 MiB of data).
- The decoder hands each block's tokens to the DEFLATE writer as the block ends: one
  block's tokens at a time.
- What still follows the image: the encoder's symbols, 2 bytes each (about one per byte of
  data on photos), since the static tables are counted over all of them before the rANS
  stream is coded backwards. They are freed before the record is copied out.

Peak heap of `formula::encode` and of the evaluation (`tests/memory.rs` checks it, on a
9.4 MB image), fitted over the 364 files of the sets above that are coded: 4.2 MB + 3.7×
the file for the first, 2.9 MB + 1.1× for the second (the constants are mostly the
windows).

### Threads

The `threads` of `formula::encode` and `formula::decode`: the output is the same whatever the
number of threads, and one thread runs the code above unchanged. One image is coded by one
rANS stream whose contexts come from the decoded pixels, so decoding an image stays serial;
threads overlap what is independent around it:

- An APNG (or any file with several image runs) has its images coded, and rebuilt, in
  parallel: each image makes a record of its own, concatenated in order.
  At most `N` images are in flight.
- A large image (1 MB of data or more) has the symbols of each window computed by stripes
  of rows on several threads, after the window's serial unfilter pass, and the symbol
  histograms counted by chunks and summed.
- Decompressing writes the DEFLATE streams on a thread of their own, block by block as the
  model decodes them (a few blocks in flight): `aqr-png bench -t 4` decompresses the sets
  above at 104 MB/s, against 88 MB/s on one thread.

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
- `tests/memory.rs`: the peak heap of coding a 2048 × 1536 photo-like image stays within
  the symbols and a few times the file (before the windows: 16× and 6.5× the data).
- Unit tests: the rANS coder, the code tables, Paeth against the specification for all
  inputs, unfiltering in pieces against the reference.

`PNGR_WINDOW=N` moves the windows on every `N` bytes, whatever the image's size, so that
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
| `src/main.rs` | Benchmark tool |
| `src/bin/probe.rs`, `src/bin/ransbench.rs` | Research tools: token statistics, rANS speed |
| `bench/` | Comparison with other tools (`bench/fetch.sh` builds them into the workspace's `third_party/`) |

## Next steps

- Speed: keep only the matches when decoding (literals are implied by their positions),
  as the encoder does now, sparing a 6-byte token per literal; this needs a aqr-flate
  writer that takes that form.
- Memory: the encoder's symbols are all that still follows the image. Dropping them needs
  the model run twice (counting, then the symbols again, window by window from the last,
  for the backward rANS coder), or tables per segment of the image (a format change).
- Ratio on photos: a pixel-domain mode for streams aqr-flate reproduces exactly (zlib),
  where a stronger image model would replace the LZ77 parse, at zlib's speed.
- More pixel formats in the raw-domain model (16-bit, palettes by index similarity).
