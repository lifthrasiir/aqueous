# aqr-tex

Block-compressed textures (BC1 to BC5: DXT1–DXT5, ATI1, ATI2) rebuilt bit for bit from one
coded stream (`FORMAT.md`), and DDS files taken apart around them: the header and whatever
follows the blocks kept as they are, the blocks given by the module `bcn-recons` from their
layout (format, size, mipmaps, layers). DDS files of uncompressed 8-, 24- and 32-bit pixels go
surface by surface to aqr-raw's `raster-recons`.

The module knows nothing of DDS: a recompressor that finds blocks of a known layout in a format
of its own (KTX, an engine's bundles) applies it the same way (`formula::apply`).

## How it does

Measured 2026-10-28 on an AMD Ryzen 7 9700X (8 cores, 16 threads), Windows, on the textures of
The Crew (52,448 DDS files, 16.4 GB; `F:\Data\crew-tex`), every file encoded, decoded and
compared by `aqr-tex-bench` (the ratio is the coded blocks and the rest of the file under
zstd -19; zstd -19 is of each whole file).

| format | files | MB | ratio | zstd -19 |
|---|---|---|---|---|
| BC1 | 25,790 | 8,194.9 | 0.4722 | 0.5773 |
| BC2 | 645 | 118.4 | 0.2209 | 0.3084 |
| BC3 | 12,701 | 6,138.9 | 0.1238 | 0.1631 |
| BC5 | 13,045 | 1,906.7 | 0.3461 | 0.4351 |
| all | 52,181 | 16,359.0 | 0.3249 | 0.4034 |

Of the rest, 230 files of uncompressed pixels (A8, RGB and ARGB of 32 bits) go to aqr-raw, and
37 of half floats (D3DFMT 111 and 113) are not taken.

Coded in one stripe a surface (before stripes, below), all of them came to 0.3240.

- Speed, one thread, on a sample of 401.6 MB (1,274 files by the hash of their paths): 22.9 MB/s
  encoding and 20.3 MB/s decoding of the blocks (BC1 19.4 / 17.4, BC3 30.9 / 27.2, BC5 24.2 /
  21.0). On an M4 (2026-11-01), a sample of 402.4 MB (1,271 files, every 41st path): 31.9 and
  27.1 MB/s (BC1 26.3 / 21.0, BC3 43.4 / 41.1, BC5 31.1 / 29.2), against 26.6 and 20.9 before the
  coding was made faster (below, "Speed"); `aqr -t` of 255 of those files (105.5 MB) on one
  thread, 3.20 s (as WebAssembly 3.85 s).
- One file on several threads (stripes), the 45 largest files of BC1, BC3 and BC5 (209.7 MB,
  2.8 and 5.6 MB each), a file at a time, decoding: 22.8 MB/s on one thread, 60.5 on 4, 79.5 on
  8, 96.8 on 16 (encoding 26.1, 66.7, 87.0, 101.5); BC3 alone 28.1 → 108.3. Through `aqr -d`,
  the largest BC3 file (5.6 MB, archived in 1.9 MB) in 353 ms on one thread, 136 on 8, 131 on
  16, process included (with the modules as WebAssembly: 462, 208, 177 ms).
- Through `aqr` (16 threads), the `gui` directory (380.7 MB, 3,420 files) archives to 61.1 MB in
  4.4 s and decodes in 3.0 s (3.5 s with the modules as WebAssembly); zstd alone (`-l 1`) gives
  100.2 MB, and stripes cost 0.4% of it (60.85 MB without).
- Where the bits go (bytes per byte of the files, all of them): indices 0.255, the cache ranks
  0.043, the choices 0.011, new endpoints 0.015.

## Design

Measured on the first 400 files of the sample (123 MB) by code length, before the coder was
written (the bits of each choice are of BC1 unless said):

- **Order.** A layer's mipmaps are coded from the smallest, and each pixel is predicted from the
  next smaller mipmap upsampled (bilinear, 9-3-3-1): the indices of BC1 0.400 → 0.364 bytes per
  byte. This sees the pixels to the right and below, which the causal neighbours do not. Both
  predictions go into the context, each as a bucket of its projection on the line between the
  endpoints (a quarter of a level): the causal one adds 0.25% on BC1 but 10% on BC3 and some
  on BC5.
- **Indices as levels.** An index is coded as its place on the line between the endpoints,
  under the contexts of where the predictions fall on it, the contrast of the endpoints and the
  mode. Contexts of the left and upper indices of the block, of whether the extremes were seen
  in the block, and of the indices of the block whose endpoints were copied gained nothing.
- **Endpoints.** These textures reuse endpoints far more than neighbours predict: 35% of BC1
  blocks have those of the block to the left or above, and most of the rest those of a block
  used not long before. Copies of the left or upper block: 0.143 → 0.130; a cache of recently
  used endpoints by MTF rank on top: 64 entries 0.095, 1024 0.086, 4096 0.085 (zstd -19 of the
  endpoints alone, 0.100). The rank is kept over a window of times, renumbered when full: a bit
  a time (set at each key's last use) and counts of groups of 1,024 times, so that a use flips
  two bits and two counts, and a rank counts the bits of at most a group and the 16 counts
  (rather than a Fenwick tree, whose loops of varying length were mispredicted: 10% of the
  time). Whole blocks recurring further than the neighbours are about 2% of blocks.
- **Stripes.** A surface of 32,768 blocks or more is coded in stripes of rows, one per 16,384
  blocks and 16 at most, so that one file decodes on several threads (natively on the pool's
  workers, as tasks in the Wasm module). Only the stripes of a surface are apart: its mipmap
  waits for the one before, as it is predicted from it. The first stripe goes on with the
  texture's models and stream; the others start from a copy of the models as they are before
  the surface, with streams of their own, and what the copies learn is dropped, so that a task
  gives back nothing but blocks. Measured on the sample (bytes per byte against 0.3464 unsplit):
  a fixed number of stripes for every surface costs 0.6% (2), 1.4% (4), 2.7% (8), 4.6% (16), as
  small textures and small mipmaps learn anew; a stripe per 4,096 blocks 3.0%, per 16,384 0.85%,
  and from a copy of the models rather than anew 1.0% and 0.29%. The stripes' streams are an
  input of their own, behind the first stream: in front of it, their packed header (of lengths
  that differ) kept the archive from finding textures alike (the `gui` directory 61.10 → 61.81
  MB).
- **Coder.** aqr-common's binary arithmetic coder, its adaptive bits learning at 1/(n + 1.5)
  first: against multi-symbol frequencies of a fixed rate, 0.551 → 0.546, as many small
  textures have few blocks to learn from. One code walks the blocks for both encoding and
  decoding (`coder::Side`), so the two cannot differ.

## Next

- **Speed.** A stripe is one serial chain: each pixel's context needs the pixel to its left,
  decoded just before. The arithmetic coding itself takes about 3 ns a decision (some 36 a BC1
  block); the rest is that latency. Done (2026-11-01, M4, the sample above: 139.6 → 111.7 G
  cycles for both ways): the coder's bits by selects rather than branches (indices are hard to
  predict), the decoder reading a tree's children before the bit that picks one, the products
  of the buckets taken beforehand, the pixel of each level looked up at once, the cache's
  ranks without a Fenwick tree, and upsampling by columns taken once. Tried and slower: the
  coder's renormalization without a branch (a shift by the bytes to go; more instructions than
  the mispredictions it saves), and the decoder's read-ahead on the encoder too. Across stripes, what is left serial bounds one file: the
  mipmaps too small to split (about 6% of a 2048×2048 texture's blocks) and a stripe of each
  split one, about 6.4 times faster than one thread at best for that texture.
- **Formats.** BC6H and BC7 (modes and partitions, then the same models of endpoints and
  indices); KTX and KTX2 containers; ETC2 and ASTC.
- **Mipmaps generated.** Where a mipmap is the encoder's output on the box filter of the one
  above, the encoder could be reproduced; whether this corpus's are was not checked.

## Usage

```sh
cargo build --release
aqr-tex-bench [-j FILES] [-t THREADS] [-z LEVEL] FILE... | @LIST   # ratio by format, speeds (a file on -t threads), round trip checked
```

```rust
use aqr_tex::formula;
// A DDS file taken apart (None where it is not one):
let f = formula::encode(&input, threads).unwrap();
// Or blocks of a known layout found by a recompressor of its own, as an input of its formula:
let node = formula::apply(&mut its_formula, blocks, &layout, threads);
```

## Layout

| File | Contents |
|---|---|
| `src/layout.rs` | Formats, the surfaces of a layout, the module's params |
| `src/dds.rs` | DDS headers (with DX10): where the data is, and what it is |
| `src/bc.rs` | What blocks decode to: colours, values, levels |
| `src/coder.rs` | The two sides of the coder, trees and tokens of adaptive bits |
| `src/model.rs` | Planes and predictions, the cache, the models of colours, values and BC2's alpha |
| `src/texture.rs` | The walk over a texture's surfaces, the stripes and their threads |
| `src/module.rs`, `src/formula.rs` | The module `bcn-recons`, and formulas of DDS files |
| `tests/dds.rs` | Every format and layout (mipmaps, cube maps, DX10 arrays, odd sizes), stripes on any number of threads, pixels, trailers, files not taken, corrupted inputs |
| `examples/bench.rs` | `aqr-tex-bench` |
