# aqr-mszip

CAB and CHM files taken apart into the contents of their files: a CAB file's MSZIP folders are
rebuilt bit for bit from their data by aqr-flate's module and this crate's (`mszip-recons`), its
LZX folders by aqr-lzx's (`lzx-recons`), its stored folders are their data; a CHM file's LZX
section is rebuilt by aqr-lzx's module from the section's files. The rest of the file is kept as
it is. What it is for is the files: each file's contents are a content of the archive,
recompressed by their formats (a PE file by aqr-exe, a JPEG file by aqr-jpeg) and stored once
where the same files are archived along with it. aqr-compress takes LZX (CAB's LZX folders, CHM
files) from level 9 only, as it is slow (below). Neither container needs a transform of its own
(CHM's reset table, which the LZX stream tells, is kept), so both are here, beside MSZIP.

```sh
cargo run --release -p aqr-mszip --example aqr-mszip-check -- [-v] [-t N] [-P PROFILE] FILE_OR_DIR...   # rebuilt, sizes, speed
cargo run --release -p aqr-mszip --example aqr-mszip-lzx -- [-x DIR] [-r] FILE...   # LZX folders and sections read, against files extracted to DIR
python3 -I codecs/aqr-mszip/scripts/gen-fixtures.py   # tests/data (Python's zlib)
```

```rust
use aqr_mszip::formula;
// None where it is no CAB file, or no folder is taken
let f = formula::encode(&cab, threads).unwrap();
// `f` rebuilds `cab` with `formula::MODULE`, aqr-flate's and aqr-lzx's (`formula::apply` runs them)
let f = formula::encode_chm(&chm, threads).unwrap();   // its LZX section by aqr-lzx's
```

`FORMAT.md` specifies the formula and the module.

## How

An MSZIP block is `CK` and a DEFLATE stream of its own, whose matches may reach 32 KiB back into
the blocks before. aqr-flate takes no preset dictionary, but it does not need one: the blocks of
a folder are one DEFLATE stream but for each block's final flag and the padding after it. So the
folder's blocks are *joined* into that stream (`mszip::join`), which aqr-flate splits into the
folder's data and a record as any other, and `mszip-recons` cuts the blocks back from it, given
how many DEFLATE blocks each holds. Neither decodes the data: the bodies of Huffman blocks are
walked symbol by symbol and copied bit for bit, and only stored blocks, whose padding depends on
where they start, are written again. The data blocks' headers (checksum, sizes, reserved field)
are kept between the blocks.

An LZX folder's blocks are one LZX stream over frames of 32 KiB (the bitstream aligned to a word
after each frame, where the data blocks end), which aqr-lzx splits into its data (e8 undone)
and a record of its parse given the data, as it does WIM's chunks: windows of up to 2 MiB, blocks
of any size over the frames, the e8 header and translation frame by frame. A CHM file's LZX
section resets every few frames: each interval is a stream of its own (the intervals rebuilt in
parallel), the last one of whole frames, whose data past the section's is zeros.

The folder's (or section's) data is cut into its files' contents, by their kinds by their names,
gaps between them kept as unknown data.

## Results

Measured on 2026-10-31 on an Apple M4 (10 cores), on 441 CAB files of Windows (1.80 GB: driver
packages, updates, installers' caches, Office's).

| | |
|---|---|
| folders | 1,058.3 MB MSZIP, 585.8 MB LZX (456 folders, windows of 32 KiB and 2 MiB), 115.7 MB stored |
| rebuilt (`aqr-mszip-check -t 10`) | 441 files of 441, every folder; none otherwise |
| data | 7,969 MB |
| MSZIP records, profile 6 (default) | 48.28 MB, 4.57% of the joined DEFLATE streams (1,057.2 MB) |
| MSZIP records, profile 10 | 12.87 MB, 1.22% |
| MSZIP speed, profile 6 | splitting 129 MB/s of data, rebuilding 176 MB/s |
| MSZIP speed, profile 10 | splitting 45 MB/s, rebuilding 73 MB/s |
| LZX records | 46.10 MB, 7.88% of the streams (584.7 MB) |
| LZX speed, one thread | splitting 22 MB/s of data, rebuilding 37 MB/s |

Through `aqr` (all threads, level mid), the 441 files archive into 1,662.7 -> 1,331.3 MB with the
MSZIP folders (-19.9%; before, the CAB files were archived as they are), compressing in 5.6 ->
73.7 s at a peak footprint of 1.29 -> 3.92 GB, testing in 19.9 s. With the LZX folders taken at
that level too, they came to 1,316.5 MB (-1.1%) in 290 s at 5.38 GB (testing in 21.5 s): hence
LZX from level 9 only, where the level mid is again 1,331.3 MB in 63 s.

The MSZIP records are three times aqr-flate's usual share at the static profiles: Microsoft's
encoder is not among the catalog's rules. Profile 10 (aqr-compress's from level 7) predicts it
far better.

An LZX CAB archived alone gains little: its contents recompressed save about what the records
cost (117 of the files, 150 MB: 135.43 -> 135.10 MB, 0.5 -> 42 s), and aqr-compress keeps most
as they are. What it is for is the files archived along with it: 40 of them (112 MB) with the
files they hold (847 MB, as 7-Zip extracts them) archive into 186.8 -> 124.4 MB (-33%), in 13 ->
54 s at 1.57 -> 2.68 GB. The time is that of the large folders, a stream of 2 MiB windows split
on one thread.

CHM files: 139 of Windows and of programs' help (90.9 MB; windows of 64 and 128 KiB, resets
every 2 or 4 frames), 138 rebuilt (the other is cut 2 bytes short of its content), the records
4.91% of the LZX content (87.5 MB). Through `aqr` at level mid (taken there for the measure):
88.36 -> 72.19 MB (-18.3%), compressing in 3.2 -> 14.8 s at 0.47 -> 0.90 GB, testing in 4.9 s; at
level 9 (as aqr-compress takes them), 69.23 MB in 12.9 s at 1.44 GB.

## Tests

```sh
cargo rt -p aqr-mszip   # the fixtures: MSZIP folders of zlib (several files, fixed codes, Huffman
                        # only, a block alone), stored folders, reserved fields; an LZX folder and
                        # a CHM file of aqr-lzx's fixtures; the limit; files cut short and damaged
```

aqr-lzx's test encoder writes the LZX fixtures (`write_cab_fixture`, `write_chm_fixture`); the
CAB one extracts with 7-Zip as it should. aqr-compress `cabs_taken_apart`: a JPEG file in a CAB
recompressed in turn; aqr-decompress `mszip_same_as_wasm`, `lzx_cab_same_as_wasm`: the modules
natively and as WebAssembly alike.

## Status

Done: CAB files of MSZIP, LZX and stored folders; CHM files' LZX sections. Left:

- Speed of LZX (about 22 MB/s of data splitting, a stream on one thread), to take it below
  level 9; and whether to take it where the probe says it does not pay.
- Quantum folders (none in the corpus); CHM's section 0 files sliced as contents (they are kept).
- The data blocks' checksums and sizes kept as they are (4 bytes of checksum a block, which a
  module could derive), and folders going on in another cabinet.
- A rule for Microsoft's MSZIP encoder in aqr-flate's catalog.
- CAB files in other data: self-extractors are found by the scan, MSI files (CAB in an OLE
  compound file, in sectors) are not.
