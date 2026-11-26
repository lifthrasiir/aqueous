# aqr-iso

ISO 9660 and UDF images (CD, DVD and BD images; the UDF bridge discs of Windows) read into their
files: a file system of aqr-disk (`FS`), which takes the image apart into the contents of the
files and the rest ([../aqr-disk/FORMAT.md](../aqr-disk/FORMAT.md)), all of an image or a
partition of it. There is no module: each file's contents become a content of the archive,
recompressed by their formats (a PE file by aqr-exe, a ZIP archive by aqr-zip) and stored once
where the same files are archived along with it. `FORMAT.md` tells what is read and how.

```sh
cargo run --release -p aqr-iso --example aqr-iso-check -- [-v] [-z] IMAGE...   # files found, rebuilt, zstd -3 of the rest
powershell -File codecs/aqr-iso/scripts/gen-fixtures.ps1                      # tests/data from Windows' IMAPI2
```

## What is taken

- **UDF** (ECMA-167 as OSTA UDF profiles it), 2048-byte blocks.
- **ISO 9660**: the primary volume descriptor's tree and the supplementary ones' (Joliet).

Where several trees give the same extent, UDF's name is taken, then Joliet's, then ISO 9660's; the
names give the contents their kinds.

Not taken: virtual, sparable and metadata partitions (UDF 1.50 on; a UDF 2.50 image, as Blu-ray
discs are, gives no file), ISO 9660's extended attribute records and interleaved files, raw
sectors (2352-byte images of CDs: `docs/other-projects.md` §7).

## Results

On an AMD Ryzen 7 9700X through `cargo aqr` (all threads, level mid, 2026-10-30), the Windows Server
2008 R2 evaluation DVD (3,166,840,832 bytes, 996 files in UDF):

| | before | after |
|---|---|---|
| archived, time, peak working set | 2,777.6 MB, 19.5 s, 2.04 GB | 2,776.9 MB, 17.9 s, 1.82 GB |

It gains little by itself, as 89% of the DVD is `sources/install.wim` and `boot.wim`: what the
image gives is these as files of their own, which aqr-wim takes apart (the DVD then archives to
2,483.1 MB). The rest is 6.9 MB, 318 KB at zstd -3; reading the trees takes a millisecond.

## Tests

`cargo rt -p aqr-iso`:

- The fixtures of IMAPI2 (ISO 9660 alone, with Joliet, with Joliet and UDF 1.02, UDF 2.01 and
  2.50), rebuilt through aqr-disk.
- Images cut short and changed bytes.

How extents are laid out is aqr-disk's to test.

## Left

- **UDF 1.50 on.** Virtual, sparable and metadata partitions are not read, so a Blu-ray's UDF 2.50
  image gives no file and archives as it is.
- **Raw CD sectors.** 2352-byte images (sync, headers, ECC) are not read.
- **ISO 9660 extras.** Extended attribute records and interleaved files are not read.
