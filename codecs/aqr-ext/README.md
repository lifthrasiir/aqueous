# aqr-ext

ext2, ext3 and ext4 volumes (Linux VMs and their backups, Raspberry Pi and Android images) read
into their files: a file system of aqr-disk (`FS`), which takes the image apart into the contents
of the files and the rest ([../aqr-disk/FORMAT.md](../aqr-disk/FORMAT.md)), all of an image or a
partition of it. There is no module: each file's contents become one content of the archive,
recompressed by its format and stored once where the same files are archived along with it; a
file in pieces is one content, its pieces split from it.

```sh
codecs/aqr-ext/scripts/gen-fixtures.sh                                    # tests/data, in the Lima VM aqr-linux
cargo run --release -p aqr-ext --example aqr-ext-check -- [-v] IMAGE...   # files found, timed, rebuilt
```

## What it reads

Written from the kernel's `Documentation/filesystems/ext4/` (the documentation alone; no
implementation's code read). mke2fs and the kernel's drivers on a loop mount make the fixtures,
as black boxes.

- The superblock; the group descriptors (32 bytes, or `s_desc_size` with `64bit`), where
  `meta_bg` puts them too (each block of them in the first group of its groups, after its
  superblock's backup); the inode tables they tell (`inode_tables`).
- The directories from the root, their entries read linearly (an htree's index blocks are the
  empty entries they are made to look like), each once, at most 256 deep.
- A regular file's blocks, each file once (a hard link's of the name found first): by its extent
  tree (ext4; at most 8 deep), or its block map (direct, indirect, double and triple indirect
  blocks: ext2, ext3). Its contents are its blocks up to its size, holes and unwritten extents
  left out.

Left in the rest: inline data (in the inode), encrypted files, symbolic links, directories,
extended attributes, the journal, the bitmaps and inode tables, free blocks (which may hold the
data of deleted files). Not read: checksums (metadata_csum's: not checked, so a damaged one does
not stop the reading).

Whatever the bytes: blocks followed in all (extents, indirect blocks) at most twice the volume's,
directory bytes read in all at most the volume's.

## How it does

Measured 2026-11-09 on an M4 (all threads, the default level) through `cargo aqr`, against the
build before (the volume compressed as it is), `--stats`:

| | before | after |
|---|---|---|
| ext4 volume by mke2fs (1,572,864,000 bytes, 4 KiB blocks): Ubuntu's `/usr/share` and `/usr/lib` until full, a third of the files deleted, `/usr/include` and `/usr/bin` written after (34,599 files, 134 in 7,297 pieces), 98% full | 478,313,306 B, 31.4 s, 882 G instructions, 8.15 GB resident | 425,659,792 B, 53.0 s, 1,989 G instructions, 3.69 GB resident |

Reading the volume and making the formula take 0.2 s (`examples/check.rs`). The time is half the
verify (26.6 s on one thread): the image is one leaf of the archive, whose thousands of
applications (5,848 `.ko.zst` modules among the files) are evaluated one by one, each decoding
large blocks again (`docs/disk-image-design.md`, stage 3). Decompressing it takes 1.4 s.

## Tests

`cargo rt -p aqr-ext`: the fixtures (ext4 of 4 KiB blocks with inline data, an htree directory,
unwritten extents, a hole, a hard link, a symbolic link and files in pieces in an extent tree;
ext2 of 1 KiB blocks, files in indirect and double indirect blocks; ext4 of meta_bg, its inode
tables where debugfs tells and its files of groups 16 on found), rebuilt through aqr-disk; volumes
cut short and changed bytes; data that is no volume. aqr-compress's tests archive the ext4
fixture.
