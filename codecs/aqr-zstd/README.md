# aqr-zstd

Zstandard frames kept as they are, and what they hold given to the files having the same: the
files of a batch that are the content of a `.zst` file (the whole, or a member of the tar archive
it is) are taken from its frames, which the archive's Patch instructions decompress, rather than
stored again. A frame is not rebuilt from its content, except in other data (a SQLite database's
blobs) where libzstd's level 3 is emulated. `FORMAT.md` specifies the formulas, the module
`zstd-frames`, and how they work.

```rust
use aqr_zstd::{decode, formula, parse};
let fr = parse(&file).unwrap(); // frames and skippable frames from the start, by their structure
let (content, lens) = decode(&file, &fr, u64::MAX).unwrap(); // checksums checked
// the nodes giving `wanted` (ranges of the content, apart, in order), of the frames' blocks in
// stream `s` of the formula `f`
let blocks: Vec<_> = fr.frames().map(|f| f.blocks.start as u64..f.blocks.end as u64).zip(lens).collect();
let nodes = formula::take(&mut f, s, &blocks, &wanted);

// frames in other data: each its pieces
let f = aqr_zstd::formula::frames_in(&file, &frames, 3, Kind::Table, threads);
```

`examples/dbframes.rs` takes a database's frames through `zstd-frames`; `examples/zcheck.rs` checks
`compress` on a database's blobs.

## What is taken

- `.zst` files of frames and skippable frames (several frames, raw and RLE blocks, checksums) whose
  content, or a member of the tar, ar, cpio, asar or ISO image it is, is a file of the same batch
  (`crates/aqr-compress/src/zst.rs`: `together`): from level 2, frames of at most an application's
  bound of content (256 MiB by default).
- SQLite databases whose blobs are mostly frames of libzstd 1.5.x at level 3, no dictionary, no
  checksum, contents under 256 KiB (`sqlite-zstd`, the blobs found by aqr-sqlite's `blobs`; Distant
  Horizons'): the frames are made again of their contents.
- Not taken: frames of other levels or strategies in other data, frames with a dictionary.

## Results

Files beside a `.tar.zst` (2026-11-09, Apple M4, `aqr -c` at the default level, 10 threads):
libarchive's source tree (1581 files, 20.98 MB) beside its tar archive as `zstd -3` (5.89 MB).

| | archive | instructions | resident peak |
|---|---|---|---|
| the tree and the `.tar.zst`, before | 11.44 MB | 18.16 G | 182 MB |
| the same, the files taken from the frame | 5.95 MB | 4.33 G | 83 MB |
| the tree alone | 5.56 MB | 18.14 G | 150 MB |

Testing it (`aqr -K`): 0.94 G instructions, 61 MB resident.

Distant Horizons' databases (`F:\Dataqr-corpus\minecraft-world-sample`), `aqr` at the default level,
Ryzen 7 9700X, 16 threads, verified:

| database | input | before | `aqr` | compress | test (`-K`) |
|---|---|---|---|---|---|
| The End | 594.31 MB | 509.13 MB (85.7%) | **420.61 MB** (70.8%) | 30.0 s | 5.3 s, 113 MB/s, heap 329 MB |
| the Nether | 3.36 GB | | **2.99 GB** (89.2%) | 101 s | 20.3 s, 165 MB/s, heap 521 MB |
| the Overworld | 6.46 GB | | **5.98 GB** (92.5%) | 156 s | 32.7 s, 198 MB/s, heap 749 MB |

`compress` makes every frame of The End (805,720) and samples of the others again, at 457 MB/s
(content ratio 9.5) and 178 MB/s (2.8) on one thread. The Nether gains little: its contents
compress hardly better than the frames did (the gain is the structure of the contents, aqr-minecraft's).

## Tests

```sh
cargo rt -p aqr-zstd                                  # frames parsed, ranges taken (through an archive), made again by compress
cargo rt -p aqr-compress --test archive zstd_held_once # files beside a .tar.zst and a .zst of two frames
```

## Left

- **Other containers.** ZIP's method 93, a `.deb`'s `data.tar.zst`, RPM payloads, squashfs and btrfs;
  frames in other data than databases. Only whole files of frames are taken from.
- **Pieces of other files.** A member of another tar archive or a ZIP's stored entry is not taken
  from: only the files of a batch, before they are recompressed.
- **Frames over the bound.** The decoder makes a patch's output whole, so frames of more than an
  application's bound (256 MiB) of content are not used; streaming it would lift that.
- **Dictionaries and size.** Frames with a dictionary (a Patch instruction has none) and of more than
  2^31 bytes.
- **Rebuilding a frame** where its content would archive much smaller (`zstd -1` of text, say): only
  `compress` for others' formats, at level 3. Fast-strategy levels (1, 2) and the block splitting of
  contents of 256 KiB or more are not emulated.
