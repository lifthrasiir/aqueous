# aqr-minecraft

Minecraft (Java Edition) region files: the module `mca-recons` (FORMAT.md). A region file
(`.mca`, of `region/`, `entities/` and `poi/`) is a header of chunk locations and timestamps,
then each chunk's zlib stream in sectors of 4 KiB. Minecraft writes every stream at zlib's
level 6 (Java's `Deflater`, of the system's zlib), so the archive stores each chunk's NBT
instead and the module makes the stream again by aqr-flate's emulation of zlib. Of the NBT, the
sections' block states and light (its *holes*) are coded apart by a model of their own
(`src/model.rs`), the rest of it (its *skeleton*) stored. What does not follow from the data
(the header, stale bytes left in sectors, chunks of other compressions or streams the emulation
does not make) is kept as it is.

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
Horizons databases aside, which are aqr-zstd's), on a Ryzen 7 9700X (16 threads), `aqr` at the
default level with verification: before `mca-recons` (whose scan took nothing of the files), of
version 1 (the NBT whole) and of version 2 (its holes coded):

| set | files | input | before | v1 | v2 | compress v1, v2 | test (`-K`) v1, v2 |
|---|---|---|---|---|---|---|---|
| The End (`region`, `entities`, `poi`) | 1,363 | 1.77 GB | 318.93 MB | 180.98 MB | **102.97 MB** | 32.5 s, 28.9 s | 4.0 s, 7.9 s |
| the three dimensions (`region`, `entities`, `poi`) | 5,206 | 8.79 GB | 5.31 GB | 4.54 GB | **2.42 GB** | 417 s, 311 s | 53 s, 94 s |

(Version 1's whole world, of 2026-11-14, before aqr's allocator kept freed blocks on Windows:
its compressing time is not comparable. v2: the heap peaking at 1.80 GB.)

Version 2's model (block states, light) was measured on 149 of the Overworld's regions
(414 MB): 271.84 -> 156.62 MB (-42%), compressing 11.4 -> 15.1 s, testing 3.0 -> 5.0 s; on 8 of
them (23.3 MB) the chunks' data came to 13.89 MB under zstd -3, the skeletons 2.92 and the holes
3.93 MB. Its time is half the decoding's (zlib's emulation the other half: 1.15 s against the
model's 0.75 s, one thread).

Version 1 was measured 2026-11-14 (the stats of the whole: `cache/world-sample-7986f6b9+.json`;
417 s compressing, the heap peaking at 1.60 GB): the Overworld's and the Nether's NBT compressed
little better than its zlib by the archive's coding (their files came to about 4.29 GB, stale
bytes included, against 4.56 GB of zlib streams in them), which version 2's model is for. The
world's 1,435,711 chunks (4.9 GB of zlib streams, 40.5 GB of NBT) were all made again by the
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

## The model

A chunk's NBT is parsed as it is made again (`src/nbt.rs`); a section's `block_states` `data`, and
its `BlockLight` and `SkyLight` of 2048 bytes, are its holes, each told by a coded flag. They are
coded by one binary arithmetic coder for each chunk, of adaptive bits kept over the file's
chunks (FORMAT.md, §5):

- Block states: each row of 16 as the row below (the section's, or the top of the section of
  the Y below), else each block as one of 16 neighbours already coded (west first, each other
  index once), its bits where none is it; the contexts of the neighbours' and the palette
  entries' hashes, so that they hold over sections of other palettes.
- Light: from the top down, a row as the row above, else each nibble as the first known of the
  light above, west and north (or 0), else by a tree; of the block's hash there.

Measured and not kept: bits counting their uses (4% smaller, 0.2 s slower), two coders taking
halves of the rows (slower, 3.4% larger), ranks of the neighbours under zstd (7.0 MB against
3.67), static ranks in 33 contexts (4.5 MB).

## Memory

`mca-recons` is a part module laid out as a stack (`aqr_api::stack`): the header, the chunks'
locations, the readers' pieces, the model (its tables of 2^18, 2^16 and 2^20 bits, 2.6 MiB) and
its walks' buffers, then for each chunk rebuilt its data, its sectors and zlib's emulator (or its
holes coded), freed before the next. Its plan (`module::plan`) follows the largest chunk rebuilt,
not the file (`mca_plans_hold` in aqr-decompress's `tests/wasm.rs`). Natively it is applied
whole (`Run::Threaded`), the chunks' streams made on threads. The encoder holds the chunks' data
twice as it gathers it (some 40 times the file in The End), at most 64 MiB and 64 times the file.

## Status

Left:

- Speed: version 2 tests at about half version 1's speed; zlib's emulation (its match search,
  `Window::walk`, a third of the decoding) and the model are what take it.
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
