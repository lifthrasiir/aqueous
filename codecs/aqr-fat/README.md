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
cargo run --release -p aqr-fat --example aqr-fat-check -- [-v] IMAGE...   # files found, timed, rebuilt
```

## What is taken

Written from Microsoft's specifications (fatgen103, the exFAT file system specification); no
other implementation's code was read.

- **FAT12, FAT16, FAT32**: the BPB, the first FAT, the directories from the root, long names (VFAT,
  checked against the short entry's sum) or else short ones (the OEM code page taken as Latin-1).
  A file's contents are its chain up to its size.
- **exFAT**: the main boot sector, the directory entry sets from the root; a file's contents its
  first ValidDataLength bytes (contiguous where NoFatChain, else its chain in the first FAT).

Whatever the bytes: a chain ends where it leaves the clusters, at a bad cluster, or when the
clusters followed in all reach twice their count (chains that meet or loop); the bytes of
directories read in all are at most the volume's; each directory is read once, at most 256 deep;
what is past the end of an image cut short is not read.

Stays in the rest: the boot regions, the FATs, the directories, exFAT's bitmap and up-case table,
a file's bytes past its size or exFAT's ValidDataLength, free clusters and deleted files. Not
read: the second FAT, FAT32's FSInfo and backup boot sector, TexFAT, checksums (exFAT's sets and
boot region are not checked, so a damaged one does not stop the reading).

## Results

On an M4 through `cargo aqr` (all threads, the default level, 2026-11-09), a FAT32 volume of
`/usr/share` (734,003,200 bytes, 20,992 files, 4 KiB clusters; a fresh copy, so no file in
pieces) against the volume compressed as it is:

| | before | after |
|---|---|---|
| archived, time, resident | 26,205,279 B, 6.0 s, 2.56 GB | 23,342,349 B, 7.0 s, 1.32 GB |

Reading the volume and making the formula take 53 ms; the rest is the files' own recompression
(aqr-infer-recs' records the most), as of the same files in a tar archive.

## Tests

`cargo rt -p aqr-fat`:

- The fixtures of hdiutil (FAT12, FAT16, FAT32, exFAT: long and Korean names, subdirectories, an
  empty file, two files in the gaps of deleted ones, in pieces on FAT), rebuilt through aqr-disk.
- Volumes cut short and changed bytes, chains that meet, data that is no volume.

aqr-compress's tests archive the FAT32 fixture, its JPEG file in pieces recompressed by aqr-jpeg
as one.

## Left

- **Metadata.** The boot regions, FATs and directories are stored as they are (zstd), not derived
  from the files.
- **Fragmented exFAT.** exFAT's allocator did not fragment the fixtures' files, so files in pieces
  are tested on FAT only.
