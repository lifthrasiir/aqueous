# aqr-iso

ISO 9660 and UDF images (CD, DVD and BD images; the UDF bridge discs of Windows) taken apart
into the contents of their files and the rest (`FORMAT.md`). The image is the concatenation of
the two, so there is no module: what it is for is that each file's contents become a content of
the archive, recompressed by their formats (a PE file by aqr-exe, a ZIP archive by aqr-zip) and
stored once where the same files are archived along with it.

```sh
cargo run --release -p aqr-iso --example aqr-iso-check -- [-v] [-z] IMAGE...   # files found, rebuilt, zstd -3 of the rest
powershell -File codecs/aqr-iso/scripts/gen-fixtures.ps1                      # tests/data from Windows' IMAPI2
```

## What it reads

- **UDF** (ECMA-167 as OSTA UDF profiles it): the anchor at sector 256 (or the last sector), the
  main volume descriptor sequence (partitions, the logical volume of 2048-byte blocks), the file
  set descriptor, then the directories: file entries and extended file entries, their data in
  short or long allocation descriptors (chained by allocation extent descriptors) or inline.
- **ISO 9660**: the trees of the primary volume descriptor and of the supplementary ones (Joliet's
  UCS-2 names), files of several extents as one.

Where several trees give the same extent, UDF's name is taken, then Joliet's, then ISO 9660's
(8.3 names, versions dropped); the names give the contents their kinds (aqr-compress's
`kind_by_name`). An extent overlapping one before it is left in the rest, and so is everything
the trees do not point at. Each directory is read once, so loops do not hang it, and what is
past the end of an image cut short is not read.

Not read: virtual, sparable and metadata partitions (UDF 1.50 on; a UDF 2.50 image, as Blu-ray
discs are, gives no file), ISO 9660's extended attribute records and interleaved files, raw
sectors (2352-byte images of CDs, with their sync, headers and ECC: see `docs/other-projects.md`
§7 for that).

## How it does

Measured 2026-10-30 on an AMD Ryzen 7 9700X (8 cores, Windows; all threads, level mid) through
`cargo aqr`, against the same build without the detection of images:

| | before | after |
|---|---|---|
| Windows Server 2008 R2 evaluation DVD (3,166,840,832 bytes) | 2,777,614,718 B, 19.5 s, 2.04 GB | 2,776,905,890 B, 17.9 s, 1.82 GB |

(sizes, time, peak working set). The DVD has 996 files in UDF (798 extents, those of the
others shared), its ISO 9660 tree only a stub telling of UDF; the rest is 6.9 MB, 318 KB at
zstd -3, and reading the trees takes a millisecond. It gains little, as 2.81 GB of it is
`sources/install.wim` and 169 MB `boot.wim`, LZX-compressed: what the image gives is these as
files of their own, for a recompressor of WIM.

aqr-compress takes an image as an archive that only bundles files (as aqr-bundle's): its
formula is not compared against the image raw, and its files are made in parallel.

## Tests

`cargo rt -p aqr-iso`: the fixtures of IMAPI2 (ISO 9660 alone, with Joliet, with Joliet and UDF
1.02, UDF 2.01 and 2.50), images cut short and changed bytes, how extents are laid out.
