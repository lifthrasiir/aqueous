# aqr-prores

Apple ProRes (SMPTE RDD 36; 422 Proxy to 4444 XQ, not ProRes RAW) recompression: a frame is
parsed into its headers and its slices' quantized coefficients, which a context model codes
again with the previous frame as context, and the frame is written again from them bit for
bit. The module `prores-recons` rebuilds a chunk of a QuickTime or MXF file. `FORMAT.md`
specifies the module, and how it works.

Written from the specification's description of the bitstream; no decoder's code was read.
The test fixtures (`tests/data`, `scripts/gen-fixtures.sh`) are ffmpeg's test patterns through
its two encoders.

Tools (`cargo run -r -p aqr-prores --example ...`):

- `aqr-prores-check [--zstd] FILE_OR_DIR...`: finds the frames and checks that each is written
  again bit for bit (with `--zstd`, zstd -3 of the frames and of their parse).
- `aqr-prores-bench [-c CHUNK_MB] [-n FRAMES] [-k STRIPES] [-r DECODINGS] FILE_OR_DIR...`: codes the
  frames in chunks and decodes them (through the module, the stripes on threads).
- `aqr-prores-model [-n FRAMES] FILE_OR_DIR...`: the static entropy of sampled frames by context
  (environment variables pick the variants).
- `aqr-prores-decisions [-n FRAMES] FILE_OR_DIR...`: the model's binary decisions by kind (zero
  flags, sizes, mantissa bits, signs, counts, DC) with the bytes each would cost.

## What is taken

`.mov` and `.mxf` files, and others that start as QuickTime or MXF with a frame in their first
64 MiB: frames delimit themselves (a big-endian size, then `icpf`), so they are found without
reading the container. Every frame of the local corpus is written again bit for bit; a frame that
does not parse is kept as it is. Alpha (4444) and the zero stuffing after the pictures are kept
as stored. Not taken: ProRes RAW, ProRes in MP4 (`.mp4` stays compressed data; Matroska goes to
aqr-mkv).

## Results

The local corpus is `F:\Data\aqr-prores-corpus` (see its README: Apple, ARRI, FFmpeg's
`prores_ks` and `prores_aw`, 2K to UHD, MOV and MXF, interlaced, alpha; 16.4 GB). zstd -3 takes
its frames to 0.9902.

Whole corpus, 33 files (16.89 GB), `cargo aqr -c --stats` at level mid, Ryzen 7 9700X (16
threads), 2026-11-01:

| | zstd -3 | aqr |
|---|---|---|
| archived | 16.72 GB (0.990) | 11.42 GB (0.676) |
| compressed in | 39 s | 371 s (46 MB/s) |
| tested (`aqr -K`, then `-t`) | | 130 s (130 MB/s) |
| heap peak | 17.5 GB | 20.5 GB compressing, 11.9 GB testing |

The peaks are the archiver's, which holds the largest files whole. By file (the bench, 32 MiB
chunks): tripod ARRI 2K 422 HQ 0.54-0.55, ARRI 4444 0.67-0.83, Apple's animated film 0.63, the SD
tape capture 0.82, ffmpeg's encodes 0.35-0.73 of their frames.

A 30-file subset (6.49 GB: FATE, ffmpeg, ARRI), commit `a1ac4866`, same machine, 2026-11-10,
default application bound: 4,503,971,815 bytes (69.4%), compressing 124.9 s, `-K` 50.1 s natively
and 71.3 s with `--wasm`. One thread codes a chunk at about 15 MB/s and decodes at about 22-30
(about 100 MB/s with 16 stripes at once).

## Tests

- `tests/formula.rs`: the formulas of the fixtures rebuild them through the module; damaged ones
  fail without a panic.
- `tests/edge.rs`: `find` on forged headers, frames cut or extended, damaged files and chunks
  (every input of the module, native).
- aqr-decompress's `prores_plans_hold` and `prores_plan_is_independent_of_length` check the
  module's memory plan.

## Left

- **Speed.** Decoding first (it is more than half of compressing too): about 7.5 binary decisions
  per input byte on the corpus (zero flags 2.9, sizes 2.3); the arithmetic coder is about 15% of
  encoding, the parse of ProRes bits a fifth. Sizes binarised around their expected value would
  save decisions on dense footage (not tried: it widens the size table).
- **8K 4:4:4 memory.** Such frames would pass the compressing bound of 256 MiB (a stripe's planes
  are 258 bytes a block of its picture, in two frames; UHD 4:4:4 takes 220 MB). Native decoding
  runs all the gang's tasks at once, so it holds the plan (UHD 4:4:4: 648 MB resident).
- **Bigger chunks.** 1 GiB instead of 64 MiB gains 0.19% on noise (to be measured on the corpus's
  footage), at the cost of the encoder's parallelism and memory.
- **Alpha.** Its run-length coding is not parsed or modelled (0.3 MB of the corpus).
- **Apple's own encoders as a sample.** AVFoundation (`avconvert`, `prores_videotoolbox`) and
  iPhone recordings; the corpus has one Apple-encoded film.
- **ProRes in MP4.** Not taken.
