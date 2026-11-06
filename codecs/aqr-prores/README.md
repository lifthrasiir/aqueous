# aqr-prores

Apple ProRes (SMPTE RDD 36; 422 Proxy to 4444 XQ, not ProRes RAW) recompression: a frame is
parsed into its headers and its slices' quantized coefficients, which a context model codes
again with the previous frame as context, and the frame is written again from them bit for
bit. The module `prores-recons` (`FORMAT.md`) rebuilds a chunk of a QuickTime or MXF file.

Frames delimit themselves (a big-endian size, then `icpf`), so `find` takes them out of any
container without reading it; aqr-compress takes `.mov` and `.mxf` files, and others that start
as QuickTime or MXF with a frame in their first 64 MiB.

Written from the specification's description of the bitstream; no decoder's code was read.
The test fixtures (`tests/data`, `scripts/gen-fixtures.sh`) are ffmpeg's test patterns through
its two encoders.

## What a frame holds

- The frame header, the pictures' headers and the slices' headers are kept as stored; the
  sizes in them (picture, slice table, components) are derived from the coded data.
- A slice's Y, Cb and Cr: DC coefficients (differences, the codebook picked by the previous
  one, the sign carried over), then AC coefficients as runs and levels interleaved across the
  slice's blocks; the codewords are Rice codes escaping to exp-Golomb. The coefficients
  determine the bits, but for whole zero bytes after the last codeword (recorded, none seen so
  far) and empty components (chroma of gray encodes).
- Alpha (4444 with alpha) is kept as stored, for now.
- Bytes after the pictures within the frame's size: zero stuffing in Apple's frames
  (64–128 bytes), kept.

## The formula (`src/formula.rs`, `src/codec.rs`)

The frames are cut into chunks of about 64 MiB (a chunk ends also where more than 4 MiB lie
between frames). A chunk's span, from its first frame to its last, is an application of
`prores-recons`, whose inputs are the coded chunk and the bytes between its frames (audio,
the container's headers); the bytes outside the spans and between frames are one stream,
`prores/rest`, in the file's order.

A coded chunk is streams of adaptive binary arithmetic coding (aqr-common's `arith`) and raw
bits. The first tells the frames: the gaps' lengths, the frames kept as they are (those that do
not parse), the headers as differences from the previous ones, the slices' headers, tails and
alpha. Each of the others codes a stripe of rows of every picture, with a model of its own and
the same stripe of the frame before as context. So the stripes decode at once (tasks in
WebAssembly, 16 slots), each writing its slices' bytes; the frames are put together from them.

A chunk has a stripe for each 4 MiB of its frames, rounded up (at most 64), whatever the
threads. Splitting rows costs little spatial context (SD at 32 stripes, a slice row each, lost
0.1%); what costs is each stripe's model learning anew, which shows where little is coded: of
150 frames of each file in one chunk, by 1, 8, 16 and 32 stripes, Apple's animated film
0.7347, 0.7414, 0.7457, 0.7516, while camera footage gained a little (ARRI 2K 0.5517, 0.5498,
0.5493, 0.5491) and decoding on 16 threads went 13, 71, 100, 108 MB/s. Stripes by size keep
their cost a small part of any chunk, and a large file's 64 MiB chunks decode on 16 threads.

The parameters give what decoding holds (blocks of a frame and of a stripe, bytes of a frame,
of the headers and of what each stripe writes, and the counts of the records and bytes the
first stream makes), exactly as the encoder found it; the decoder holds to them. The module
lays its memory out as a stack (`aqr_api::stack`), so its limit is its plan (`codec::plan`):
the instance's memory and as many stripes' tasks at once as its 16 slots allow, each a copy of
the instance with its own on top.

## The model (`src/model.rs`)

One code path for both directions (`Coder`). Each picture's planes (`src/plane.rs`: blocks put
in place, in a macroblock TL, TR, BL, BR) are coded block by block in raster order:

- DC: predicted by the previous frame's DC, else the median of the left, above and gradient;
  the residual's size in unary, sign modelled, the rest raw.
- The number of nonzero AC coefficients (six bits, a tree), in the context of the
  neighbours' counts and the previous frame's.
- AC in ProRes's scan order until that many are found: a zero flag and the size (bit length,
  unary) in the context of the expected magnitude (twice the previous frame's coefficient;
  else the blocks above, left, above left and above right, and the coefficients above and left
  in the block itself), the scan band and the nonzeros left. The top two mantissa bits and the
  sign are modelled with the previous frame's coefficient; signs without one, and lower
  mantissa bits, are raw.
- A frame that does not parse is kept as it is. One that parses needs no check: every
  coefficient set has one coding, padding is zero and recorded, sizes are derived.

The contexts were chosen with `examples/model.rs` (static entropy of sampled frames by
context; environment variables pick the variants). Sampled from the corpus, against the coded
bytes: neighbours alone 0.850; median DC, frequency neighbours, mantissa bits 0.830; the
previous frame for AC sizes 0.793, for DC 0.775, for signs and mantissa 0.738. Coding the
difference from the previous frame instead helps static footage and hurts noisy footage, about
even in all. The other block placement and a raster scan cost more, which confirms both.

## Status

Tools (`cargo run -r -p aqr-prores --example ...`):

- `aqr-prores-check [--zstd] FILE_OR_DIR...`: finds the frames and checks that each is written
  again bit for bit (with `--zstd`, zstd -3 of the frames and of their parse).
- `aqr-prores-bench [-c CHUNK_MB] [-n FRAMES] [-t THREADS] FILE_OR_DIR...`: codes the frames
  in chunks and decodes them.
- `aqr-prores-model [-n FRAMES] FILE_OR_DIR...`: the context estimator above.

The local corpus is `F:\Data\aqr-prores-corpus` (see its README: Apple, ARRI, FFmpeg's
`prores_ks` and `prores_aw`, 2K to UHD, MOV and MXF, interlaced, alpha; 16.4 GB). Every frame
of it is written again bit for bit; zstd -3 takes the frames to 0.9902.

Measured 2026-11-01 on a Ryzen 7 9700X (8 cores, 16 threads), `cargo aqr -c --stats` of the
corpus directory (33 files, 16.89 GB) at level mid, against the build before aqr-prores
(`cache/aqr-corpus.txt`):

| | before | after |
|---|---|---|
| archived | 16.72 GB (0.990) | 11.42 GB (0.676) |
| compressed in | 39 s | 371 s (46 MB/s; the largest file, 5.8 GB, 113 s on 12 threads) |
| tested (`aqr -K`, then `-t`) | | 130 s (130 MB/s; the largest file 61 s on 10.4 threads) |
| heap peak | 17.5 GB | 20.5 GB compressing, 11.9 GB testing |

Compressing, aqr-compress decodes every application to check it (prevalidate): of its 3,250
thread-seconds, 1,720 decode and 1,230 encode (`cache/aqr-corpus-stripes.txt`). With 8 stripes
and 32 MiB chunks it was 11.46 GB, tested in 153 s.

The peaks are the archiver's: it holds the largest files whole (17.5 GB before as well), and a
leaf's output is its file. By file, from the bench (`examples/bench.rs`, 32 MiB chunks): tripod
ARRI 2K 422 HQ 0.54–0.55, ARRI 4444 0.67–0.83, Apple's animated film 0.63, the SD tape capture
0.82, ffmpeg's encodes 0.35–0.73 of their frames. A chunk codes at about 15 MB/s on a thread
and decodes at about 13 (about 100 MB/s with 16 stripes at once).

Left:

- Speed, decoding first (it is more than half of compressing too): writing the slices is about
  a quarter of decoding; about 8–9 binary decisions per input byte, mostly zero flags (3.8 a byte) and sizes
  (1.8); the arithmetic coder itself is about 15% of the encoding time, the rest the decisions'
  contexts and branches. Fewer decisions (zero runs coded together, sizes binarised around
  their expected value) are the way to go faster.
- aqr-compress's weighing of the formula against zstd of the file runs on one thread at the
  end (3.7 s for 1.3 GB) and could be skipped for ProRes, as for PPMd.
- Alpha: its run-length coding parsed and modelled (0.3 MB of the corpus).
- Apple's own encoders as a sample: AVFoundation (`avconvert`, `prores_videotoolbox`) and iPhone
  recordings; the corpus has one Apple-encoded film.
- MP4 or Matroska holding ProRes are not taken (`.mp4` stays compressed data; Matroska goes to
  aqr-mkv).
