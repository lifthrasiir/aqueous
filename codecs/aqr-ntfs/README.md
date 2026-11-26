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

## What is taken

There is no specification of NTFS; this is written from the Linux-NTFS project's documentation of
the format (no implementation's code read). mkntfs and ntfs-3g make a fixture, and Windows itself
(diskpart, format, compact, fsutil) another, as black boxes.

- The boot sector, and the MFT through the runs of its own `$DATA` and its extension records, each
  record's fixups checked (a record whose fixups do not match is not read).
- Of each record in use: its `$FILE_NAME` (a DOS name only where there is no other; a hard link's
  file is one) and its non-resident `$DATA` attributes, unnamed and named (alternate data
  streams, `path:name`); a stream's runs are put in order of their VCNs across extension records.
- Paths by the parents of `$FILE_NAME` up to the root (at most 256 deep). A stream's contents are
  its clusters up to its initialized size, holes of a sparse stream left out.
- WOF's files (a reparse point of WOF's file provider): the file itself, its contents
  `WofCompressedData`'s, compressed by the algorithm the reparse point tells (aqr-disk takes the
  data apart).
- Compressed streams (LZNT1): the clusters of each compression unit (stored, compressed, or a
  hole), so that aqr-disk takes the data apart.

Whatever the bytes: records are read at most as many as the volume could hold, runs as far as they
decode, the MFT extended only by a piece starting where its runs end.

Stays in the rest: resident data (in its record), encrypted streams, the system files (records 0
to 15, and those below `$Extend`), directories and their indexes, free clusters (which may hold the
data of deleted files).

## Results

On an M4 through `cargo aqr` (all threads, the default level, 2026-11-09), against the volume
compressed as it is: an NTFS volume by mkntfs (734,003,200 bytes, 4 KiB clusters; Ubuntu's
`/usr/share`, a third of its files deleted, `/usr/lib/python3*` written after: 25,542 files, 48 in
pieces; 67% full):

| | before | after |
|---|---|---|
| archived, time, resident | 132,467,037 B, 19.0 s, 3.53 GB | 123,846,119 B, 13.5 s, 2.33 GB |

On Windows (an AMD Ryzen 7 9700X), a fixed VHD of 3 GB that Windows formatted, a copy of System32
(4,769 files), a third deleted and Program Files written after, full (3,768 files non-resident, 52
in pieces): 939.0 -> 871.5 MB, 167 -> 100 s, resident peak 8.86 -> 4.53 GB; `aqr -d` gives it back
in 4.8 s. The rest is 340 MB, much of it free clusters holding deleted files. Reading the volume
and making the formula take 0.2 s.

## Tests

`cargo rt -p aqr-ntfs`:

- The fixture of mkntfs and ntfs-3g (resident and non-resident files, a hard link, an alternate
  data stream, a sparse file, a compressed one, files in pieces), rebuilt through aqr-disk.
- Windows' (a fixed VHD of an MBR: the same, a WOF-compressed file, a file of 384 runs in an
  extension record).
- Volumes cut short and changed bytes, data that is no volume.
- A volume made by the test, of what ntfs-3g did not make (the runs of a file and of the MFT
  itself in extension records, an initialized size short of the data, fixups that do not match).

aqr-compress's tests archive the fixture.

## Left

- **MFT records.** The MFT's records themselves are not taken apart: they stay in the rest, with
  the directories' indexes.
- **Resident data.** Small files (in their record) are not taken, nor are encrypted streams.
- **Memory.** The heap passed its budget on the System32 volume (3.48 GB of 2.00 GB, most of it the
  verify's: `docs/disk-image-design.md`, stage 3).
