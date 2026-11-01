# aqr-unpack

Archives of other formats than Aqueous read for extraction, so that `aqr -d` extracts them as
it does its own (`crates/aqr/src/extract.rs`): the archive is read into the same tree of
entries, with the metadata it keeps made the tree's, and the contents of its files are given as
they are decoded, on the pool's threads. The format is told by the name (`by_name`), and the
output is the name without it.

| Names | Read | Decoded |
|---|---|---|
| `.zip` `.jar` `.apk` `.xpi` `.cbz` `.whl` | the central directory | stored, DEFLATE, bzip2, LZMA, Zstandard, xz; by entry |
| `.7z` `.cb7` | the header (an encoded one decoded first) | folders of LZMA, LZMA2, DEFLATE, bzip2, stored, BCJ/ARM/…/Delta, BCJ2; by folder, an LZMA2 stream by its segments |
| `.tar`, `.tar.gz` `.tgz`, `.tar.bz2` `.tbz2`, `.tar.xz` `.txz`, `.tar.lzma`, `.tar.zst` | as it is decoded (v7, ustar, GNU, pax) | gzip (members), bzip2 (a wave of blocks at a time), xz (blocks, and an LZMA2 stream's segments), LZMA, Zstandard |
| `.gz` `.bz2` `.xz` `.lzma` `.zst` | | a single file, written as it is decoded |

```rust
let opened = aqr_unpack::Opened::open(format, &archive)?;  // the listing read (ZIP, 7z)
let done = opened.unpack(threads, &sink)?;                  // sink: entries told, then contents
// done.tree: the whole tree; done.skipped: entries left out (told to sink.note)
```

## What is kept

- Times: ZIP's NTFS field, else Info-ZIP's extended timestamp, else the MS-DOS time as local
  time; 7z's modification, creation and access times; tar's modification time (pax's of
  nanoseconds), GNU's and pax's access and status change times, libarchive's creation time.
- Permissions and kinds: the Unix mode of ZIP (Unix hosts) and of 7z (`0x8000` in the
  attributes), tar's; Windows' attributes of ZIP (MS-DOS hosts) and 7z; symbolic links of all
  three; tar's hard links (as link groups), FIFOs and devices (made with `-p` alone).
- Owners: tar's ids and names, Info-ZIP's Unix ids. Extended attributes: pax's
  `SCHILY.xattr.` and `LIBARCHIVE.xattr.`.
- Names: ZIP's UTF-8 where flagged or given by Info-ZIP's Unicode path field, else UTF-8 where
  they are, else of the system's code page (on Windows; as they are elsewhere). `\` separates
  names of ZIP's MS-DOS hosts and of 7z.

Left out and told (the run then fails, once the rest is extracted): paths with `..` (a `/` or
a drive at the start is dropped), encrypted entries, methods and coders not known (Deflate64,
PPMd, AES), sparse files and multi-volume parts of tar. A path given again takes the place of
the one before (as tar has it); a file and a directory of one name keep the first.

## How it does

Measured 2026-11-01 on Windows 11 (16 threads, NVMe SSD), extracting a Python 3.14 install
(318 MB, 10,694 files) archived by 7-Zip 24 (ZIP and 7z at `-mx=5`), bsdtar, gzip -6, xz -6
-T0, zstd -3 and bzip2 -9; against 7-Zip (`7z x`) and bsdtar (`-xf`), best of two:

| Archive | aqr | 7-Zip / bsdtar | `aqr -t` |
|---|---|---|---|
| ZIP, 92.4 MB | 0.85 s | 2.71 s | 0.11 s |
| 7z, 40.1 MB (LZMA2) | 1.70 s | 2.30 s | 0.90 s |
| tar | 0.96 s | 1.58 s | 0.39 s |
| tar.gz, 81.0 MB | 1.18 s | 1.91 s | 0.64 s |
| tar.xz, 40.8 MB | 1.21 s | 2.44 s | |
| tar.zst, 53.6 MB | 1.00 s | 1.47 s | |
| tar.bz2, 54.0 MB | 2.71 s | 5.65 s | |

Each program ran under a name Windows Defender's real-time scanning leaves out on that machine:
scanned, writing these files takes 3–4 s more whatever writes them (a file's scan on its
close), which hides the rest (aqr 4.1 s, 7-Zip 2.8 s on the ZIP archive then).

## Left

- Formats of whose code there is more in the repository: ALZ, EGG, LHA, CAB, CHM, ISO, WIM.
- RAR; PPMd (7z, ZIP); Deflate64; AES and ZipCrypto (no passwords asked for yet).
- A 7z folder of LZMA (not LZMA2), a gzip member and a single-block xz decode on one thread;
  a 7z folder is held whole until its files are written.
- Names of ZIP and tar archives neither UTF-8 nor flagged, on macOS (raw bytes, which APFS does
  not take).
