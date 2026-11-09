# aqr-disk

Disk images taken apart into the contents of their files and the rest ([FORMAT.md](FORMAT.md)):
the part the readers of file systems share. A file system is read by a crate of its own (aqr-iso
for ISO 9660 and UDF, aqr-fat for FAT and exFAT, aqr-ntfs for NTFS, aqr-ext for ext2/3/4; APFS and HFS+ to come,
`docs/disk-image-design.md`), a
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
codecs/aqr-disk/scripts/gen-fixtures.sh   # tests/data: partition tables of hdiutil (macOS)
```

## What it reads

- **Partition tables**: GPT (of 512- or 4096-byte sectors; the primary header, its checksums not
  checked), else MBR (its four entries, and the logical partitions of the extended ones' chains
  of EBRs; a table any of whose entries is not one, as a volume's boot sector, is none), else APM
  (of the block size of the driver descriptor, or 512 bytes; but the map itself and free space).
  The types are not looked at: a volume is known by its file system's own bytes.
- **Volumes**: all of the image, then each partition (but one starting at the image's start, as
  a hybrid ISO image's own are), cut at the image's end; the first file system that starts one
  reads it. A partition's files are found along with all of the image's: of extents of several
  file systems, the first starting where it does is taken (`FORMAT.md`, "Pieces").
- **Files in pieces**: a file whose extents are not contiguous is one content all the same, its
  pieces split from it; a piece overlapping one before it is left in the rest, and the file's
  contents are those taken.

Not read yet: containers (VHD's footer is the rest of a fixed VHD, read as raw; dynamic VHD,
VHDX, VMDK, qcow2, VDI, DMG), holes of sparse files and of zeros as nodes of their own (they are
in the rest, to zstd), file systems' metadata taken apart (`docs/disk-image-design.md`).

## How it does

Measured 2026-11-09 on an M4 (all threads, the default level) through `cargo aqr`, against the build
before aqr-iso's formula moved here: the same archives, byte for byte, of an image of
`/usr/share` (`hdiutil makehybrid -iso -joliet -udf`, 626,591,744 bytes: 124,569,665 B, 511.1
against 510.8 G instructions, 2.0 GB resident at peak either way) and of aqr-iso's five fixtures.
The images' own sizes and times are aqr-iso's README's.

## Tests

`cargo rt -p aqr-disk`: the partition tables of hdiutil (GPT, MBR, APM), logical partitions and
their loops, data that is no table, how extents are laid out, files in pieces (evaluated and
archived), a toy file system in each table and in all of an image, images cut short and changed
bytes. aqr-iso's tests read its fixtures on a partition, and aqr-compress's a JPEG file in pieces
recompressed as one.
