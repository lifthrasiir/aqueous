# aqr-minecraft

Minecraft (Java Edition) region files: the module `mca-recons` (FORMAT.md). A region file
(`.mca`, of `region/`, `entities/` and `poi/`) is a header of chunk locations and timestamps,
then each chunk's zlib stream in sectors of 4 KiB. Minecraft writes every stream at zlib's
level 6 (Java's `Deflater`, of the system's zlib), so the archive stores each chunk's NBT
instead and the module makes the stream again by aqr-flate's emulation of zlib. What does not
follow from the data (the header, stale bytes left in sectors, chunks of other compressions or
streams the emulation does not make) is kept as it is.

```rust
let f = aqr_minecraft::formula::encode(&file, threads); // None where it is not a region file
```

```sh
cargo run --profile release-test -p aqr-minecraft -- check [-t N] [-z LEVEL] FILE_OR_DIR...
```

`check` encodes each `.mca` file, makes it again natively and compares, and tells the streams'
bytes (with `-z`, of zstd at that level over all the files' streams of a kind, in batches of 64
MiB).

## Results

The world of `F:\Data\aqr-corpus\minecraft-world-sample` (18 GB, three dimensions; its Distant
Horizons databases aside, which are aqr-sqlite's), on a Ryzen 7 9700X (16 threads), `aqr` at
the default level with verification, against the `aqr` before (whose scan took nothing of the
files):

| set | files | input | before | `aqr` | compress | decompress (`-K`) |
|---|---|---|---|---|---|---|
| The End (`region`, `entities`, `poi`) | 1,363 | 1.77 GB | 318.93 MB | **180.98 MB** | 28.0 MB/s | 369 MB/s |
| the three dimensions (`region`, `entities`, `poi`) | 5,206 | 8.79 GB | 5.31 GB | **4.54 GB** | 21.1 MB/s | 165 MB/s |

Measured 2026-11-14 (the stats of the whole: `cache/world-sample-7986f6b9+.json`). Compressing
took 417 s against 190 s before, the heap peaking at 1.60 GB. The Overworld's and the Nether's
NBT compresses little better than its zlib by the archive's coding (their files come to about
4.29 GB, stale bytes included, against 4.56 GB of zlib streams in them): the NBT itself is what
is left (Status). The world's 1,435,711 chunks (4.9 GB of zlib streams, 40.5 GB of NBT) were all made again by the
emulation at level 6, memory level 8 (`check`).

- Stale bytes: Minecraft allocates a chunk's sectors before it frees the old ones and never
  clears them, so free sectors hold whole streams of earlier writes, and the padding after a
  stream the tail of a longer one written there before. In four regions of the Nether, the
  Overworld and The End (28 MB), 7.9 MB were padding of such tails and 1.8 MB free sectors (1.1
  MB of them 125 whole streams), against 17.2 MB of current streams: the tails are of old
  versions, and only 7% of their bytes are found in the current streams (32-byte windows), so
  they are kept. `mca/kept` is of no kind, so the archive's scan finds the whole streams in it.
- Speed: on Windows encoding stopped scaling past four threads (49.8 MB/s a thread on one,
  10.0 on sixteen: `check` on 40 End regions), as zlib's emulator makes its buffers (window,
  heads, links, tokens) anew for each stream and Windows' heap gives large blocks back as they
  are freed. aqr's allocator now keeps a few such blocks for each thread (aqr-common's
  `mem::cache`): The End through `aqr` 64-70 -> 27-28 s, `-K` 4.9 -> 3.9 s (the table's
  speeds are from before).

## Memory

`mca-recons` is a part module laid out as a stack (`aqr_api::stack`): the header, the chunks'
locations, the readers' pieces, then for each chunk rebuilt its data, its sectors and zlib's
emulator, freed before the next. Its plan (`module::plan`) follows the largest chunk rebuilt,
not the file (`mca_plans_hold` in aqr-decompress's `tests/wasm.rs`). Natively it is applied
whole (`Run::Threaded`), the chunks' streams made on threads. The encoder holds the chunks' data
twice as it gathers it (some 40 times the file in The End), at most 64 MiB and 64 times the file.

## Status

Left:

- The NBT itself: the palettes' indices unpacked from their packed longs, the light (a quarter
  of the bytes, two fifths in the Nether) predicted from the blocks; an earlier measurement of
  zstd -9 over the NBT gave The End 60% less than its zlib, the others 17-19%.
- Chunks of other compressions: LZ4 (type 4, the server's `region-file-compression=lz4`: lz4-java's
  block framing), gzip (type 1); chunks in files of their own (`c.X.Z.mcc`) are zlib files, which
  aqr-compress takes as such.
- In WebAssembly the module makes the chunks one after another; tasks would spread them.
- Distant Horizons' `DistantHorizons.sqlite` (10.4 GB of the sample's 18 GB): its blobs, frames
  of zstd-jni at level 3, are made again of their contents by aqr-zstd's `zstd-frames`
  (aqr-compress's `sqlite-zstd`; aqr-zstd's README has the numbers). Left, of the contents (some
  9 times the frames in The End, 3 in the others): the table `FullData`'s columns of points
  (`Data`, and the sides `*AdjData`: a varint count for each column, then each point's id with
  two flags, its height, its bottom against the one above, its light) coded by a model of their
  own rather than by the archive's zstd; and the coarser levels (`DetailLevel` 1 to 8, 31% of
  the blobs' bytes, 44% in The End) made of the finer: a parent's column is the 2x2 merge of its
  children's (each point the most common id among the four columns at its height, the light
  averaged of that id), then culled of the points hidden by opaque neighbours, which is exact
  but for which ids are opaque (to be stored); its mapping, generation steps and compression
  modes follow from the children's too.
- Bedrock Edition worlds (LevelDB).
