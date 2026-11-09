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
cargo rt -p aqr-zstd                                  # frames parsed, ranges taken (through an archive)
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
  say): not done, as above.
