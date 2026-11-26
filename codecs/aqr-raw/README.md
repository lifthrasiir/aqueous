# aqr-raw

Uncompressed sampled data, PCM audio and raster images, coded losslessly: for any format whose
samples are stored as they are (or close to it), rather than for one format. `FORMAT.md`
specifies the modules, and how it works. It works both ways round:

- **As a library**, the models. Other recompressors code what their formats decode to with
  them: aqr-flac codes the residuals of FLAC frames with the audio model
  ([`audio`](src/audio)), aqr-png the pixels behind DEFLATE's literals with the pixel model
  ([`image::pixel`](src/image/pixel.rs)), and aqr-pdf shares the pixel model's predictor.
- **As modules of entry formulas** ([`formula`](src/formula)): `pcm-recons`, `raster-recons`
  and `raster-recons-16` (16-bit samples) each give the samples of a layout (their params)
  from the coded streams, and nothing else. A file is the concatenation of its container's
  bytes before the samples, the application, and its bytes after them, so the container's own
  bytes are kept as they are and never derived. A recompressor that finds samples in a format
  of its own applies the modules the same way (`formula::apply`).

A new container is a parser that finds the samples and their layout
([`container::Found`](src/container.rs)); a new kind of sample (float audio) is a layout and a
model.

```sh
cargo build --release --example aqr-raw   # target/release/examples/aqr-raw

aqr-raw bench [-t THREADS] [-n ITERS] [-v] [-r LAYOUT] FILE...   # ratio and speed, the archive round trip checked
aqr-raw info FILE...                                            # the container and where its samples are
```

`-r LAYOUT` takes the input as headerless samples: PCM as `s16le:2` (sign, bits,
endianness, channels), or a raster as `640x480x3` (width, height, channels).

`aqr` takes headerless PCM where the attributes say so (docs/hints-design.md): `aqr-format=raw`
with `aqr-bits` (8, 16, 24 or 32; 16 by default), `aqr-channels` (2), `aqr-endian` (`le` or
`be`; `le`) and `aqr-sign` (`signed` or `unsigned`; unsigned of 8 bits alone), the bytes after
the last whole frame as they are (aqr-compress's format `pcm`, never detected).

```rust
use aqr_raw::formula;
// A container taken apart (None where it is not one):
let f = formula::encode(&input, 1).unwrap();
// Or samples found by a recompressor of its own, as an input of its formula:
let node = formula::apply(&mut its_formula, samples, layout, 1);
```

## What is taken

- Containers ([`container`](src/container.rs)): WAV (PCM, IMA ADPCM and Microsoft ADPCM; the
  ADPCM blocks are aqr-ima's `ima-recons` and aqr-msadpcm's `msadpcm-recons`), AIFF / AIFC,
  BMP, PNM, uncompressed TIFF, DICOM (PNM, TIFF and DICOM of 8- or 16-bit samples), and 16-bit
  samples after a header that tells their size in data of no format (`container::sized`).
- Modules: `pcm-recons` (version 2, a part module), `raster-recons` (8-bit rows of 1 to 4
  channels) and `raster-recons-16`. A raster of more than 48 MiB is cut into bands.
- Not taken: fewer than 8 bits per pixel, RLE BMP, ASCII PNM, TGA (no reliable signature),
  and ADPCM other than IMA's and Microsoft's. Other containers with samples inside (other
  TIFFs, PSD raw channels, tracker modules, SoundFont) are for their recompressors to take
  apart with `formula::apply`.

## Results

Apple M4, one thread, `aqr-raw bench -n 2` (formula and `.aqr` archive at level `mid`, then
the archive decoded and compared), 2026-10-24:

| set | files | MB | ratio | zstd -3 | formula | decompress |
|---|---|---|---|---|---|---|
| WAV (aqr-flac's `corpus/synth`, 16- and 24-bit) | 20 | 223 | 0.420 | 0.962 | 193 MB/s | 203 MB/s |
| BMP (8-bit gray, 24-bit, 32-bit), PGM, PPM | 17 | 52.4 | 0.223 | 0.400 | 259 MB/s | 308 MB/s |

The images are 17 PNG files of aqr-png's `codec-corpus` of 200 KB or more (every 25th by path)
written by ImageMagick as BMP3, BMP4 with alpha, PGM and PPM in turn; as PNG files they were
19.2 MB (0.367). The peak RSS of `aqr-raw bench` on one image is 4-5x its size. Made again
from the files of 200 KiB or more, from the third on (17 files, 52.8 MB; zstd -3 0.421), they
come through `aqr-show` (one process per file) to 0.228 at 71 / 303 MB/s, with a peak RSS of
21 MB + 5.5x the file. The conformance files of `codec-corpus` (176 BMP and PNM files, valid
and not) round-trip; 97 of them are not coded (fewer than 8 bits per pixel, RLE, 16-bit or
ASCII samples).

16-bit rasters (2026-10-29, M4, one thread): silesia's `x-ray` (1900 x 2230, 12-bit, after a
header of its own, 8.47 MB) 0.421 and `mr` (DICOM, 19 frames of 512 x 512, 9.97 MB) 0.203, at
115 and 174 MB/s, decoded at 171 and 205 MB/s; zstd -3 0.718 and 0.355. Six of the eight
uncompressed TIFF files of the Waterloo set (7.26 MB) come to 2.93 MB from 3.94 MB; the two
synthetic ones (`frymire`, `serrano`) code larger and are kept as they were.

16-bit PNM and TIFF (2026-11-10, M4, `cargo aqr -c`, one file an archive): four 576 x 576
photographs of codec-corpus' `gb82` scaled by 1.5 in 16 bits (PPM, 4.48 MB each) 17.92 MB ->
13.54 MB (0.756; zstd -3 0.990); with Gaussian noise (PPM and TIFF) 35.83 -> 30.21 MB (0.843).
The six DSS images of aqr-fits' corpus as PGM and TIFF (891 x 893; 19.03 MB) 8.60 -> 8.27 MB
against aqr-infer-recs' transpose, though one of them grows (0.95 -> 1.07 MB; aqr-fits takes
the FITS files themselves to about 0.27).

IMA ADPCM WAV files (2026-10-31, M4, `aqr -c`, one file an archive): 2 minutes of a live
recording (`aqr-flac-corpus/ia`, 44.1 kHz stereo) by ffmpeg (`adpcm_ima_wav`, blocks of 1024
bytes) 5.33 MB -> 3.06 MB (0.575; zstd -3 0.862, xz -9e 0.712), by sox (`-e ima-adpcm`, 512)
5.37 MB -> 3.26 MB (0.607; zstd -3 0.891, xz -9e 0.751), and in mono by sox 2.68 MB -> 2.10 MB
(0.784; xz -9e 0.797). A file of a few tens of KB stays as it was, its gain smaller than the
module.

A 300 MB 16-bit stereo file needs 18.4 MiB of the application's memory (2224 MiB held whole).
A 126 MB 24-bit BMP is 52.39 MB in three bands (+0.006% against none) instead of stored.

## Tests

- `tests/pcm.rs`, `tests/data`: WAV / AIFF / AIFC files and headerless PCM: round trips on
  threads, junk, cuts, corrupted inputs, the resource.
- `tests/part.rs`: `pcm-recons` as a part module through the native emulation, against the
  whole decoding, on streams far past the windows, in every format, every stream damaged, cut
  and extended.
- `tests/ima.rs`, `tests/ima`: IMA ADPCM WAV files of ffmpeg and sox: round trips, junk, cuts,
  layouts not taken.
- `tests/msadpcm.rs`, `tests/msadpcm`: Microsoft ADPCM WAV files of ffmpeg: the same.
- `tests/raster.rs`: synthetic BMP and PNM files: round trips, trailers, cuts, corrupted
  inputs.
- `tests/hostile.rs`: containers that lie (looping TIFF IFDs, chunks out of order or of sizes
  0 and 0xFFFFFFFF, OS/2 and V4/V5 BMP, malformed DICOM, PAM and PNM headers), and all of them
  damaged: never a panic, hang or allocation the data cannot hold.

`cargo rt -p aqr-raw`.

## Left

- **Float audio and other PCM containers.** Float samples, more than 8 channels, compressed
  AIFC, CAF, W64, RF64 and Apple's IMA4 (AIFC and CAF `ima4`, packets of another layout) are
  not covered; CAF, W64 and RF64 are plain PCM that `pcm-recons` would take with a parser each.
- **Rasters of data without a layout.** Data of the formula kind `Pixels` carries no layout
  yet, so aqr-compress cannot route it here.
- **BMP and PNM signatures.** Only looked for at the start of a file: two bytes are too weak
  a signature to scan other data for.
- **Raster encoder speed.** It is scalar; aqr-png's NEON row pass shows the way.
