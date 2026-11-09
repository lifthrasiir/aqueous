# aqr-fat

FAT12, FAT16, FAT32 and exFAT volumes (SD cards, USB drives, EFI system partitions, camera and
firmware images) read into their files: file systems of aqr-disk (`FAT`, `EXFAT`), which takes the
image apart into the contents of the files and the rest
([../aqr-disk/FORMAT.md](../aqr-disk/FORMAT.md)), all of an image or a partition of it. There is
no module: each file's contents become one content of the archive, recompressed by its format and
stored once where the same files are archived along with it. A file in clusters here and there is
one content all the same, its pieces split from it.

```sh
codecs/aqr-fat/scripts/gen-fixtures.sh   # tests/data: volumes by hdiutil, files written mounted (macOS)
```

## What it reads

Written from Microsoft's specifications (fatgen103, the exFAT file system specification); no
other implementation's code was read.

- **FAT12, FAT16, FAT32**: the BPB (a jump, the signature, the sizes the specification allows;
  the width by the count of clusters, FAT32's by its own fields), the first FAT, the directories
  from the root (FAT12's and FAT16's fixed region, FAT32's cluster), their long names (VFAT,
  checked against the short entry's sum) or else the short ones (of the lower case Windows NT's
  flags tell; the OEM code page taken as Latin-1). A file's contents are its chain up to its
  size.
- **exFAT**: the main boot sector, the directory entry sets from the root (a File entry, its
  Stream Extension and its File Name entries), a file's contents its first ValidDataLength bytes
  (contiguous where the set says NoFatChain, else its chain in the first FAT).

Whatever the bytes: a chain ends where it leaves the clusters, at its end or a bad cluster, or as
the clusters followed in all reach twice their count (chains that meet or loop); the bytes of
directories read in all are at most the volume's; each directory is read once, at most 256 deep;
what is past the end of an image cut short is not read.

Left in the rest: the boot regions, the FATs, the directories, exFAT's bitmap and up-case table,
the bytes of a file past its size (to the end of its cluster) or past exFAT's ValidDataLength,
free clusters and deleted files. Not read: the second FAT (the first is taken), FAT32's FSInfo
and backup boot sector (rest), TexFAT, checksums (exFAT's sets and boot region: not checked, so
a damaged one does not stop the reading).

## How it does

Measured 2026-11-09 on an M4 (all threads, the default level) through `cargo aqr`, against the
build before (the volume compressed as it is), `--stats`:

| | before | after |
|---|---|---|
| FAT32 volume of `/usr/share` (`cp -RLX` until full; 734,003,200 bytes, 20,992 files, 4 KiB clusters) | 26,205,279 B, 6.0 s, 74 G instructions, 2.56 GB resident | 23,342,349 B, 7.0 s, 300 G instructions, 1.32 GB resident |

(with aqr-compress's contents looked for in linear time, the commit before). Reading the volume
and making the formula take 53 ms of it (`examples/check.rs`); the rest is the files' own
recompression (aqr-infer-recs' records the most) and sealing them, as of the same files in a tar
archive. The copy is fresh, so no file is in pieces.

```sh
cargo run --release -p aqr-fat --example aqr-fat-check -- [-v] IMAGE...   # files found, timed, rebuilt
```

## Tests

`cargo rt -p aqr-fat`: the fixtures of hdiutil (FAT12, FAT16, FAT32, exFAT: long and Korean names,
subdirectories, an empty file, and two files written into the gaps of deleted ones, in pieces on
FAT; exFAT's allocator did not fragment them), the images rebuilt through aqr-disk, volumes cut
short and changed bytes, chains that meet, data that is no volume. aqr-compress's tests archive
the FAT32 fixture, its JPEG file in pieces recompressed by aqr-jpeg as one.
