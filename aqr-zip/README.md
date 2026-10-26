# aqr-zip

ZIP recompression. An archive is rebuilt bit for bit from the contents of its entries, so the
archive stores the contents instead: they compress better, and are recompressed in turn (a JPEG
or a gzip in a ZIP is found as such). DEFLATE bodies are rebuilt by aqr-flate from the data and
a record. Everything else is kept in a *layout*, with every fixed field stored as its XOR with a
prediction: CRCs, sizes, offsets and what the central directory repeats of the local headers
cost nothing when they are what they should be, and are kept exactly when they are not.

```sh
cargo run --release -p aqr-zip --example aqr-zip-check -- [-v] [-t N] FILE_OR_DIR...   # rebuilt, zstd -3 before and after, speed
aqr-zip/scripts/gen-fixtures.sh                    # tests/data from python3, Info-ZIP, ditto, bsdtar and Go
python3 aqr-zip/scripts/make-42.py OUT.zip         # 42.zip's levels without its password
```

```rust
use aqr_zip::formula::{self, MODULE};
// None where there is no local entry
let f = formula::encode(&archive, 8).unwrap();
// `f` rebuilds `archive` when evaluated with `formula::decode` as the module `MODULE`
```

`FORMAT.md` specifies the module `zip-recons`.

## What it takes

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

## Bodies that would not pay

With `Options::probe` (aqr-compress gives one at level `mid`), a DEFLATE body of 256 KiB or more
is rebuilt only where its record and its data may archive in at least 1/32 less than it
(`aqr_formula::probe`): the record estimated by aqr-flate on runs of the stream, the data at
the fewest bytes the archive's level may take it in, the files in it that are recompressed in
turn counted as nothing, and so is what the other entries of its kind hold of it (by anchors of
a rolling hash: fuzzing corpora and near copies compress together, which no sample of one entry
tells). The others are kept as they are. Smaller bodies are rebuilt as before: the contents of
small entries compress far better together than alone, and cost little. The bodies are parsed
first (in parallel), probed, then the ones taken deconstructed (in parallel).

## Zip bombs and 42.zip

Bodies the same as an earlier one are taken again, without looking into them: in 42.zip, each
of the 16 archives of a level is the same, so each level inflates one of them. The data that
DEFLATE bodies inflate to is bounded (`Options::limit`, which aqr-compress gives from its budget
for the whole file, 32 times its size and 64 MiB at every depth together). Where the sizes the
headers say fit, the bodies inflate at once, each stopped as soon as it holds more than it says.
Otherwise, or where one was stopped, the bodies are measured by a parse that keeps no data, then
inflated least expanding first, so that bombs, the most expanding, are kept as their DEFLATE
streams. Those are periodic, and cost the archive little.

`make-42.py` writes 42.zip's levels unencrypted (32,934 bytes: five levels of 16 archives each, down to a
page holding 4,294,967,295 zero bytes). aqr-compress takes it to any depth in 0.14 s and 41 MB:
five levels of one archive each, the zeros kept deflated. zstd already finds the repetition in
it (2,127 bytes archived as it is, 2,213 through the formulas), so the file is kept as it is.

## Results

Measured on 2026-10-23 on the ZIP files of one Mac (172 archives, 1.34 GB: Python wheels, Go
module test data, Office documents, an APK, a VS Code build and others), on an M4.

- `aqr-zip-check` rebuilds all 172, none failing: 12,452 bodies stored, 92,304 deflated, 7,971
  the same as an earlier one, and 3 kept. On 8 threads the encoder runs at 46 MB/s of archive,
  and the module at 66 MB/s.
- Through aqr-compress, the Aqueous archive (level mid) is 8.2% smaller than that of the files
  as they are, and 9.2% with the contents recompressed in turn (depth 3). Most of the bytes are
  in a few large archives of already compressed media and binaries; the 118 smallest (12.2 MB)
  come out 22.6% smaller.
- The layout is 1.2% of what the inputs take under zstd -3 (2.0 MB of 1.09 GB), the records
  4.8%.
- One thread, through `aqr-show` (see the [top-level README](../README.md#how-it-was-measured);
  ratios tentative): enwik9.zip (322 MB, one entry of 1 GB of text) comes to 0.957 (zstd -3:
  1.000) at 8.3 MB/s compressing and 23 MB/s decompressing, with a peak RSS of 4.5 GB (14×
  the archive). Two Python wheels (3.1 MB) come to 0.760 (zstd -3: 0.950).

Variants of pictures (with layers shown or hidden, "差分"), as they are distributed: seven ZIP
archives of Clip Studio Paint PNG files (684 MB, in groups of 13 to 88 of the same size, one
archive cut short), measured on 2026-10-26 through `aqr-show -t 8`. The PNG files alike are
coded against each other as sets (aqr-compress, aqr-png's `png-set`): the archives come to
114.1 MB, against 497.1 MB with each file coded alone (and the archive cut short kept raw,
before it was taken), at 28-39 MB/s compressing (44-54 alone) and 84-167 MB/s
decompressing (71-90 alone).

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
