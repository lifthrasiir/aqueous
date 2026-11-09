# aqr-ntfs

NTFS volumes (Windows' system and data volumes, in VM disks and backups) read into their files: a
file system of aqr-disk (`FS`), which takes the image apart into the contents of the files and the
rest ([../aqr-disk/FORMAT.md](../aqr-disk/FORMAT.md)), all of an image or a partition of it. There
is no module: each file's contents become one content of the archive, recompressed by its format
and stored once where the same files are archived along with it; a file in pieces is one content,
its pieces split from it.

```sh
codecs/aqr-ntfs/scripts/gen-fixtures.sh                                     # tests/data/ntfs.img, in the Lima VM aqr-linux
powershell -File codecs/aqr-ntfs/scripts/gen-fixtures.ps1                   # tests/data/windows.vhd, elevated, on Windows
cargo run --release -p aqr-ntfs --example aqr-ntfs-check -- [-v] IMAGE...   # files found, timed, rebuilt
```

## What it reads

There is no specification of NTFS; this is written from the Linux-NTFS project's documentation of
the format (no implementation's code read). mkntfs and ntfs-3g make a fixture, and Windows itself
(diskpart, format, compact, fsutil) another, as black boxes.

- The boot sector (its OEM name, the signature, the sizes of a cluster and of an MFT record).
- The MFT, through the runs of its own `$DATA`, and those its extension records add as they are
  come to; each record's fixups (a record whose fixups do not match is not read).
- Of each record in use: its `$FILE_NAME` (a DOS name only where there is no other: a hard link's
  file is one, of one of its names) and its non-resident `$DATA` attributes, unnamed and named
  (alternate data streams, `path:name`). An extension record's attributes are its base record's,
  so a stream's runs are put in order of their VCNs across them (as an attribute list tells, not
  read itself).
- Paths by the parents of `$FILE_NAME` up to the root (at most 256 deep). A stream's contents are
  its clusters up to its initialized size, holes of a sparse stream left out.

- WOF's files (a reparse point of WOF's file provider): the file itself, its contents
  `WofCompressedData`'s, compressed by the algorithm the reparse point tells, of the unnamed
  stream's size (aqr-disk takes the data apart: its README).

Left in the rest: resident data (in its record), compressed and encrypted streams (their clusters
as they are: LZNT1 is stage 5 of `docs/disk-image-design.md`), the system files (records 0 to 15,
and those below `$Extend`), directories and their indexes, free clusters (which may hold the data
of deleted files). The MFT's records themselves are not taken apart yet.

Whatever the bytes: records are read at most as many as the volume could hold, runs as far as they
decode, the MFT extended only by a piece starting where its runs end.

## How it does

Measured 2026-11-09 on an M4 (all threads, the default level) through `cargo aqr`, against the
build before (the volume compressed as it is), `--stats`:

| | before | after |
|---|---|---|
| NTFS volume by mkntfs (734,003,200 bytes, 4 KiB clusters): Ubuntu's `/usr/share`, a third of its files deleted, `/usr/lib/python3*` written after (25,542 files, 21,176 non-resident, 48 in pieces), 67% full | 132,467,037 B, 19.0 s, 535 G instructions, 3.53 GB resident | 123,846,119 B, 13.5 s, 520 G instructions, 2.33 GB resident |

On Windows (an AMD Ryzen 7 9700X, 16 workers; measured by a peer session at d5bba8c1, before the
split's outputs were hashed in one pass), a fixed VHD of 3 GB that Windows formatted, a copy of
System32 (4,769 files), a third deleted and Program Files written after, full (3,768 files
non-resident, 52 in pieces, the most 376): 939,041,266 -> 871,541,538 B, 167 -> 100 s, resident
peak 8.86 -> 4.53 GB, given back by `aqr -d` in 4.8 s. The heap passed its budget then (3.48 GB of
2.00 GB, most of it the verify's: `docs/disk-image-design.md`, stage 3).

Reading the volume and making the formula take 0.2 s (`examples/check.rs`). Before, the scan of
the raw volume found compressed files in it alone, one after another; now they are files, made
apart. The rest is 340 MB, much of it free clusters holding deleted files.

## Tests

`cargo rt -p aqr-ntfs`: the fixture of mkntfs and ntfs-3g (resident and non-resident files, a hard
link, an alternate data stream, a sparse file, a compressed one, files in pieces), rebuilt through
aqr-disk; Windows' (a fixed VHD of an MBR: the same, a WOF-compressed file, and a file of 384 runs,
in an extension record); volumes cut short and changed bytes; data that is no volume; and a volume made by the
test of what ntfs-3g did not make (the runs of a file and of the MFT itself in extension records,
an initialized size short of the data, fixups that do not match). aqr-compress's tests archive the
fixture.
