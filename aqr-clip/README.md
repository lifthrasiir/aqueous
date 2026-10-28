# aqr-clip

Lossless Clip Studio Paint (`.clip`) recompression: the module `clip-recons` (FORMAT.md). The
tiles of a file's layers are coded by a model of whole pixels and their zlib streams made again
exactly; its SQLite database goes to aqr-sqlite; the rest is kept as it is.

```rust
let f = aqr_clip::formula::encode(&file, threads);      // None where it is not a .clip file
```

```sh
cargo build --release
aqr-clip formula [-t N] FILE...                # the formula, its archive at level mid, evaluated back
aqr-clip stat FILE...                          # chunks, block data, tiles by layout and class
aqr-clip bench [-t N] [-b TILES] [-c] FILE...  # tiles by every model; -c checks the dynamic models' round trip
aqr-clip speed [-t N] [-b TILES] [-m dynamic|static] FILE...  # one model as a module would run it
```

## Results

Measured on 2026-10-28 on a Ryzen 7 9700X through `aqr-show -t 16` (16 threads), each file
archived at level `mid` with its round-trip check, then extracted; the decoder modules are in
the archives (25 KB for `clip-recons`).

| set | files | MB | ratio | zstd -3 | aqr before | compress | decompress |
|---|---|---|---|---|---|---|---|
| the 13 files below | 13 | 926 | **0.344** | 0.760 | 0.726 | 32 MB/s | 81 MB/s |
| a held-out file | 1 | 14.1 | 0.499 | | 0.763 | 20 MB/s | 47 MB/s |

The held-out file was not looked at before it was measured. Its tiles of few colours repeat
across the file: through the archive their inflated bytes come to 0.041 (16 colours or fewer)
and 0.291 (256 or fewer) of their zlib bytes, the model of whole pixels, which codes each tile
on its own, 0.414 and 0.607 (`aqr-clip bench`; with the best model of each class, 0.437).

## Format

A `CSFCHUNK` container (see `src/clip.rs`): `Head`, `Exta` chunks (external data: a layer's
tiles as block data, or data of other kinds such as 3D models and audio), one `SQLi` chunk (an
SQLite database of everything else) and `Foot`. No public specification; what is here was read
off 13 files of 2019–2025.

- A block is a 256×256 tile, zlib-compressed, or none where the tile is empty. Every one of the
  111,515 zlib streams in those files is what zlib 1.3 makes at level 1, window 15,
  **memLevel 9** (checked with Python's zlib 1.3.2), so the streams can be made again exactly.
- Tiles inflate to 327,680 bytes (the alpha plane, then B, G, R and a fourth byte, mostly 0 or
  255, interleaved), 65,536 (one plane) or 131,072 (two planes).
- `BlockStatus` and `BlockCheckSum` follow the blocks, a u32 each per block; the checksum is
  0 for empty blocks and none of CRC-32 or Adler-32 of the stream, the data or the record.
- The database's `Offscreen.Attribute` gives each layer's canvas and block grid (`Parameter`)
  and the sizes of its block records again (`BlockSize`); `Offscreen.BlockData` names the
  `Exta` chunk.

## Results

Measured on 2026-10-28 on a Windows machine, 16 threads, with `aqr-clip bench -t 16 -c` on 13
files (926 MB, 775 MB of it zlib streams; 32 GB once inflated). Ratios are to the zlib bytes of
each group of tiles. Every stream of the dynamic models decodes back to its tiles (`-c`).

| tiles | count | zlib MB | as now (zl/gen) | px/gen | aqr-raw | aqr-psd | dyn-l | dyn-m | dyn |
|---|---|---|---|---|---|---|---|---|---|
| colour, all zero | 20,011 | 29.1 | 0.000 | 0.007 | 0.002 | 0.132 | 0.004 | 0.004 | **0.000** |
| colour, one pixel | 24,242 | 39.0 | 0.001 | 0.021 | 0.003 | 0.121 | 0.004 | 0.004 | **0.000** |
| colour, ≤16 pixels | 20,884 | 86.7 | 0.613 | 0.277 | 0.234 | 0.300 | 0.104 | **0.102** | 0.115 |
| colour, ≤256 | 22,008 | 289.0 | 0.913 | 0.703 | 0.649 | 0.610 | 0.486 | **0.450** | 0.457 |
| colour, more | 8,386 | 301.3 | 0.987 | 0.766 | 0.550 | 0.501 | 0.493 | **0.405** | 0.408 |
| one plane, ≤16 | 2,712 | 2.7 | 0.739 | 0.440 | 0.924 | 1.073 | 0.278 | **0.272** | 0.280 |
| one plane, ≤256 | 3,971 | 19.0 | 0.965 | 0.723 | 0.715 | 0.629 | 0.448 | **0.379** | 0.404 |
| two planes, ≤256 | 219 | 5.2 | 1.000 | 0.834 | **0.490** | **0.489** | 0.851 | 0.603 | 0.604 |
| all tiles | | 774.9 | 0.826 | 0.617 | 0.506 | 0.490 | 0.402 | **0.352** | 0.357 |

- *as now*: the zlib streams through the archive (level mid), which is what aqr does with a
  `.clip` file today; *px/gen*: the inflated tiles through the archive; *dyn-l*, *dyn-m*, *dyn*:
  the dynamic model (below) with `Escape::Left`, with `Escape::Med`, and with `Escape::Med`,
  runs and tile kinds.
- Whole files, each class by its best model, the database by aqr-sqlite and the rest through
  the archive: **926 MB → 312 MB (0.337)**, against 672 MB (0.726) through `aqr-show` as it
  is. The database is 25.5 MB of that (mostly its PNG preview, not recompressed), the rest
  16.2 MB (13.3 MB of it one file's 3D data).

### Speed

`aqr-clip speed` runs a model alone as a module would: batches of 64 tiles of a layout in file
order, each a stream of its own (the model anew; this costs nothing in size), coded and decoded
on 16 threads (Ryzen 7 9700X, 8 cores) or one; the encoder inflates the tiles, the decoder
deflates them again at level 1 (zlib-rs, memLevel 8: the speed, not the file's streams). 13
files, 926 MB.

| model | tiles / zlib | encode, 16 threads | decode, 16 threads (the check included) | model alone, one thread (encode / decode) |
|---|---|---|---|---|
| `dynamic` (adaptive bits) | **0.357** | 9.9 s (93 MB/s) | 12.8 s | 14.8 / 17.8 MB/s |
| `static` (static tables) | 0.384 | **7.6 s (122 MB/s)** | **8.3 s** | 21.4 / 30.3 MB/s |

On one thread inflating takes 11.8 s and deflating 22 s on top of the model (zlib-rs). The
streams themselves are made again by `aqr_clip::zlib` (aqr-flate's emulation of zlib 1.3's
compressor at level 1, memLevel 9): all 111,515 of them byte for byte, in 74.5 thread seconds
on 16 threads (zlib-rs 47 s), the decode then 13.1 s wall with the check. With 16
threads a thread runs at about half its speed alone (SMT).

`static` (`src/static.rs`) is the same model in symbols of static tables per chunk of 2^20
symbols (rANS, as aqr-psd), the list a ring with a hash index instead of a list moved to front.
It is 7.8% larger: not its tables (5.2 MB of 298 MB) but the adaptivity it gives up; smaller
chunks code the symbols smaller (2^15: −7%) and the tables larger (+6 MB on one file).

### Speed of the models in `bench`

Inflated bytes a second, a thread (the encoder; every model of a batch runs on the same
thread, 16 at once, so these are lower than alone):

| tiles | aqr-psd | dyn-m | dyn |
|---|---|---|---|
| colour, zero or one pixel | ~950 | ~135 | 1300–1500 |
| colour, ≤16 | 711 | 139 | 551 |
| colour, ≤256 | 392 | 56 | 83 |
| colour, more | 199 | 39 | 47 |
| one plane | 300–1300 | 25–30 | 56–450 |

## The dynamic model (`src/dynamic.rs`)

A pixel is coded whole (all its samples at once): whether it copies its left, upper, upper
right or upper left neighbour; else its place in a list of recent pixels; else its samples.
Adaptive binary contexts, carry-less arithmetic coding (`aqr_common::arith`).

- `Escape::Left`: samples as differences to the left pixel's.
- `Escape::Med`: as aqr-psd's model, the residual against MED (B and R corrected by G's
  residual), contexts by sample, transparency, activity and G's residual; and the encoder
  codes a listed pixel as a new one where that costs less (the decoder just follows the bit).
- Runs and tile kinds: all-zero and one-pixel tiles by a flag; where the row above equals the
  left pixel for 4 pixels on, the pixels equal to it are coded at once (as long as that
  stretch above, else a length). 1.4% larger than without, several times as fast on flat
  tiles.

It beats the per-sample models on everything but the few two-plane tiles: the art is mostly
flat fills, line art and the colours under transparent pixels, which copies and the list
catch whole.

## Next steps

- `static`: the size back without its adaptivity (tables coded against the last chunk's, so
  that chunks can be small; richer contexts).
- Neighbouring tiles as context (the block grid is in the database), mipmaps predicted from
  the layer they come from.
- The PNG preview of the database through aqr-png, `BlockSize` derived from the blocks.
- A formula: the tiles, zlib at level 1 / memLevel 9 made again, the rest as it is.
