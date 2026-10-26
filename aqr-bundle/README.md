# aqr-bundle

Archives that only bundle files, rebuilt bit for bit from the contents of their members
(`FORMAT.md`): ar (System V and GNU, BSD, Windows libraries), tar (v7, ustar, GNU, pax) and cpio
(newc, crc, odc, binary of either byte order). The headers are kept, their sizes, name sizes and
checksums derived from the contents; padding of zeros is not stored.

Nothing is gained on an archive alone: what it is for is that its contents become contents of
the application, to be recompressed by their formats (a PNG file in a tar archive by aqr-png,
the objects of an ar library by aqr-exe), and stored once where the same files are archived
along with it. A tar archive in gzip (`.tar.gz`, `.tgz`, `.crate`, Homebrew bottles) is taken in
the gzip's data.

## How it does

Measured 2026-10-26 on an Apple M4 (10 cores), through `aqr` (all threads, level mid) against
the commit before aqr-bundle (`e6ec924`), and with `aqr-bundle-check`.

| | before | after | time | peak footprint |
|---|---|---|---|---|
| the 10gb corpus (79,431 files, 10.0 GB) | 2810.8 MB | 2775.0 MB (−1.3%) | 109.8 → 106.0 s | 8.59 → 7.49 GB |
| `gimp.tar` and its 4143 files, calgary (163 MB) | 26.78 MB | 14.25 MB | 2.1 → 1.7 s | |
| mingw of the 10gb corpus (1.5 GB, 4660 ar archives) | 344.60 MB | 342.05 MB | 13.3 → 15.7 s | |
| 68 Homebrew bottles (`.tar.gz`, 281 MB) | 219.99 MB | 219.70 MB | 16.3 → 17.3 s | 5.56 → 7.72 GB |
| 1500 crates (`.crate`, 58 MB) | 56.41 MB | 56.40 MB | 0.93 → 1.35 s | 1.02 → 1.08 GB |

- What comes of it is sharing: of the 10gb corpus, `gimp.tar` and its files −11.3 MB (the tar
  archive 11.87 → 6.93 MB), and the rest of the −35.8 MB from what came with it in aqr-compress
  (slices of 1 KiB shared, files of 4 KiB held; JPEG files of the same picture, baseline and
  progressive, −15.2 MB). The 4660 ar archives come to +1.0 MB: their members are 1 KB
  objects on average, which compress about as well with their headers, and coding each
  archive's formula against it raw takes 18% more time on mingw.
- Every ar and tar archive of the 10gb corpus is rebuilt (4656 ar, 494,950 members; 8 tar, 6810
  members; 4 ar archives of no member are not taken), the module at 6.6 GB/s to encode and
  28 GB/s to decode (`aqr-bundle-check`). The corpus has no cpio archive.
- A bottle gains little: its tar archive is mostly Mach-O dylibs, whose code aqr-exe takes, but
  that is a few MB of the data; the formula of a bottle of 88 MB (icu4c) archives larger than
  the tar archive raw. The bottles take more memory, as the members' contents are copied into
  their streams by kind (on top of the tar archive inflated).
- With aqr-zip's `zip-recons`, the contents of large entries are slices of their own: a tar
  archive in a ZIP archive (silesia's `mozilla`) is taken apart, and found again where the same
  file is archived along with it.

## Tests

`cargo test --release -p aqr-bundle`; `aqr-bundle-check [-v] FILE_OR_DIR...` (the archives
found, whether each is rebuilt, zstd -3 of the archives against the module's inputs, speeds).
`scripts/gen-fixtures.sh` makes `tests/data` with the tools of this machine (bsdtar, bsdcpio,
Python's tarfile, the ar of macOS, llvm-ar and mingw).
