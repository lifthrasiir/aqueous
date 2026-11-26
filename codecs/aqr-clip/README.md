# aqr-clip

Lossless Clip Studio Paint (`.clip`) recompression: the tiles of a file's layers are coded by a
model of whole pixels and their zlib streams made again exactly; its SQLite database goes to
aqr-sqlite; the rest is kept as it is. `FORMAT.md` specifies the module `clip-recons`, and how
it works (the tile model, the zlib emulation, the memory plan).

```rust
let f = aqr_clip::formula::encode(&file, threads);      // None where it is not a .clip file
```

```sh
cargo build --release --example aqr-clip   # target/release/examples/aqr-clip
aqr-clip formula [-t N] FILE...                # the formula, its archive at level mid, evaluated back
aqr-clip stat FILE...                          # chunks, block data, tiles by layout and class
aqr-clip bench [-t N] [-b TILES] [-c] FILE...  # tiles by every model; -c checks the dynamic models' round trip
aqr-clip speed [-t N] [-b TILES] [-m dynamic|static] FILE...  # one model as a module would run it
```

## What is taken

- `.clip` files (a `CSFCHUNK` container; no public specification, read off 13 files of
  2019-2025): their tiles of 256x256 pixels, zlib-compressed at level 1 with memLevel 9 (all
  111,515 streams of those files are what zlib 1.3 makes), as colour, one-plane or two-plane
  tiles; a tile equal to an earlier one is a reference to it.
- The SQLite database goes to aqr-sqlite (its PNG preview is not recompressed); other chunks
  (3D models, audio) and tiles whose zlib stream cannot be made again stay as they are.

## Results

Ryzen 7 9700X, 16 threads, 2026-10-28, through `aqr-show -t 16`: each file archived at level
`mid` with its round-trip check, then extracted; the decoder module is in the archives (24 KB
block).

| set | files | MB | ratio | zstd -3 | aqr without aqr-clip | compress | decompress |
|---|---|---|---|---|---|---|---|
| the 13 files of the corpus | 13 | 926 | **0.340** | 0.760 | 0.726 | 35 MB/s | 91 MB/s |
| a held-out file | 1 | 14.1 | 0.454 | | 0.763 | 20 MB/s | 47 MB/s |

Of the 13 files' 775 MB of zlib streams, the tiles come to 0.352 of their zlib bytes (the
dynamic model; 0.384 with the static one, 122 MB/s encoding against 93 on 16 threads); flat
tiles (all zero, one pixel) to 0.000-0.001, tiles of up to 16 colours 0.10, of up to 256
0.45, other 0.41. Whole files with each class by its best model: 926 MB -> 312 MB (0.337);
the database is 25.5 MB of that and the rest 16.2 MB (13.3 MB one file's 3D data).

## Tests

- `tests/formula.rs`: the formula of synthetic `.clip` files evaluated back with the module.
- `tests/edge.rs`: tiles of noise, streams zlib would not make, an embedded database, extreme
  numbers, damaged files and streams (no panic, hang or unbounded allocation).
- `aqr-clip bench -c` checks that every stream of the dynamic models decodes back to its tiles.

## Left

- **Tiles alike but not equal.** 54.7% of the tiles with data equal an earlier one of their file
  (12.1% of the zlib bytes; references take those); an earlier tile (the same block of another
  layer) as context would take the near-equal ones.
- **`static` model's size.** 7.8% larger than `dynamic` for the adaptivity it gives up
  (1.5x the speed); tables coded against the last chunk's, so that chunks can be small, and
  richer contexts.
- **Neighbouring tiles as context.** The block grid is in the database; mipmaps could be
  predicted from the layer they come from.
- **The database's PNG preview** (most of its 25.5 MB) through aqr-png, and `BlockSize` derived
  from the blocks.
