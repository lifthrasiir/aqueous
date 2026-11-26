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

`FORMAT.md` specifies the modules, and how they work (the walk, the models, the stripes, and the
design notes).

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

## What is taken

- DDS (headers incl. DX10), KTX, KTX 2, PVR (version 3) and astcenc `.astc` files, with the
  blocks of BC1-BC5, BC6H, BC7, ETC1/ETC2/EAC, PVRTC1 and every 2D ASTC footprint (LDR and HDR);
  mipmaps, cube maps and arrays. A block that does not come back as it was (ASTC's illegal ones)
  is kept as it is.
- Uncompressed 8-, 24- and 32-bit DDS files go to aqr-raw's `raster-recons`; half-float DDS files
  (D3DFMT 111 and 113) are not taken.
- A surface of 32,768 blocks or more is coded in stripes, so that one file decodes on several
  threads; stripes cost 0.4% of the size.
- Not taken: PVRTC2; ASTC of 3D footprints.

## Results

Ryzen 7 9700X (8 cores, 16 threads), Windows, 2026-10-28, on the textures of The Crew (52,448
DDS files, 16.4 GB; `F:\Data\crew-tex`) by `aqr-tex-bench` (every file encoded, decoded and
compared; the ratio is the coded blocks and the rest of the file; zstd -19 is of each whole
file):

| format | files | MB | ratio | zstd -19 |
|---|---|---|---|---|
| BC1 | 25,790 | 8,194.9 | 0.4722 | 0.5773 |
| BC2 | 645 | 118.4 | 0.2209 | 0.3084 |
| BC3 | 12,701 | 6,138.9 | 0.1238 | 0.1631 |
| BC5 | 13,045 | 1,906.7 | 0.3461 | 0.4351 |
| all | 52,181 | 16,359.0 | 0.3249 | 0.4034 |

Of the rest, 230 files of uncompressed pixels go to aqr-raw, and 37 of half floats are not
taken. Through `aqr` (16 threads) the `gui` directory (380.7 MB, 3,420 files) archives to 61.1
MB in 4.4 s and decodes in 3.0 s (3.5 s with the modules as WebAssembly); zstd alone gives
100.2 MB.

Speed, blocks of a sample of about 402 MB (1,271 files), one thread, on an M4 (2026-11-01):
31.9 MB/s encoding and 27.1 decoding (BC1 26.3 / 21.0, BC3 43.4 / 41.1, BC5 31.1 / 29.2); on
the Ryzen 22.9 and 20.3. `aqr -K` then `-t` of 255 of those files (105.5 MB) on one thread:
3.20 s (3.85 s as WebAssembly). One file on several threads (the 45 largest BC1, BC3 and BC5
files, 209.7 MB), decoding: 22.8 MB/s on one thread, 60.5 on 4, 79.5 on 8, 96.8 on 16
(encoding 26.1, 66.7, 87.0, 101.5).

**BC7**, on the BC7 textures of 9 Unity games on Steam (164 textures, 194.5 MB, dumped by
aqr-bundle's `aqr-bundle-textures`), Ryzen, 2026-11-13: 0.2340 bytes per byte against zstd -19's
0.3466; 19.5 MB/s encoding and 21.6 decoding a thread. Through `aqr`, 6 of them each as KTX,
KTX 2 and DDS (37.3 MB): 4.05 MB.

**Other formats**, on samples made of real images by encoders run as black boxes (Ryzen,
2026-11-14, `aqr-tex-bench -z 3`; zstd -3 is of each whole file; every file came back as it
was): the 163 BC7 textures above of 64 pixels or more each way, decoded (*game*; opaque formats
take their RGB); 16 CC0 textures of Poly Haven at 1024x1024 (diffuse *photo*, *normal*,
*rough*ness) and 6 of its HDRIs at 2048x1024 (*hdr*), mipmaps down to 1x1, by etcpak, astcenc
5.7.1 (`-medium`), Compressonator 4.6.0, PVRTexTool 4.17 and texconv. Speeds of PVRTC and ASTC
rows are of `-j 8` after the speed work.

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
| PVRTC 4bpp | PVRTexTool | game, photo | 179 | 101.6 | 0.3109 | 0.4798 | 8.9 / 8.8 |
| PVRTC 2bpp | PVRTexTool | game, photo | 179 | 50.8 | 0.3655 | 0.5333 | 4.3 / 4.3 |
| ASTC 4×4 | astcenc | game, photo | 179 | 272.7 | 0.3477 | 0.4504 | 10.4 / 12.1 |
| ASTC 6×6 | astcenc | game, photo | 179 | 121.7 | 0.4349 | 0.5413 | 7.1 / 8.1 |
| ASTC 8×8 | astcenc | game, photo | 179 | 68.3 | 0.4291 | 0.5414 | 5.0 / 5.8 |
| ASTC 6×6 (`-normal`) | astcenc | normal | 16 | 10.0 | 0.7593 | 0.9552 | 1.6 / 2.4 |
| ASTC 4×4 HDR | astcenc (`-ch`) | hdr | 6 | 16.8 | 0.8142 | 0.9540 | 6.4 / 7.7 |
| ASTC 6×6 HDR | astcenc (`-ch`) | hdr | 6 | 7.5 | 0.8131 | 0.9667 | 3.3 / 4.0 |
| BC6H (UF16) | texconv | hdr | 6 | 16.8 | 0.6605 | 0.9859 | 11.4 / 11.9 |
| BC6H | Compressonator | hdr | 6 | 16.8 | 0.6561 | 0.9829 | 10.9 / 11.6 |
| BC7 | texconv | photo | 16 | 22.4 | 0.6132 | 0.9401 | 6.7 / 7.2 |

The encoders matter little (within 4%). Photographs' textures are noise to these models (BC7 of
texconv on them 0.61, the game's 0.23); the game's textures hold flat areas and repeats. Through
`aqr`, 3 files of each row (69 files, 86.3 MB): 35.9 MB (zstd -3 49.7 MB). Real games' textures
of BC6H, ETC, EAC, PVRTC and ASTC were not at hand (none in the 88 Unity games surveyed).

## Tests

- `tests/dds.rs`: every format and layout (mipmaps, cube maps, DX10 arrays, odd sizes), stripes on
  any number of threads, pixels, trailers, files not taken, corrupted inputs.
- `tests/formats.rs`: BC7, BC6H, ETC and EAC, PVRTC, ASTC: random blocks of every mode, stripes,
  smooth textures; the containers, DDS's DX10.
- Pixels decoded match texture2ddecoder's (MIT, a black box) on random blocks of every format,
  but for punch-through's transparent pixels (the spec's are black, its not) and ASTC within 1.

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
| `examples/bench.rs` | `aqr-tex-bench` |

## Left

- **Speed.** A stripe is one serial chain (each pixel's context needs the pixel to its left);
  the arithmetic coding itself is about 3 ns a decision. Mipmaps too small to split (about 6%
  of a 2048x2048 texture's blocks) and one stripe of each split one bound one file to about 6.4x
  of one thread. ASTC (5-12 MB/s a thread) and PVRTC (4-9) are slower by their bytes than the
  rest; what is left is mostly the coding and the parse of ASTC's blocks.
- **Real textures of mobile formats.** BC6H, ETC, EAC, PVRTC and ASTC are measured on samples
  made by encoders, not on mobile games' packages (Android, iOS).
- **PVRTC2 and ASTC of 3D footprints.** Not taken.
- **BC7 indices.** Most of BC7's bits are indices; a model of their levels as an image
  (residuals from a predicted level) rather than the joint buckets of two predictions may do
  better on 4-bit indices.
- **Mipmaps generated.** Where a mipmap is the encoder's output on the box filter of the one
  above, the encoder could be reproduced; whether this corpus's are was not checked.
