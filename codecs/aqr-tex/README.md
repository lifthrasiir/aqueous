# aqr-tex

Block-compressed textures rebuilt bit for bit from one coded stream (`FORMAT.md`), a module for
each group of formats: `bcn-recons` of BC1 to BC5 (DXT1–DXT5, ATI1, ATI2), `bc7-recons` of BC7,
`bc6h-recons` of BC6H, `etc-recons` of ETC1, ETC2 (RGB, RGBA, punch-through alpha) and EAC (R11,
RG11, signed or not), `pvrtc-recons` of PVRTC1 (4 and 2 bits a pixel), `astc-recons` of ASTC (of
every 2D footprint, LDR and HDR).
The containers are taken apart around them: DDS files (the header and whatever follows the blocks
kept as they are), and KTX, KTX 2, PVR (version 3) and astcenc's `.astc` files (the bytes between
their surfaces kept, the module's output cut into the surfaces where the file has them), the
blocks given by the module from their layout (format, size, mipmaps, layers). DDS files of
uncompressed 8-, 24- and 32-bit pixels go surface by surface to aqr-raw's `raster-recons`.

The modules know nothing of containers: a recompressor that finds blocks of a known layout in a
format of its own (an engine's bundles: Unity's textures in aqr-bundle) applies them the same
way (`formula::apply`).

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
  coding was made faster (below, "Speed"); `aqr -K` (then `-t`) of 255 of those files (105.5 MB) on one
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

### BC7

Measured 2026-11-13 on the Ryzen 7 9700X, on the BC7 textures of 9 games of Unity on Steam (A
Dance of Fire and Ice, Donut County, Ellipsis, Freshly Frosted, Getting Over It, Girls Like
Robots, Glass Masquerade 2, HUMANITY's demo, Hidden Folks: 164 textures, 194.5 MB, dumped by
aqr-bundle's `aqr-bundle-textures`) by `aqr-tex-bench -j 4`: 0.2340 bytes per byte, against
zstd -19's 0.3466 (endpoints 0.066, indices 0.135, the choices and the cache 0.021, the modes
and the like 0.012); 19.5 MB/s encoding and 21.6 decoding a thread. Through `aqr`, 6 of them
each as KTX, KTX 2 and DDS (37.3 MB): 4.05 MB, natively and as WebAssembly given back.

- **Copies.** These textures repeat their blocks: of the largest mipmaps of the first 56 of them,
  67% of the blocks are the block to their left or above, and 18% more have the endpoints (all
  but the indices) of a block before. So a block is first the left or upper one, then its
  endpoints are those of the left or upper one or of a block of the cache (by a hash of them, the
  last block of each kept, as BC1's endpoints are): 0.1583 → 0.1358 on those 56 (zstd -19's
  0.1770), and twice as fast.
- **Endpoints** are predicted from pixels, not from the neighbours' endpoints: the ends of the
  line through the subset's pixels of the smaller mipmap, else through the pixels decoded around
  that touch the subset, their order by the subset's first pixel. Against the mean of the pixels
  around: 0.2466 → 0.2423 on all of them; the endpoints of a neighbour of the same mode and
  partition, where there is one, predicted worse than the pixels (0.2423 → 0.2340 without them).
  The smaller mipmap is the better of the two where there is one (the pixels around alone,
  0.2348; their average, 0.2343).
- **Indices** as BC1's, of 7 kinds (the bits, and whether of all the channels, the colour or the
  alpha); of no smaller mipmap, the pixels above and to the left as the two contexts rather than
  none and MED (0.1358 → 0.1336 on the 56). The resolution of the buckets mattered little (a
  quarter of a level of 3-bit indices, or a whole one of 4-bit: within 0.1%); an edge context
  (which of the left and upper pixels the upper left one is nearer) gained 0.4% for 10% of the
  speed, and is not used.

### Encoders on real images

Measured 2026-11-14 on the Ryzen 7 9700X by `aqr-tex-bench -z 3` (16 files at once, a thread
each; zstd -3 is of each whole file): samples made of real images by encoders run as black
boxes. The images: the 163 BC7 textures above of 64 pixels or more each way, decoded (*game*,
mostly RGBA; opaque formats take their RGB); 16 CC0 textures of Poly Haven at 1024×1024 (diffuse
*photo*, `nor_gl` *normal*, *rough*ness) and 6 of its HDRIs at 2048×1024 (*hdr*). Mipmaps down
to 1×1: etcpak (UnityPy's binding) and astcenc 5.7.1 (`-medium`) were given each mipmap (box
filtered) and the KTX written around them; Compressonator 4.6.0, PVRTexTool 4.17
(`etcfast`, `pvrtcnormal`; PVRTC resized to a power of two square, at most 2048) and
texconv made their own. Every file came back as it was.

| format | encoder | images | files | MB | ratio | zstd -3 | MB/s enc. / dec. |
|---|---|---|---|---|---|---|---|
| ETC1 | etcpak | game, photo | 179 | 136.4 | 0.2022 | 0.3125 | 10.2 / 11.0 |
| ETC1 | Compressonator | game, photo | 179 | 136.4 | 0.2049 | 0.3214 | 10.1 / 11.2 |
| ETC2 RGB | etcpak | game, photo | 179 | 136.4 | 0.2118 | 0.3321 | 11.2 / 13.0 |
| ETC2 RGB | Compressonator | game, photo | 179 | 136.4 | 0.2195 | 0.3484 | 10.3 / 11.6 |
| ETC2 RGBA | etcpak | game | 163 | 250.3 | 0.1267 | 0.2221 | 17.0 / 19.8 |
| ETC2 RGBA | Compressonator | game | 163 | 250.3 | 0.1240 | 0.2139 | 17.9 / 21.6 |
| ETC2 RGB A1 | Compressonator | game | 163 | 125.2 | 0.1634 | 0.2709 | 12.4 / 14.7 |
| ETC2 RGB A1 | PVRTexTool | game | 163 | 125.2 | 0.1650 | 0.2721 | 12.1 / 14.0 |
| EAC R11 | etcpak | rough | 16 | 11.2 | 0.6344 | 0.8584 | 8.9 / 9.4 |
| EAC R11 | PVRTexTool | rough | 16 | 11.2 | 0.6548 | 0.8796 | 8.8 / 9.3 |
| EAC RG11 | etcpak | normal | 16 | 22.4 | 0.7439 | 0.9435 | 7.6 / 8.2 |
| EAC RG11 | PVRTexTool | normal | 16 | 22.4 | 0.7573 | 0.9538 | 7.8 / 8.3 |
| PVRTC 4bpp | PVRTexTool | game, photo | 179 | 101.6 | 0.3109 | 0.4798 | 3.0 / 3.1 |
| PVRTC 2bpp | PVRTexTool | game, photo | 179 | 50.8 | 0.3655 | 0.5333 | 1.3 / 1.4 |
| ASTC 4×4 | astcenc | game, photo | 179 | 272.7 | 0.3477 | 0.4504 | 4.0 / 4.7 |
| ASTC 6×6 | astcenc | game, photo | 179 | 121.7 | 0.4349 | 0.5413 | 3.1 / 3.5 |
| ASTC 8×8 | astcenc | game, photo | 179 | 68.3 | 0.4291 | 0.5414 | 2.3 / 2.7 |
| ASTC 6×6 (`-normal`) | astcenc | normal | 16 | 10.0 | 0.7593 | 0.9552 | 1.6 / 2.4 |
| ASTC 4×4 HDR | astcenc (`-ch`) | hdr | 6 | 16.8 | 0.8142 | 0.9540 | 3.9 / 4.4 |
| ASTC 6×6 HDR | astcenc (`-ch`) | hdr | 6 | 7.5 | 0.8131 | 0.9667 | 3.3 / 4.0 |
| BC6H (UF16) | texconv | hdr | 6 | 16.8 | 0.6605 | 0.9859 | 11.4 / 11.9 |
| BC6H | Compressonator | hdr | 6 | 16.8 | 0.6561 | 0.9829 | 10.9 / 11.6 |
| BC7 | texconv | photo | 16 | 22.4 | 0.6132 | 0.9401 | 6.7 / 7.2 |

The encoders matter little (within 4%). Photographs' textures are noise to these models (BC7 of
texconv on them 0.61, the game's 0.23); the game's textures hold flat areas and repeats. ASTC
and PVRTC are 2 to 8 times slower than the rest (a block of ASTC is a parse of its fields and of
the weights' grid, PVRTC's pixels are blended from four words), which is what to work on next.
Through `aqr`, 3 files of each row (69 files, 86.3 MB): 35.9 MB (zstd -3 49.7 MB), natively and
as WebAssembly given back.

### BC6H

As BC7 (the same copies, endpoints predicted from the pixels, indices by their buckets), of
planes of the endpoints unquantized (16 bits, before they are scaled to half floats), the
endpoints coded as stored (of transformed modes, the differences from the first) against the
prediction quantized. No games' textures of BC6H were at hand (none in the 88 games of Unity
surveyed; HDRIs encoded, above): on a smooth texture of mode 10 alone (`tests/formats.rs`), 0.645 of zstd -19, BC7's
alike 0.51.

### ETC1, ETC2 and EAC

Each part of a block (RGB; EAC of alpha, R11) as BC7's blocks are (copies, then new fields): an
RGB block's mode (individual, differential, T, H, planar), its fields predicted from the pixels
(a subblock's base by the mean of its predicted pixels, its codeword by their spread, the flip
by which split of them spreads less, planar's colours by the pixels at its corners), the bits
of no field (those that only make the overflows of T, H and planar) as they are, so that any 64
bits are a block; its indices as levels on a *ladder* of their values (the sums of a level's
channels), by where the predictions fall on it (T's and H's by the paint colour nearest). No
games' textures of these formats were at hand (none in the games of Unity surveyed, which are of
Windows; real images encoded, above): on smooth textures of `tests/formats.rs`, ETC1's (differential, of the least error)
0.46 of zstd -19, EAC R11's 0.83. The decoding of the blocks, which the planes hold, matched
texture2ddecoder's (MIT, as a black box) on random blocks of every format, but for the colour of
punch-through's transparent pixels (the spec's are black, its not).

### PVRTC1

A word's colours are samples of two images of low resolution, each pixel blended between them
from the 4 words around it (wrapping), so a surface is coded in two passes of no stripes: every
word's colours (copies, or new ones predicted by MED of the words around), then the modulation
of the words not copied whole, each pixel's level on the line between the images there. The
walk takes the words row by row, the data has them in Morton order. On a smooth texture of 4
bits a pixel (`tests/formats.rs`: of each word the least and most of its pixels, the weights
nearest), 0.42 of zstd -19. The pixels matched texture2ddecoder's on random words of both rates,
of 2 bits a pixel once its stored values are of weights 0, 3, 5, 8 and never punch-through (as
PowerVR's decoders; the Khronos description has mode 1's weights). PVRTC2 is not taken.

### ASTC

A block is read into its fields as ASTC reads them, and written back; one that does not come back
as it was (illegal ones, reserved modes) is an error block, kept as it is. Of the integer
sequences, the packed trits and quints of each group are a choice among the codes of its values
(13 tuples of trits and 3 of quints have two codes, and a group cut short several), which an
encoder makes the same way each time: so any bits are some values and choices, the choices cheap.
Blocks are copies as BC7's, or new: their block mode, partitions, endpoint modes, colour values
(from a neighbour of the same configuration, or from the pixels predicted for the modes of direct
and scaled colours) and weights (by where the pixel predicted at each grid point falls between
the endpoints there, and by the weights around it in the grid), in ranks of what they unquantize
to. HDR endpoint modes predict only roughly. On a smooth texture of 4×4 blocks (CEM 8, a grid of
4×4 weights of 3 bits; `tests/formats.rs`), 0.67 of zstd -19. The decoding was within 1 of
texture2ddecoder's on every texel of 485 legal LDR blocks of random bits (it rounds the 8 bits
of the interpolation otherwise).

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
- **Formats.** PVRTC2; ASTC of 3D footprints.
- **BC6H, ETC, EAC, PVRTC, ASTC** are measured on samples of real images made by encoders
  (above, "Encoders on real images"), not on textures of real games (mobile games' packages for
  Android and iOS). ASTC and PVRTC are slow (2-4 and 1-3 MB/s a thread).
- **BC7.** Most of its bits are indices; a model of their levels as an image (residuals from a
  predicted level) rather than the joint buckets of two predictions may do better on 4-bit
  indices. Real textures of mobile formats are missing (the sample is of Unity on Windows).
- **Mipmaps generated.** Where a mipmap is the encoder's output on the box filter of the one
  above, the encoder could be reproduced; whether this corpus's are was not checked.

## Usage

```sh
cargo build --release
aqr-tex-bench [-j FILES] [-t THREADS] [-z LEVEL] FILE... | @LIST   # of DDS or .tex files: ratio by format, speeds (a file on -t threads), round trip checked
```

```rust
use aqr_tex::formula;
// A DDS file taken apart (None where it is not one), or a KTX, KTX 2, PVR or .astc file:
let f = formula::encode(&input, threads).unwrap();
let f = formula::encode_container(&input, threads).unwrap();
// Or blocks of a known layout found by a recompressor of its own, as an input of its formula:
let node = formula::apply(&mut its_formula, blocks, &layout, threads);
```

## Layout

| File | Contents |
|---|---|
| `src/layout.rs` | Formats and their groups, the surfaces of a layout, the modules' params |
| `src/dds.rs` | DDS headers (with DX10): where the data is, and what it is |
| `src/container.rs` | KTX, KTX 2, PVR and `.astc` files: their layout, where each surface is |
| `src/bc.rs` | What blocks decode to: colours, values, levels |
| `src/coder.rs` | The two sides of the coder, trees and tokens of adaptive bits |
| `src/plane.rs` | Planes of 8- or 16-bit samples, their predictions: causal, and the smaller mipmap upsampled |
| `src/model.rs` | The cache, the models of colours, values and BC2's alpha |
| `src/bcn.rs` | BC1 to BC5 as a family of the walk: their parts, planes and models |
| `src/bptc.rs` | BC7's and BC6H's blocks: their fields read and written back, their pixels (bcdec's partition tables and BC6H's layouts) |
| `src/copies.rs` | Blocks as those around or before (the choices, the cache of heads), of BC7 and BC6H |
| `src/bc7.rs` | BC7 as a family of the walk: the models of modes, endpoints and indices |
| `src/bc6h.rs` | BC6H as a family of the walk, as BC7 |
| `src/etc.rs` | ETC1's, ETC2's and EAC's blocks: modes, fields and the bits left over, pixels |
| `src/etcm.rs` | ETC1, ETC2 and EAC as a family of the walk: their parts (RGB, EAC), models and ladders |
| `src/pvrtc.rs` | PVRTC1: its words, images and pixels, Morton order, and its family of two passes |
| `src/astc.rs` | ASTC's blocks: their fields read and written back, integer sequences and their codes, unquantization, partitions, pixels |
| `src/astcm.rs` | ASTC as a family of the walk: copies, new blocks' fields, weights by their grid points |
| `src/texture.rs` | The walk over a texture's surfaces (of any family of formats), the stripes and their threads |
| `src/module.rs`, `src/formula.rs` | The modules, and formulas of DDS files and the other containers |
| `tests/dds.rs` | Every format and layout (mipmaps, cube maps, DX10 arrays, odd sizes), stripes on any number of threads, pixels, trailers, files not taken, corrupted inputs |
| `tests/formats.rs` | The other groups (BC7, BC6H, ETC and EAC, PVRTC, ASTC): random blocks of every mode, stripes, smooth textures; the containers, DDS's DX10 |
| `examples/bench.rs` | `aqr-tex-bench` |
