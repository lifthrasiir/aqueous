# aqr-zstd

Zstandard frames kept as they are, and what they hold given to the files having the same: the
files of a batch that are the content of a `.zst` file (the whole, or a member of the tar archive
it is) are taken from its frames, which the archive's Patch instructions decompress (of an empty
source), rather than stored again. The formula is in [FORMAT.md](FORMAT.md).

No frame is rebuilt from its content. Zstandard has many levels and strategies, and a record of
its parse (as aqr-lzo and aqr-lzma keep) would cost a good part of the frame: its entropy coding
is already about as good as the archive's would be, so a frame costs about what its content
would, and keeping it is the cheapest record there is. What sharing needs is only the content,
which a Patch instruction gives.

```rust
use aqr_zstd::{decode, formula, parse};
let fr = parse(&file).unwrap(); // frames and skippable frames from the start, by their structure
let (content, lens) = decode(&file, &fr, u64::MAX).unwrap(); // checksums checked
// the nodes giving `wanted` (ranges of the content, apart, in order), of the frames' blocks in
// stream `s` of the formula `f`
let blocks: Vec<_> = fr.frames().map(|f| f.blocks.start as u64..f.blocks.end as u64).zip(lens).collect();
let nodes = formula::take(&mut f, s, &blocks, &wanted);
```

aqr-compress takes it in an archive of several files (`crates/aqr-compress/src/zst.rs`): a `.zst`
file is put in a batch with the files its content holds (the members of a tar archive, or the
whole: `together`), and those files are taken from its frames.

## The compressor emulated

`compress::compress(content, 3)` makes the frame libzstd (1.5.x) writes of `content` at level 3
(`ZSTD_compress`, or a stream ended at once with its size pledged: no dictionary, no checksum),
byte for byte: the parameters of the level for the size, the double-fast match finder, the
literals' Huffman coding and the sequences' FSE coding with libzstd's choices of tables and
block types. It is written from what is known of libzstd's behaviour and checked against it as
a black box (Python's `compression.zstd`, the `zstd` crate's); libzstd's code is not read. It
needs no `encode`, so modules can make frames. Not emulated: other strategies' levels, and
contents of 256 KiB or more, whose blocks libzstd may split (`None`).

Checked on Distant Horizons' databases (`F:\Data\aqr-corpus\minecraft-world-sample`, blobs of
zstd-jni at level 3): every one of The End's 805,720 frames and samples of 33,392 (the Nether)
and 26,376 (the Overworld) made again, at 457 MB/s (The End, content ratio 9.5) and 178 MB/s
(the others, 2.8) on one thread of a Ryzen 7 9700X (`examples/zcheck.rs`).

## Frames in other data: `zstd-frames`

The module `zstd-frames` (FORMAT.md) makes a file of frames in pieces again: the bytes kept, and
each frame made of its content by `compress` where its first piece is, held in a pool until its
last piece is put. aqr-compress takes it for SQLite databases whose blobs are frames
(`sqlite-zstd`, the blobs found by aqr-sqlite's `blobs`), as Distant Horizons' are.

```rust
let f = aqr_zstd::formula::frames_in(&file, &frames, 3, Kind::Table, threads); // frames: each its pieces
```

Natively the module makes the frames on threads, 2048 at a time ahead of where they are put; in
WebAssembly one at a time. `examples/dbframes.rs` takes a database's frames through it.

Distant Horizons' databases of `F:\Data\aqr-corpus\minecraft-world-sample`, `aqr` at the
default level (verified), a Ryzen 7 9700X, 16 threads:

| database | input | before | `aqr` | compress | test (`-K`) |
|---|---|---|---|---|---|
| The End | 594.31 MB | 509.13 MB (85.7%) | **420.61 MB** (70.8%) | 30.0 s | 5.3 s, 113 MB/s, heap 329 MB |
| the Nether | 3.36 GB | | **2.99 GB** (89.2%) | 101 s | 20.3 s, 165 MB/s, heap 521 MB |
| the Overworld | 6.46 GB | | **5.98 GB** (92.5%) | 156 s | 32.7 s, 198 MB/s, heap 749 MB |

- The End's 805,720 frames (491 MB) hold 5.05 GB; the layout is 3.0 MB and the bytes kept
  103 MB (the pages, the other columns, the tables of chunks' hashes).
- Their pieces: overflow chains go back and forth over the file, so that 38% of The End's frame
  bytes and 68% of the Nether's are of frames whose pieces are out of the file's order, and at
  most 1,882 frames (19 MB of pool) are open at once in The End, 2,541 (93 MB) in the Nether.
- The Nether gains little: its blobs' contents (2.8 times them) compress hardly better than the
  frames did; what gains is the contents' structure (Distant Horizons' columns of points) and
  the coarser LODs that the finer ones make (aqr-minecraft's Status).
- Compressing, the heap peaks at 1.34, 3.95 and 6.78 GB, most of it the archive's zstd of the
  contents (9.4 GB of them in the Nether, 18 GB in the Overworld), past the budget: what the
  archive holds of a long stream as it codes it, not the module's (the stats' "zstd").

## Results

Measured on 2026-11-09 on an Apple M4 (load about 3), `aqr -c` at the default level, 10 threads:
libarchive's source tree (1581 files, 20.98 MB) beside its tar archive as `zstd -3`
(`libarchive.tar.zst`, 5.89 MB).

| | archive | instructions | resident peak |
|---|---|---|---|
| the tree and the `.tar.zst`, before | 11.44 MB | 18.16 G | 182 MB |
| the same, the files taken from the frame | 5.95 MB | 4.33 G | 83 MB |
| the tree alone | 5.56 MB | 18.14 G | 150 MB |

- Every file of the tree is taken from the frame; the archive is the frame, 36 KB of formula
  (one formula block) and 21.5 KB of metadata. The frame (`zstd -3`) is 6% larger than the archive would make the tree.
- Testing the archive (`aqr -K`): 0.94 G instructions and 61 MB resident (0.97 G and 35 MB
  before, the tree's own blocks decoded): the frame is decompressed once, whole.
- A formula block of many files taking pieces of one Split cost Swaps growing as their square
  (a program of hundreds of MB decoded, 364 MB resident testing): each file made is now put in
  its place below the pieces not taken yet (`Emit::sink` in aqr-formula), a Swap each.

## Tests

```sh
cargo rt -p aqr-zstd                                  # frames parsed, ranges taken (through an archive), made again by compress
cargo rt -p aqr-compress --test archive zstd_held_once # files beside a .tar.zst and a .zst of two frames
```

## Status

Done: `.zst` files of frames and skippable frames (several frames, raw and RLE blocks, checksums)
whose content, or a member of the tar, ar, cpio, asar or ISO image it is, is a file of the same
batch: from level 2 (`aqr -l 2`), frames of at most an application's bound of content (256 MiB by
default), as a decoder makes a patch's output whole.

Not yet:

- Frames in other containers: ZIP's method 93, a `.deb`'s `data.tar.zst` (an ar member), RPM
  payloads, squashfs and btrfs; and frames found in other data. Only whole files of frames are
  taken from.
- Pieces of other files than whole files (a member of another tar archive, a ZIP's stored
  entry): only the files of a batch, before they are recompressed, are taken.
- The decoder streaming a patch's output (it makes it whole): frames over the bound are not used.
- Frames with a dictionary (a Patch instruction has none), and of more than 2^31 bytes.
- Rebuilding a frame where its content would archive much smaller than it (`zstd -1` of text,
  say): only by `compress` for others' formats (level 3), not for `.zst` files. Levels of the
  fast strategy (1, 2), and the block splitting of contents of 256 KiB or more, are not
  emulated.
