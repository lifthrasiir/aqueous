# aqr-hfs

HFS+ and HFSX volumes (Macs' disks before APFS, Time Machine and backup volumes, disk images) read
into their files: a file system of aqr-disk (`FS`), which takes the image apart into the contents
of the files and the rest ([../aqr-disk/FORMAT.md](../aqr-disk/FORMAT.md)), all of an image or a
partition of it. There is no module: each file's contents become one content of the archive,
recompressed by its format and stored once where the same files are archived along with it; a
file in pieces is one content, its pieces split from it.

```sh
codecs/aqr-hfs/scripts/gen-fixtures.sh                                     # tests/data, on macOS (hdiutil)
cargo run --release -p aqr-hfs --example aqr-hfs-check -- [-v] IMAGE...   # files found, timed, rebuilt
```

## What it reads

Written from Apple's Technical Note TN1150, *HFS Plus Volume Format* (the document alone; no
implementation's code read). hdiutil and macOS's own driver make the fixtures, as black boxes.

- The volume header at byte 1024 (`H+` version 4, `HX` version 5; a block size of a power of two
  from 512).
- The extents overflow file and the catalog file, from the header's fork data (the catalog's own
  further extents from the overflow file). The catalog's leaf nodes, by the forward links from the
  first leaf, each once and at most as many as the file holds; the folder and file records
  (UTF-16 names, the BSD info's mode, flags and special field, the Finder info's type and creator,
  the data fork).
- The directories from the root (CNID 2), each once, at most 256 deep. A regular file's contents:
  the extents of its data fork in order (the 8 of the record, then those of the overflow file's
  records of its id and first block) up to its logical size.
- A file hard link (`hlnk`/`hfs+`, its link number in the BSD info's special field) is the indirect
  node file `iNode<N>` of the private directory (under the root, a name starting with four NULs):
  one file, under the first link's name found, with the inode's data fork. The private directory
  is not walked.

Left in the rest: files compressed by decmpfs (`UF_COMPRESSED`: their data is in an attribute or
the resource fork), symbolic links, directories, resource forks, the journal (not replayed), the
attributes and allocation files, the startup file, free blocks, directory hard links' inodes
(`fdrp`/`MACS`, their targets are reached by their own paths), HFS-wrapped volumes (`BD`). Names
are as stored (decomposed Unicode, UTF-16 made into UTF-8).

Whatever the bytes: each B-tree node is read once, the nodes of a tree at most as many as its
file holds; extents are those of the catalog and the overflow records read, each within the
volume; the size of the names is bounded by the image's.

## How it does

On an Apple M4, with `aqr` (2026-11-09): an HFS+ volume in a GPT of 1.33 GB made by hdiutil of a
Rust toolchain's `lib` (2644 files, 1.09 GB of them: rlibs, dylibs, sources):

| | archived | time | heap peak |
|---|---|---|---|
| `zstd -3` | 307.45 MB | | |
| `aqr`, the image scanned (no HFS+) | 245.53 MB | 14.1 s | 0.71 GB |
| `aqr`, its files taken apart | 231.71 MB | 15.8 s | 1.26 GB |

The files are found and the image laid out in 0.07 s (`aqr-hfs-check`). The same disk as a UDZO
image (371.63 MB) archives to 230.94 MB (aqr-compress's `dmg`, its zlib chunks rebuilt).

## Tests

`cargo rt -p aqr-hfs`: the fixtures (an HFS+ volume in an APM and an HFSX volume in a GPT:
nested directories, names in Hangul and long ones, 400 files of one directory, a hard link, a
symbolic link, a file with a resource fork, a decmpfs file, files in the gaps of a nearly full
volume, one of 251 extents with the overflow file used), rebuilt through aqr-disk; images cut short
and changed bytes in the volume header, the catalog and the overflow file; data that is no volume.
