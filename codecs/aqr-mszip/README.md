# aqr-mszip

CAB files taken apart into the contents of their files: MSZIP folders are rebuilt bit for bit
from their data by aqr-flate's module and this crate's (`mszip-recons`), stored folders are their
data, and the rest of the file is kept as it is. What it is for is the files: each file's
contents are a content of the archive, recompressed by their formats (a PE file by aqr-exe, a
JPEG file by aqr-jpeg) and stored once where the same files are archived along with the CAB.

```sh
cargo run --release -p aqr-mszip --example aqr-mszip-check -- [-v] [-t N] [-P PROFILE] FILE_OR_DIR...   # rebuilt, sizes, speed
python3 -I codecs/aqr-mszip/scripts/gen-fixtures.py   # tests/data (Python's zlib)
```

```rust
use aqr_mszip::formula;
// None where it is no CAB file, or no folder is taken
let f = formula::encode(&cab, threads).unwrap();
// `f` rebuilds `cab` with `formula::MODULE` and aqr-flate's (`formula::apply` runs both)
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

The folder's data is cut into its files' contents, by their kinds by their names, gaps between
them kept as unknown data.

## Results

Measured on 2026-10-31 on an Apple M4 (10 cores), on 441 CAB files of Windows (1.80 GB: driver
packages, updates, installers' caches, Office's).

| | |
|---|---|
| folders | 1,058.3 MB MSZIP, 585.8 MB LZX, 115.7 MB stored |
| rebuilt (`aqr-mszip-check -t 10`) | 311 files of 441, every MSZIP and stored folder; none otherwise |
| data | 3,483 MB |
| records, profile 6 (default) | 48.28 MB, 4.57% of the joined DEFLATE streams (1,057.2 MB) |
| records, profile 10 | 12.87 MB, 1.22% |
| speed, profile 6 | splitting 129 MB/s of data, rebuilding 176 MB/s |
| speed, profile 10 | splitting 45 MB/s, rebuilding 73 MB/s |

Through `aqr` (all threads, level mid), the 441 files archive into 1,662.7 -> 1,331.3 MB
(-19.9%; before, the CAB files were archived as they are), compressing in 5.6 -> 73.7 s at a
peak footprint of 1.29 -> 3.92 GB, testing in 19.9 s and 2.10 GB.

The records are three times aqr-flate's usual share at the static profiles: Microsoft's encoder
is not among the catalog's rules. Profile 10 (aqr-compress's from level 7) predicts it far
better.

## Tests

```sh
cargo rt -p aqr-mszip   # the fixtures: MSZIP folders of zlib (several files, fixed codes, Huffman
                        # only, a block alone), stored folders, reserved fields; the limit; files
                        # cut short and damaged
```

aqr-compress `cabs_taken_apart`: a JPEG file in a CAB recompressed in turn; aqr-decompress
`mszip_same_as_wasm`: the module natively and as WebAssembly alike.

## Status

Done: CAB files of MSZIP and stored folders. Left:

- LZX folders (33% of the corpus's folders): aqr-lzx reads WIM's chunks only (a 32 KiB window,
  each chunk alone); CAB's are streams of 32 KiB frames with windows up to 2 MiB.
- Quantum folders (none in the corpus).
- The data blocks' checksums and sizes kept as they are (4 bytes of checksum a block, which a
  module could derive), and folders going on in another cabinet.
- A rule for Microsoft's MSZIP encoder in aqr-flate's catalog.
- CAB files in other data: self-extractors are found by the scan, MSI files (CAB in an OLE
  compound file, in sectors) are not.
