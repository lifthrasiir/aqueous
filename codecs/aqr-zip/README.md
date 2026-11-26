# aqr-zip

ZIP recompression. An archive is rebuilt bit for bit from the contents of its entries, so the
archive stores the contents instead: they compress better, and are recompressed in turn (a JPEG
or a gzip in a ZIP is found as such). DEFLATE bodies are rebuilt from the data and a record by
aqr-flate's module (`flate-set`, one application for all of them), whose output the module
takes with the CRC-32 and sizes it derives, so `zip-recons` itself has no DEFLATE code.
Everything else is kept in a *layout* that costs nothing where the headers are what they should
be, and keeps them exactly where they are not.

```sh
cargo run --release -p aqr-zip --example aqr-zip-check -- [-v] [-t N] FILE_OR_DIR...   # rebuilt, zstd -3 before and after, speed
codecs/aqr-zip/scripts/gen-fixtures.sh             # tests/data from python3, Info-ZIP, ditto, bsdtar and Go
python3 codecs/aqr-zip/scripts/make-42.py OUT.zip  # 42.zip's levels without its password
```

```rust
use aqr_zip::formula::{self, MODULE};
// None where there is no local entry
let f = formula::encode(&archive, 8).unwrap();
// `f` rebuilds `archive` when evaluated with `formula::apply` (`MODULE` and aqr-flate's)
```

`FORMAT.md` specifies the module `zip-recons`, and how it works.

## What is taken

The archive is read from its start, record after record. Whatever is not a record is a gap,
kept as it is, so any file starting with a local header is rebuilt, however odd:

- Data before the archive (self-extractors), with offsets from either start; data after it;
  comments holding signatures; several archives one after another.
- Data descriptors in all four forms (with or without signature, 32- or 64-bit sizes); zip64
  fields in the local and central headers, the zip64 end records; sizes of `0xFFFFFFFF` with no
  zip64 field (42.zip's `0.dll`).
- Central headers in another order than the entries, repeated, or pointing nowhere; entries
  the central directory does not list (a stream cut short, deleted entries); entries overlapping
  ("better zip bombs").
- Encrypted entries and other methods (bzip2, LZMA, AES...), kept as they are; DEFLATE streams
  that do not fill their body, or are broken, kept too.

The central directory, found from the end, only says where bodies end when the local headers
do not, and a record it does not know of may not run over an entry it does (signatures turn up
in self-extractors' code).

Where an archive is in other data (as aqr-compress finds it), it ends with its end record, or
else, cut short as downloads often are, with the data: where a whole entry comes first and the
records follow each other up to one that the data cuts short (`zip::extent`). Without an end
record, other data right after an entry is not taken for the rest of the archive, nor is it.

## Limits

- With `Options::probe` (aqr-compress gives one at level `mid`), a DEFLATE body of 256 KiB or
  more is rebuilt only where its record and its data may archive in at least 1/32 less than it
  (`aqr_formula::probe`); the others are kept as they are. Smaller bodies are always rebuilt.
- The data that DEFLATE bodies inflate to is bounded (`Options::limit`; aqr-compress gives 32
  times the file's size and 64 MiB at every depth together), so zip bombs are kept as their
  DEFLATE streams, which are periodic and cost the archive little. 42.zip (`make-42.py` writes
  its levels without the password: 32,934 bytes) is taken to any depth in 0.14 s and 41 MB, but
  zstd already finds its repetition (2,127 bytes as it is, 2,213 through the formulas), so the
  file is kept as it is.

## Results

On the ZIP files of one Mac (172 archives, 1.34 GB: Python wheels, Go module test data, Office
documents, an APK, a VS Code build and others), M4, 2026-10-23. All 172 rebuild: 12,452 bodies
stored, 92,304 deflated, 7,971 the same as an earlier one, 3 kept.

- Encoder 46 MB/s of archive on 8 threads, module 66 MB/s.
- Through aqr-compress (level mid) the Aqueous archive is 8.2% smaller than that of the files as
  they are, 9.2% with the contents recompressed in turn (depth 3); the 118 smallest archives
  (12.2 MB) come out 22.6% smaller (most bytes are in a few large archives of already
  compressed media and binaries). The layout is 1.2% of what the inputs take under zstd -3,
  the records 4.8%.
- One thread through `aqr-show` (see the [file formats overview](../../docs/file-formats.md#how-it-was-measured)):
  enwik9.zip (322 MB) 0.957 (zstd -3: 1.000) at 8.3 / 23 MB/s compressing / decompressing, peak
  RSS 4.5 GB; two Python wheels (3.1 MB) 0.760 (zstd -3: 0.950).
- Variants of pictures ("差分"): seven ZIP archives of Clip Studio Paint PNG files (684 MB) come
  to 114.2 MB, against 497.1 MB with each PNG coded alone, at 47-79 MB/s compressing and 99-209
  MB/s decompressing (`aqr-show -t 8`, 2026-10-26; the PNG files alike coded against each other
  as sets, aqr-png's `png-set`).

## Tests

- `tests/formula.rs`: the fixtures of `tests/data`, rebuilt with no body kept; archives written
  by hand with every descriptor form, zip64, data around them, central headers in other orders
  or pointing nowhere, lying headers, encrypted and other methods, overlapping entries, cuts; the
  inflation limit; a nest like 42.zip; damaged layouts (errors, never panics) and damaged
  archives (still rebuilt).
- aqr-compress `tests/compress.rs` (`zip_nested`, `zip_cut`, `zip_png_set`, `zip_nested_like_42`):
  contents recompressed in turn, archives found in data and in gzip, an archive cut short,
  variants of a picture as a set, and six levels of nested archives over 96 MiB of zeros within
  the budget; `src/extent.rs` (`zip_cut`): the
  fixtures cut at every byte after their first entry.
- `tests/corrupt.rs`: inputs claiming more than they can be (an error, not an aborting
  allocation); `tests/zip64_locator.rs`: the zip64 end record's locator.

## Left

- **Memory of huge entries.** enwik9.zip peaks at 4.5 GB resident (14x the archive).
- **Other methods.** Bodies of bzip2, LZMA and the like in ZIP are kept as they are (aqr-bz2's
  "bzip2 in ZIP/7z" is left too); encrypted entries are kept.
