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
- Speed: on this Windows machine encoding stops scaling past four threads (49.8 MB/s a thread
  on one, 10.0 on sixteen: `check` on 40 End regions); with a global allocator keeping freed
  blocks of 32 KiB and more for the thread, 35.8 on sixteen. The emulator's buffers (window,
  heads, links, tokens) are made anew for each stream, and Windows' heap gives large blocks
  back to the system as they are freed. This is aqr's allocator (aqr-common's `Mapped` is the
  system's on Windows), not this crate's.

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
- Distant Horizons' `DistantHorizons.sqlite` (zstd level 3 blobs, the coarser LODs derived from
  the finer).
- Bedrock Edition worlds (LevelDB).
