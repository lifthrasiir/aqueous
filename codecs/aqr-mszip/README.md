# aqr-mszip

CAB and CHM files taken apart into the contents of their files: a CAB file's MSZIP folders are
rebuilt bit for bit from their data by aqr-flate's module and this crate's (`mszip-recons`, a part module), its
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

`FORMAT.md` specifies the formula and the module, and how it works.

## What is taken

- CAB files whose folders are MSZIP, LZX or stored (type 0, 1, 3): the folder's data is cut into
  its files' contents, each recompressed by its format and stored once where the same files are
  archived along. Gaps between files are kept as unknown data.
- CHM files (ITSF 3) whose LZX section has control and reset tables of version 2.
- Not taken: Quantum folders (none seen), folders going on in another cabinet; CHM's section 0
  files are kept as they are.

## Results

On 441 CAB files of Windows (1.80 GB: driver packages, updates, installers' caches, Office's),
Apple M4 (10 cores), 2026-10-31. All 441 rebuild, every folder.

| | |
|---|---|
| MSZIP records, profile 6 (default) | 4.57% of the joined DEFLATE streams (1,057.2 MB): 48.28 MB |
| MSZIP records, profile 10 | 1.22%: 12.87 MB |
| MSZIP speed, profile 6 | splitting 129 MB/s of data, rebuilding 176 MB/s |
| MSZIP speed, profile 10 | splitting 45 MB/s, rebuilding 73 MB/s |
| LZX records | 7.88% of the streams (584.7 MB): 46.10 MB |
| LZX speed, one thread | splitting 22 MB/s of data, rebuilding 37 MB/s |

Through `aqr` (all threads, level mid) the 441 files archive into 1,331.3 MB from 1,662.7 MB
(-19.9%), compressing in 73.7 s (5.6 s as they were) at a peak of 3.92 GB, testing in 19.9 s.
aqr-compress takes LZX from level 9 only: at mid it gained 1.1% more in 290 s against 73.7 s. The MSZIP
records are three times aqr-flate's usual share at the static profiles (Microsoft's encoder is
not among the catalog's rules); profile 10, which aqr-compress uses from level 7, predicts it
far better.

An LZX CAB archived alone gains about what its records cost (117 files, 150 MB: 135.43 ->
135.10 MB), and aqr-compress keeps most as they are; it pays for the files archived along with
it: 40 of them (112 MB) with the files they hold (847 MB) archive into 124.4 MB, against 186.8 MB
as they were, in 54 s at 2.68 GB.

CHM files: 139 of Windows and of programs' help (90.9 MB), 138 rebuilt (the other is cut 2 bytes
short of its content), the records 4.91% of the LZX content (87.5 MB). Through `aqr` at level 9:
88.36 -> 69.23 MB (-21.7%) in 12.9 s at 1.44 GB.

## Tests

- `tests/formula.rs`: the fixtures of `scripts/gen-fixtures.py` (MSZIP folders of zlib, several
  files, fixed codes, Huffman only, stored folders, reserved fields), and an LZX folder and a
  CHM file of aqr-lzx's fixtures (`write_cab_fixture`, `write_chm_fixture`; the CAB one extracts
  with 7-Zip) taken apart and given back.
- `tests/edge.rs`: CAB structure's edge cases (PREV/NEXT, signatures, trailing bytes, odd block
  lists), `join`, the module's refusals, the limit.
- `tests/part.rs`: `mszip-recons` as a part module against the whole decoding, on folders past
  the windows.
- `tests/corrupt.rs`: inputs claiming more than they can be, files cut short and damaged: an
  error, not an allocation that aborts.
- aqr-compress `cabs_taken_apart` (a JPEG in a CAB recompressed in turn); aqr-decompress
  `mszip_same_as_wasm`, `lzx_cab_same_as_wasm` (the modules natively and as WebAssembly alike).

## Left

- **Speed of LZX.** About 22 MB/s of data splitting (a stream on one thread) keeps it to level 9
  and over; whether to take it where the probe says it does not pay.
- **Quantum folders.** None in the corpus. CHM's section 0 files are kept, not sliced as
  contents.
- **Data blocks' checksums and sizes.** Kept as they are (4 bytes of checksum a block, which a
  module could derive); folders going on in another cabinet.
- **A rule for Microsoft's MSZIP encoder** in aqr-flate's catalog (its records are 3x the usual
  share at the static profiles).
- **CAB files in other data.** Self-extractors are found by the scan; MSI files' cabinets (in an
  OLE compound file, in sectors) are put together by aqr-ole through aqr-disk.
