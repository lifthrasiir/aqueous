# aqr-disk

Disk images taken apart into the contents of their files and the rest ([FORMAT.md](FORMAT.md)):
the part the readers of file systems share. A file system is read by a crate of its own (aqr-iso
for ISO 9660 and UDF, aqr-fat for FAT and exFAT, aqr-ntfs for NTFS, aqr-ext for ext2/3/4, aqr-apfs
for APFS, aqr-hfs for HFS+ and HFSX; `docs/disk-image-design.md`), a
`FileSystem` that gives the files of a volume and their extents; aqr-disk finds the volumes
(all of the image, and the partitions of its table), lays the files' extents out over the image
and makes the formula. The image is the concatenation of the two, so there is no module: what it
is for is that each file's contents become one content of the archive, recompressed by its format
and stored once where the same files are archived along with it.

```rust
let opt = aqr_disk::formula::Options { file_systems: &[aqr_iso::FS], kind: |_| Kind::Unknown };
// None where no volume is of a file system given, or none of its files has contents
let f = aqr_disk::formula::encode_with(&image, &opt).unwrap();
```

```sh
codecs/aqr-disk/scripts/gen-fixtures.sh     # tests/data: partition tables of hdiutil (macOS)
codecs/aqr-disk/scripts/gen-containers.sh   # tests/data: a disk and its containers by qemu-img (Lima VM)
powershell -File codecs/aqr-disk/scripts/gen-containers.ps1   # tests/data/win*: Windows' (elevated)
codecs/aqr-disk/scripts/gen-dmg.sh          # tests/data/dmg-*: a disk of HFS+ and its UDIF images (macOS)
```

## What it reads

- **Containers** (`container`): dynamic VHD, VHDX, VMDK (a hosted sparse extent), qcow2 (versions
  2 and 3) and VDI, from their published descriptions (Microsoft's VHD specification, [MS-VHDX],
  VMware's Virtual Disk Format 1.1, QEMU's `qcow2.txt`, libyal's notes on VDI). The virtual disk
  is what readers read (an `Image`: each read puts its bytes together from the block table, those
  not allocated zeros), and the files' extents are taken where the container holds them; the
  block tables and headers are the rest. A fixed VHD and a flat VMDK extent are a plain disk.
  A UDIF image (`.dmg`, `dmg`: the `koly` trailer, the property list's `mish` block tables, from
  the format's public descriptions) of raw chunks and zeros only (UDRO, UDRW) is a container as
  well; one of compressed chunks is parsed for aqr-compress, whose formula rebuilds its zlib and
  bzip2 chunks from the disk.
  Not read: differencing disks and those of a parent or backing file, encrypted ones, compressed
  grains and clusters (streamOptimized VMDK; qcow2's compressed clusters read as zeros: stage 8),
  VHDX's log, VHD's and VHDX's sector bitmaps (an allocated block is taken whole).
- **Partition tables**: GPT (of 512- or 4096-byte sectors; the primary header, its checksums not
  checked), else MBR (its four entries, and the logical partitions of the extended ones' chains
  of EBRs; a table any of whose entries is not one, as a volume's boot sector, is none), else APM
  (of the block size of the driver descriptor, or 512 bytes; but the map itself and free space).
  The types are not looked at: a volume is known by its file system's own bytes.
- **Volumes**: all of the image, then each partition (but one starting at the image's start, as
  a hybrid ISO image's own are), cut at the image's end; the first file system that starts one
  reads it. A partition's files are found along with all of the image's: of extents of several
  file systems, the first starting where it does is taken (`FORMAT.md`, "Pieces").
- **Files stored compressed** (`compressed`): a file a file system reports compressed (aqr-ntfs's
  of WOF and of LZNT1) is its data, a content, and its compressed bytes are rebuilt from it:
  LZNT1's units (stored, holes, compressed) by aqr-lznt1's `lznt1-recons`; WOF's chunk table
  kept, its LZX chunks by aqr-lzx's module `lzx-recons`, its XPRESS chunks (XPRESS4K, 8K, 16K) by
  aqr-xpress's `xpress-recons`: applications of 32 MiB of data at most (the modules hold their data
  whole), of streams split of 8 MiB at most of XPRESS (its split takes about 6 bytes a byte). A file
  stored compressed and not taken apart is its compressed bytes, of kind compressed whatever its
  name.
- **Files in pieces**: a file whose extents are not contiguous is one content all the same, its
  pieces split from it; a piece overlapping one before it is left in the rest, and the file's
  contents are those taken.

Not read yet: compressed containers (stage 8; DMGs of other compressions than zlib), holes of sparse files and of zeros as nodes of their own (they are
in the rest, to zstd), file systems' metadata taken apart (`docs/disk-image-design.md`).

## How it does

Measured 2026-11-09 on an M4 (all threads, the default level) through `cargo aqr`, against the build
before aqr-iso's formula moved here: the same archives, byte for byte, of an image of
`/usr/share` (`hdiutil makehybrid -iso -joliet -udf`, 626,591,744 bytes: 124,569,665 B, 511.1
against 510.8 G instructions, 2.0 GB resident at peak either way) and of aqr-iso's five fixtures.
The images' own sizes and times are aqr-iso's README's.

Measured 2026-11-09 on an M4 through `cargo aqr`, against the build before containers were read
(the container compressed as it is), `--stats`: a 2 GB GPT disk of an ext4 volume of Ubuntu's
`/usr/lib` and `/usr/share` (1.7 GB used), as qemu-img converts it:

| | before | after |
|---|---|---|
| dynamic VHDX (1,870,659,584 bytes) | 510,860,772 B, 39.9 s, 9.27 GB resident | 461,927,266 B, 48.6 s, 4.38 GB resident |
| qcow2 (1,791,295,488 bytes) | 515,665,820 B, 42.0 s, 9.40 GB resident | 467,413,878 B, 43.1 s, 4.14 GB resident |
| the disk itself (2,147,483,648 bytes) | | 461,942,344 B, 49.8 s, 5.00 GB resident |

A container archives as its disk does.

Files of NTFS's LZNT1, the same (`compact /c`, 957,014,359 bytes on the volume): 672,774,728 ->
364,216,362 B, 140 -> 3,226 G instructions (7.9 -> 73.8 s), heap peak 1.14 -> 1.63 GB, given back.
Most of the time is the verify decoding blocks again for the values whose hashes are not told
(`docs/disk-image-design.md`, stage 5).

Of all three (aqr-ntfs's peer volume: the same three files by XPRESS4K, 8K, 16K, LZX and LZNT1),
1,106,436 -> 664,744 B: their data deduplicated once made.

Files of WOF's XPRESS4K, the same (`compact /exe:xpress4k`, 854,628,226 bytes of XPRESS): 696,150,574
-> 379,813,648 B, 244 -> 1,930 G instructions (9.2 -> 43.2 s), heap peak 1.27 -> 1.48 GB, given back.
(Streams of 32 MiB took the heap to 2.31 GB, of the budget's 2.00; applications of one stream of 8
MiB each, 399,662,316 B: of more applications, fewer of the files' own formulas fit the bound of
the description.)

Files of WOF's LZX, on an M4 (2026-11-09): a 2 GB fixed VHD a Windows peer session made, 1,500 MiB
of System32 compacted by `compact /exe:lzx` (10,607 files, 1,562,928,797 bytes of data in
630,974,409 of LZX), its free space then written with zeros: against the build before (the WOF
streams as they are) 531,167,800 -> 393,309,474 B, 195 -> 2,136 G instructions (7.2 -> 102.9 s;
57 s in another run of as many instructions), heap peak 0.97 -> 1.72 GB, given back. With the free
space still holding the files as they were before compact: 844,576,448 -> 706,525,686 B. Much of
the time is the verify running `lzx-recons` on one thread (one leaf: `docs/disk-image-design.md`,
stage 3). (Most of the time is the verify of one leaf:
`docs/disk-image-design.md`, stage 3.)

## Tests

`cargo rt -p aqr-disk`: a disk of qemu-img in each container (the virtual disk as the disk is,
its files of ext4 and ext2 where the container holds them, the container rebuilt; compressed
clusters; containers cut short and changed bytes); Windows' own (`scripts/gen-containers.ps1`,
elevated: diskpart's dynamic VHDX and VHD, Hyper-V's New-VHD's VHDX), their NTFS files; hdiutil's
UDIF images of each kind (chunks covering the disk, UDRO's a container; cut short and changed
bytes); the partition tables of hdiutil (GPT, MBR, APM), logical partitions and
their loops, data that is no table, how extents are laid out, files in pieces (evaluated and
archived), a toy file system in each table and in all of an image, images cut short and changed
bytes. aqr-iso's tests read its fixtures on a partition, and aqr-compress's a JPEG file in pieces
recompressed as one.
