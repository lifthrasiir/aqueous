# aqr-minecraft

Minecraft (Java Edition) region files (`.mca`, of `region/`, `entities/` and `poi/`) and Distant
Horizons' databases: the module `mca-recons` stores each chunk's NBT instead of its zlib stream
(Minecraft writes every stream at zlib's level 6) and makes the stream again by aqr-flate's
emulation, the sections' block states and light coded by a model of their own; `dh-lods` makes the
coarser levels of a Distant Horizons database of their children. `FORMAT.md` specifies both
modules, and how they work.

```rust
let f = aqr_minecraft::formula::encode(&file, threads); // None where it is not a region file
```

```sh
cargo run --profile release-test -p aqr-minecraft -- check [-t N] [-z LEVEL] FILE_OR_DIR...
```

`check` encodes each `.mca` file, makes it again natively and compares, and tells the streams'
bytes (with `-z`, of zstd at that level over all the files' streams of a kind, in batches of 64
MiB). `examples/dhcheck.rs` makes every parent of four children of a database again, `examples/dhlods.rs` encodes a database and makes it again.

## What is taken

- **Region files**: the chunks of zlib (rebuilt from their NBT) and of no compression; the header
  and the stale bytes Minecraft leaves in freed sectors are kept as they are, as are chunks of
  other compressions and streams the emulation does not make. Not taken: chunks of LZ4 (type 4,
  `region-file-compression=lz4`) and gzip, which are kept; chunks in files of their own
  (`c.X.Z.mcc`) are zlib files, which aqr-compress takes as such.
- **Distant Horizons**' `DistantHorizons.sqlite`: its blobs are frames of zstd-jni at level 3,
  made again by aqr-zstd's `zstd-frames` (aqr-compress's `sqlite-zstd`); `dh-lods` makes the
  sections of the coarser levels (`DetailLevel` 1 to 8) of their children that come before them in
  the file, the rest stored.

## Results

The world of `F:\Data\aqr-corpus\minecraft-world-sample` (18 GB, three dimensions), on a Ryzen 7
9700X (16 threads), `aqr` at the default level with verification (heap peak 1.80 GB), against the
files without `mca-recons`:

| set | files | input | before | after | compress | test (`-K`) |
|---|---|---|---|---|---|---|
| The End (`region`, `entities`, `poi`) | 1,363 | 1.77 GB | 318.93 MB | **102.97 MB** | 28.9 s | 7.9 s |
| the three dimensions | 5,206 | 8.79 GB | 5.31 GB | **2.42 GB** | 311 s | 94 s |

All 1,435,711 chunks of the world (4.9 GB of zlib streams, 40.5 GB of NBT) are made again by the
emulation at level 6, memory level 8. On 149 of the Overworld's regions (414 MB) the model of
block states and light is 42% smaller than storing the NBT whole (271.84 -> 156.62 MB).

Distant Horizons' databases, through `aqr` (default level, verified), against `zstd-frames` alone:

| database | zstd-frames | and dh-lods | compress | test (`-K`) |
|---|---|---|---|---|
| The End (594 MB) | 420.61 MB | **361.15 MB** | 30 -> 66 s | 5.3 -> 9.8 s |
| the Nether (3.36 GB) | 2.99 GB | **2.84 GB** | 101 -> 184 s | 20.3 -> 35.3 s |
| the Overworld (6.46 GB) | 5.98 GB | **5.92 GB** | 156 -> 277 s | 32.7 -> 40.1 s |

96.2% of The End's parents of four children (24,192 of 25,149) and 98.7% of the Nether's are made
again exactly; the others are stale.

## Tests

`cargo rt -p aqr-minecraft`:

- `tests/formula.rs`: region files made up of chunks of every kind, encoded and made again.
- `tests/edge.rs`: NBT that is a candidate for holes but none, malformed NBT, chunks inflating past
  the limit, gzip and custom chunks.
- `tests/lods.rs`: Distant Horizons' databases through `dh-lods` and `zstd-frames` (the parents
  made of their children that come before them), the file made again natively, whole and streamed,
  on one thread and on several.

`check`, `dhcheck` and `dhlods FILE [THREADS]` (the database encoded, its streams told, made again
natively and compared) run on the world sample.

## Left

- **Speed.** Testing (decompressing) takes about twice the time of storing the NBT whole; zlib's
  emulation (its match search, `Window::walk`, a third of the decoding) and the model are what
  take it.
- **WebAssembly tasks.** In WebAssembly the module makes the chunks one after another; tasks would
  spread them.
- **Other compressions.** Chunks of LZ4 (lz4-java's block framing) and gzip are kept as they are.
- **Distant Horizons.** Parents made of children that come after them in the file (a third of the
  quadrants in The End, two fifths in the Nether, more in the Overworld: the order of the file is
  DH's writes') cannot be taken by a streaming decoder; a model of the columns' contents (counts,
  ids, heights, light) rather than the archive's zstd; and speed (compressing takes 2.2 to 1.8
  times as long as zstd-frames alone).
- **Bedrock Edition worlds** (LevelDB).
