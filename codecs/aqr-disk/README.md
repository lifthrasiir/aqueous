# aqr-disk

Disk images taken apart into the contents of their files and the rest, so that each file is
recompressed by its format and stored once where the same files are archived along with it.
`FORMAT.md` specifies the formula, and how the images are read. A file system is read by a crate
of its own (aqr-iso for ISO 9660 and UDF, aqr-fat for FAT and exFAT, aqr-ntfs for NTFS, aqr-ext
for ext2/3/4, aqr-apfs for APFS, aqr-hfs for HFS+ and HFSX; `docs/disk-image-design.md`); aqr-disk
finds the volumes, lays the files' extents out over the image and makes the formula. The image
is the concatenation of the files' contents and the rest, so there is no module.

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

## What is taken

- **Containers**: dynamic VHD, VHDX, VMDK (hosted sparse extents, streamOptimized too), qcow2
  (versions 2 and 3, compressed clusters too), VDI, UDIF (`.dmg`, raw or compressed chunks) and
  EnCase's E01 segment files; a fixed VHD and a flat VMDK extent are plain disks. A container
  archives as its disk does; the compressed chunks of a qcow2, a VMDK, a UDIF or an E01 are
  rebuilt by aqr-compress from the disk.
- **Partition tables**: GPT (512- or 4096-byte sectors), MBR (with logical partitions), APM.
- **Volumes**: all of the image, then each partition, by the file systems given.
- **Files stored compressed**: NTFS's WOF (XPRESS4K, 8K, 16K, LZX) and LZNT1 files are their data,
  rebuilt by aqr-lzx, aqr-xpress and aqr-lznt1; applications of 32 MiB of data at most.
- **Files in pieces**: one content all the same.

Not taken: differencing disks and those of a parent or backing file, encrypted containers,
qcow2's zstd clusters, VHDX's log, VHD's and VHDX's sector bitmaps (an allocated block is taken
whole).

## Results

On an M4 through `cargo aqr` (all threads, the default level, 2026-11-09), each against the volume
or container compressed as it is:

| input | before | after |
|---|---|---|
| ext4 disk of 2 GB (Ubuntu's `/usr/lib` and `/usr/share`), as a dynamic VHDX (1.87 GB) | 510.9 MB, 39.9 s, 9.27 GB resident | 461.9 MB, 48.6 s, 4.38 GB |
| the same as qcow2 (1.79 GB) | 515.7 MB, 42.0 s, 9.40 GB | 467.4 MB, 43.1 s, 4.14 GB |
| NTFS volume of LZNT1 files (`compact /c`, 957 MB on the volume) | 672.8 MB, 7.9 s | 364.2 MB, 73.8 s |
| NTFS volume of WOF XPRESS4K files (854.6 MB of XPRESS) | 696.2 MB, 9.2 s | 379.8 MB, 43.2 s |
| 2 GB fixed VHD with 1,500 MiB of System32 under WOF LZX (631 MB of LZX) | 531.2 MB, 7.2 s | 393.3 MB, 102.9 s |

The time of files stored compressed is mostly the verify of the module (`lzx-recons` on one
thread; `docs/disk-image-design.md`, stages 3 and 5). The ISO 9660 and UDF formula is
byte for byte what aqr-iso made before: an image of `/usr/share` (627 MB) archives to 124.6 MB.

## Tests

`cargo rt -p aqr-disk`:

- Containers: a disk of qemu-img in each (the virtual disk as the disk is, its ext4 and ext2
  files, the container rebuilt, compressed clusters, containers cut short and changed bytes);
  Windows' own (`scripts/gen-containers.ps1`, elevated: diskpart's dynamic VHDX and VHD,
  Hyper-V's VHDX) with their NTFS files.
- Chunked images: the chunks of qemu-img's compressed qcow2 and streamOptimized VMDK, of
  ewfacquire's E01 (one file, and segment files) and hdiutil's UDIF images of each kind (cut
  short, changed bytes).
- Partition tables of hdiutil (GPT, MBR, APM), logical partitions and their loops, data that is no
  table.
- Layout: how extents are laid out, files in pieces (evaluated and archived), a toy file system
  in each table and in all of an image, images cut short and changed bytes.

aqr-iso's tests read its fixtures on a partition, and aqr-compress's a JPEG file in pieces
recompressed as one.

## Left

- **EWF2.** `.Ex01` files are not read.
- **Holes.** Holes of sparse files and of zeros are not nodes of their own: they stay in the rest,
  to zstd.
- **File systems' metadata.** Directories, inode tables and the like stay in the rest, coded by
  zstd (`docs/disk-image-design.md`).
- **Speed of compressed files.** WOF and LZNT1 files spend most of their time in the verify on one
  leaf (7.9 -> 73.8 s for LZNT1, 7.2 -> 102.9 s for LZX); spreading it over the files is where to
  start.
