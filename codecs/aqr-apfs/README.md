# aqr-apfs

APFS containers (Macs' and iOS devices' disks, disk images, Time Machine and backup volumes) read
into their files: a file system of aqr-disk (`FS`), which takes the image apart into the contents
of the files and the rest ([../aqr-disk/FORMAT.md](../aqr-disk/FORMAT.md)), all of an image or a
partition of it. There is no module: each file's contents become one content of the archive,
recompressed by its format and stored once where the same files are archived along with it; a
file in pieces is one content, its pieces split from it.

```sh
codecs/aqr-apfs/scripts/gen-fixtures.sh                                     # tests/data, on macOS (hdiutil)
cargo run --release -p aqr-apfs --example aqr-apfs-check -- [-v] IMAGE...   # files found, timed, rebuilt
```

## What it reads

Written from Apple's *Apple File System Reference* (the document alone; no implementation's code
read). hdiutil and macOS's own driver make the fixtures, as black boxes.

- The container superblock: the newest valid one (magic, Fletcher-64 checksum) of the checkpoint
  descriptor area, or block 0's (a descriptor area that is not contiguous is not read).
- The container's object map (a B-tree of physical blocks), the newest version of each oid up to
  the superblock's xid; each volume of `nx_fs_oid`, its superblock, its object map and its
  file-system tree (variable-size keys, nodes visited once each, at most 16 deep).
- In the tree: the directory records (hashed or not), the inodes (mode, `private_id`, the size of
  the data stream's extended field) and the file extents. The directories from the root, each
  once, at most 256 deep; the files of all volumes, names unprefixed.
- A regular file's contents: the extents of its data stream in order, up to its size, holes
  (physical block 0) left out. Each inode once (a hard link's other names are skipped); a clone
  shares its original's stream, so it has the same extents (aqr-disk keeps the first).

Left in the rest: files compressed by decmpfs (`UF_COMPRESSED`: their data is in an extended
attribute or the resource fork), symbolic links, directories, extended attributes, snapshots (only
the current tree is read), encrypted and sealed volumes, the object maps, the trees, the space
manager and the reaper, free blocks. Not read: the nodes' checksums (a damaged node does not stop
the reading), crypto states.

Whatever the bytes: each B-tree node is read once and the nodes of the container at most as many
as its blocks; records, extents and directory entries are those of the nodes read.

## How it does

Measured 2026-11-09 on an M4 (all threads, the default level) through `cargo aqr`, against the
build before (the image compressed as it is), `--stats`:

| | before | after |
|---|---|---|
| APFS image by hdiutil (1,572,864,000 bytes, a GPT): Rust's toolchain `lib` and `/usr/share` copied in (12,410 files), 98% full | 346,283,550 B, 18.3 s, 475 G instructions, 5.57 GB resident | 325,334,908 B, 21.4 s, 871 G instructions, 3.56 GB resident |

Reading the container and making the formula take 0.09 s (`examples/check.rs`).

```sh
cargo run --release -p aqr-apfs --example aqr-apfs-check -- [-v] IMAGE...   # files found, timed, rebuilt
```

## Tests

`cargo rt -p aqr-apfs`: the fixtures (a container of one volume in a GPT: nested directories,
names in Hangul and long ones, 400 files of one directory, a hard link, a symbolic link, a clone,
a sparse file, files in the gaps of a nearly full volume; a container of two volumes), rebuilt
through aqr-disk; images cut short and changed bytes in the first MiBs; data that is no container.
