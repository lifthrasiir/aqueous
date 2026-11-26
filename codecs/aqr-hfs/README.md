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

## What is taken

Written from Apple's Technical Note TN1150, *HFS Plus Volume Format* (the document alone; no
implementation's code read). hdiutil and macOS's own driver make the fixtures, as black boxes.

- The volume header at byte 1024 (`H+` version 4, `HX` version 5), the extents overflow file and
  the catalog file (its leaf nodes by the forward links, each once): the folder and file records
  (UTF-16 names, BSD info, Finder info, the data fork).
- The directories from the root (CNID 2), each once, at most 256 deep. A regular file's contents:
  the extents of its data fork in order (the 8 of the record, then those of the overflow file's)
  up to its logical size.
- A file hard link (`hlnk`/`hfs+`) is the indirect node file `iNode<N>` of the private directory:
  one file, under the first link's name found, with the inode's data fork.

Whatever the bytes: each B-tree node is read once, the nodes of a tree at most as many as its file
holds; extents are each within the volume; the size of the names is bounded by the image's.

Stays in the rest: files compressed by decmpfs (`UF_COMPRESSED`), symbolic links, directories,
resource forks, the journal (not replayed), the attributes and allocation files, the startup file,
free blocks, directory hard links' inodes (`fdrp`/`MACS`; their targets are reached by their own
paths). Not read: HFS-wrapped volumes (`BD`). Names are as stored (decomposed Unicode, UTF-16 made
into UTF-8).

## Results

On an Apple M4 with `aqr` (2026-11-09): an HFS+ volume in a GPT of 1.33 GB made by hdiutil of a
Rust toolchain's `lib` (2644 files, 1.09 GB of them: rlibs, dylibs, sources):

| | archived | time | heap peak |
|---|---|---|---|
| `zstd -3` | 307.45 MB | | |
| `aqr`, the image scanned (no HFS+) | 245.53 MB | 14.1 s | 0.71 GB |
| `aqr`, its files taken apart | 231.71 MB | 15.8 s | 1.26 GB |

The files are found and the image laid out in 0.07 s. The same disk as a UDZO image (371.63 MB)
archives to 230.94 MB (aqr-compress's `dmg`, its zlib chunks rebuilt).

## Tests

`cargo rt -p aqr-hfs`, rebuilt through aqr-disk:

- An HFS+ volume in an APM and an HFSX volume in a GPT (nested directories, names in Hangul and
  long ones, 400 files of one directory, a hard link, a symbolic link, a file with a resource fork,
  a decmpfs file, files in the gaps of a nearly full volume, one of 251 extents with the overflow
  file used).
- Images cut short and changed bytes in the volume header, the catalog and the overflow file; data
  that is no volume.

## Left

- **decmpfs files.** Files compressed by decmpfs are not taken (their data is in an attribute or
  the resource fork), so they stay in the rest.
- **Resource forks.** Only the data fork is a file.
- **HFS-wrapped volumes** (`BD`) and a journal needing replay are not read.
