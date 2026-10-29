# aqr-raw

Uncompressed sampled data, PCM audio and raster images, coded losslessly: for any format whose
samples are stored as they are (or close to it), rather than for one format. It works both
ways round:

- **As a library**, the models. Other recompressors code what their formats decode to with
  them: aqr-flac codes the residuals of FLAC frames with the audio model
  ([`audio`](src/audio)), aqr-png the pixels behind DEFLATE's literals with the pixel model
  ([`image::pixel`](src/image/pixel.rs)), and aqr-pdf shares the pixel model's predictor.
- **As modules of entry formulas** ([`formula`](src/formula)): `pcm-recons`, `raster-recons`
  and `raster-recons-16` (16-bit samples) each give the samples of a layout (their params)
  from the coded streams, and nothing else. A file is the concatenation of its container's
  bytes before the samples, the application, and its bytes after them, so the container's own
  bytes are kept as they are and never derived. [`container`](src/container.rs) takes the
  trivial containers apart (WAV, AIFF, BMP, PNM, uncompressed TIFF, DICOM), and finds 16-bit
  samples after a header that tells their size in data of no format (`container::sized`). A
  recompressor that finds samples in a format of its own applies the modules the same way
  (`formula::apply`).

The format is specified in [FORMAT.md](FORMAT.md). A new container is a parser that finds the
samples and their layout ([`container::Found`](src/container.rs)); a new kind of sample
(float audio) is a layout and a model.

## Results

Measured on 2026-10-24 on an Apple M4, one thread, with `aqr-raw bench -n 2` (formula and `.aqr`
archive at level `mid`, then the archive decoded and compared).

| set | files | MB | ratio | zstd -3 | formula | decompress |
|---|---|---|---|---|---|---|
| WAV (aqr-flac's `corpus/synth`, 16- and 24-bit) | 20 | 223 | 0.420 | 0.962 | 193 MB/s | 203 MB/s |
| BMP (8-bit gray, 24-bit, 32-bit), PGM, PPM | 17 | 52.4 | 0.223 | 0.400 | 259 MB/s | 308 MB/s |

The images are 17 PNG files of aqr-png's `codec-corpus` of 200 KB or more (every 25th by
path) written by ImageMagick as BMP3, BMP4 with alpha, PGM and PPM in turn; as PNG files
they were 19.2 MB (0.367). The peak RSS of `aqr-raw bench` on one image is 4–5× its size
(input, formula, archive and output included).

The recipe does not pin the set down (which 25th, the depth and alpha of each output): made
again on 2026-10-24 from the files of 200 KiB or more, from the third on (17 files, 52.8 MB;
zstd -3 0.421), they come through `aqr-show` (one process per file, at `da98d11`) to 0.228 at
71 / 303 MB/s, with a peak RSS of 21 MB + 5.5× the file (a linear fit).

Before the PCM path moved here from aqr-flac, it gave the same streams for these files, byte
for byte (the header and trailer are slices of the formula now, where they were streams of
the module). The conformance files of `codec-corpus` (176 BMP and PNM
files, valid and not) round-trip; 97 of them are not coded (fewer than 8 bits per pixel, RLE,
16-bit or ASCII samples).

16-bit rasters (2026-10-29, M4, one thread, `aqr-raw bench`): silesia's `x-ray` (1900 × 2230,
12-bit, after a header of its own, 8.47 MB) 0.421 and `mr` (DICOM, 19 frames of 512 × 512,
9.97 MB) 0.203, at 115 and 174 MB/s, decoded at 171 and 205 MB/s; zstd -3 0.718 and 0.355
(as archives of aqr before: 0.497 and 0.273, mostly aqr-infer-recs' transpose). Six of the
eight uncompressed TIFF files of the Waterloo set (7.26 MB) come to 2.93 MB from 3.94 MB; the
two synthetic ones (`frymire`, `serrano`) code larger and are kept as they were.

## Usage

```sh
cargo build --release

aqr-raw bench [-t THREADS] [-n ITERS] [-v] [-r LAYOUT] FILE...   # ratio and speed, the archive round trip checked
aqr-raw info FILE...                                            # the container and where its samples are
```

`-r LAYOUT` takes the input as headerless samples: PCM as `s16le:2` (sign, bits,
endianness, channels), or a raster as `640x480x3` (width, height, channels).

```rust
use aqr_raw::formula;
// A container taken apart (None where it is not one):
let f = formula::encode(&input, 1).unwrap();
// Or samples found by a recompressor of its own, as an input of its formula:
let node = formula::apply(&mut its_formula, samples, layout, 1);
```

## Layout

| File | Contents |
|---|---|
| `src/audio/frame.rs` | Frames as FLAC models them: subframes (constant, verbatim, fixed, LPC, wasted bits), sample synthesis |
| `src/audio/model.rs` | The residual model: sign-sign LMS stage (NEON), scale contexts, token / raw bit coding |
| `src/audio/residual.rs` | The pipeline over frames (state across frames, built-in tables), subframe parameter serialisation |
| `src/audio/par.rs` | Pipelines for more than one thread: batches, stages, pool, reordering |
| `src/audio/pcm.rs` | Interleaved PCM: sample formats, the block LPC encoder, decoding |
| `src/image/pixel.rs` | The pixel model: MED, channel decorrelation, activity buckets (scalar and NEON) |
| `src/image/raster.rs` | Raw rasters: the coder of 8-bit rows of 1 to 4 channels |
| `src/image/raster16.rs` | Raw rasters of 16-bit samples: MED or mean prediction per chunk, tokens by rANS and raw bits |
| `src/container.rs` | WAV, AIFF / AIFC, BMP, PNM, TIFF, DICOM, and samples after a header telling their size: where the samples are, and their layout |
| `src/formula/` | The modules `pcm-recons` and `raster-recons`, and formulas of containers |
| `model/audio.bin` | The residual model's built-in tables (trained by `aqr-flac-train`) |
| `tests/pcm.rs`, `tests/data` | WAV / AIFF / AIFC files and headerless PCM: round trips on threads, junk, cuts, corrupted inputs, the resource |
| `tests/raster.rs` | Synthetic BMP and PNM files: round trips, trailers, cuts, corrupted inputs |

## Design

### Audio

FLAC's model of a frame is the common ground: FLAC files parse into it exactly (aqr-flac), and
PCM is cut into it here.

- **Residual model**: FLAC's predictor stays, its residuals are the input. A sign-sign LMS
  stage over the last 32 residuals of the channel follows, with its weight update applied two
  samples late (delayed LMS: 0.02 pp, but out of the dependency chain). The LMS error is coded
  as a token under static rANS tables, by a scale context from a running mean of its zigzag
  value, and raw bits; the bits read depend only on the token and the context. On real CD
  material about 1.2 pp come from the entropy coding (against FLAC's partitioned Rice
  parameters) and about 2.3 pp from the LMS stage.
- **PCM**: blocks of 4096 samples become FLAC-like frames: stereo decorrelation (left / side,
  side / right, mid / side) chosen by the sum of second differences, wasted bits, constant
  blocks, and per channel a quantised LPC predictor (Tukey-windowed autocorrelation,
  Levinson–Durbin, 14-bit coefficients). The order (up to 16) is chosen per block from the
  Levinson prediction errors against four times the cost of the extra coefficients and
  warm-up samples; the LMS stage takes over much of what a higher order would gain.
- **Speed**: separate passes per frame; LMS on two channels in one loop with four independent
  NEON accumulators and the newest term in scalar code (about 11 cycles per residual each
  way); rANS with flat alias tables, bounds checks per frame, raw bits in two streams by
  channel parity (about 12 cycles per residual to decode, 7 + 4 to encode).
- **Threads** (the formula is the same whatever their number): a pool makes the frames of the
  blocks ahead, one thread runs the LMS stage in order, the calling thread codes. Decoding is
  a pipeline of subframe parameters → error decoding → LMS stage, then a pool reconstructs
  the samples straight into their place in the output.

### Images

- **Pixel model**: a sample is predicted by LOCO-I's MED from its left, upper and upper left
  neighbours; the second and third channels of three or more add the previous channel's
  residual (green and blue from red, or their counterparts in BGR). The context is the
  channel and the activity bucket (the neighbours' gradients plus, in a raster, the
  magnitudes of the left and upper residuals; in aqr-png, those of the filtered bytes).
- **Raster coder**: the residual symbols with static rANS tables per chunk of 2^22 samples
  (aqr-png's table coding), so the encoder holds a chunk's symbols, and both sides only the
  current and previous rows besides.

## Open issues and limitations

- **Not covered**: 16-bit images (PNM, PAM, TIFF), fewer than 8 bits per pixel, RLE BMP, ASCII
  PNM, TGA (no reliable signature), float audio, more than 8 channels, compressed AIFC, CAF,
  W64, RF64. 16-bit images need a model of their own (the audio model's token and raw bits
  are a likely start).
- **Other containers** with samples inside (TIFF strips, PSD raw channels, DICOM, tracker
  modules, SoundFont) are for their recompressors to take apart with `formula::apply`; data
  of the formula kind `Pixels` carries no layout yet, so aqr-compress cannot route it here.
- BMP and PNM are only looked for at the start of a file: two bytes are too weak a signature
  to scan other data for.
- The raster encoder is scalar; aqr-png's NEON row pass shows the way if it matters.
