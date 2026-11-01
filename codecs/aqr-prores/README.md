# aqr-prores

Apple ProRes (SMPTE RDD 36; 422 Proxy to 4444 XQ, not ProRes RAW) recompression: a frame is
parsed into its headers and its slices' quantized coefficients, which are coded again by a
context model with the previous frame as context, and the frame is written again from them
bit for bit. No formula or module yet.

Frames delimit themselves (a big-endian size, then `icpf`), so `find` takes them out of any
container (QuickTime, MXF, Matroska) without reading it.

Written from the specification's description of the bitstream; no decoder's code was read.

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

## The model (`src/model.rs`, `src/codec.rs`)

Frames are coded in chunks (32 MB in the bench), each with adaptive binary arithmetic coding
(aqr-common's `arith`), one code path for both directions (`Coder`). Each picture's planes
(`src/plane.rs`: blocks put in place, in a macroblock TL, TR, BL, BR) are coded block by block
in raster order, with the same plane of the previous frame in the chunk as context:

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
- Headers as differences from the previous ones (most repeat); sizes are derived.
- A frame that does not parse is kept as it is. One that parses needs no check: every
  coefficient set has one coding, padding is zero and recorded, sizes are derived.

The contexts were chosen with `examples/model.rs` (static entropy of sampled frames by
context; environment variables pick the variants). Sampled from the corpus, against the coded
bytes: neighbours alone 0.850; median DC, frequency neighbours, mantissa bits 0.830; the
previous frame for AC sizes 0.793, for DC 0.775, for signs and mantissa 0.738. Coding the
difference from the previous frame instead helps static footage and hurts noisy footage, about
even in all. The other block placement and a raster scan cost more, which confirms both.

## Status

`examples/check.rs` (`aqr-prores-check FILE_OR_DIR...`) finds the frames and checks them.
On the local corpus (`F:\Data\aqr-prores-corpus`, see its README: Apple, ARRI, FFmpeg's
`prores_ks` and `prores_aw`, 2K to UHD, MOV and MXF, interlaced, alpha) every frame is
rebuilt.

`examples/bench.rs` (`aqr-prores-bench [-c CHUNK_MB] [-n FRAMES] FILE_OR_DIR...`) codes
the frames in chunks and decodes them. On the corpus (16.4 GB, Ryzen 7 9700X):

| | ratio | encode | decode |
|---|---|---|---|
| all, 16 threads on 8 cores (MB/s per thread) | 0.7308 | 9.6 | 8.0 |
| 40 frames of three files, one thread (MB/s) | 0.7212 | 15.7 | 12.6 |

By file: tripod ARRI 2K 422 HQ 0.54–0.55, ARRI 4444 0.67–0.83, Apple's animated film 0.63, the
SD tape capture 0.82, ffmpeg's encodes 0.35–0.73. About 8–9 binary decisions per input byte,
mostly zero flags (3.8 per byte) and sizes (1.8); the arithmetic coder itself is about 15% of
the encoding time.

Left:

- Speed, decoding first (`Frame::write` is about a quarter of it).
- The formula and module; chunking by the archive's needs.
- Alpha: its run-length coding parsed and modelled.
