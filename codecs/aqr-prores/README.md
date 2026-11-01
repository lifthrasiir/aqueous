# aqr-prores

Apple ProRes (SMPTE RDD 36; 422 Proxy to 4444 XQ, not ProRes RAW) recompression. So far the
bitstream alone: a frame is parsed into its headers and its slices' quantized coefficients,
and written again from them bit for bit. Modelling the coefficients (as aqr-jpeg does for
JPEG's) is what is left for any saving.

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

## Status

`examples/check.rs` (`aqr-prores-check FILE_OR_DIR...`) finds the frames and checks them.
On the local corpus (`F:\Data\aqr-prores-corpus`, see its README: Apple, ARRI, FFmpeg's
`prores_ks` and `prores_aw`, 2K to UHD, MOV and MXF, interlaced, alpha) every frame is
rebuilt.

Left:

- A model of the coefficients (neighbouring blocks, the quantizer and matrices), and the
  formula and module.
- Alpha: its run-length coding parsed and modelled.
- Checking the decoding's meaning (scan order, signs, block placement) against a decoder's
  pictures before modelling: the round trip alone holds for any consistent reading.
