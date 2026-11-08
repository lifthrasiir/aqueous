# 💧 Aqueous

Aqueous is an archive format and an archiver, `aqr`, built around lossless recompression.
Most files people keep are already compressed, by encoders far weaker than what a modern
machine can afford: JPEG's and MP3's Huffman codes, deflate in PNG, ZIP, PDF and gzip, LZ4 and
Snappy in browser profiles and databases. Aqueous undoes that compression, codes the contents
with stronger models, and rebuilds the original files bit for bit on extraction.

- **Recompressors** for a long and growing list of formats, each in its own crate under
  `codecs/` with a `FORMAT.md` describing what it stores. To name some: images (PNG, JPEG, GIF,
  PSD, Clip Studio Paint, BMP and PNM, BCn textures in DDS), audio and video (MP3, FLAC, Ogg
  Vorbis, WAV and AIFF, FMOD sound banks, ProRes, Matroska and WebM), documents and data (PDF,
  SQLite, HDF5, FITS, CSV and JSON Lines, plain text, FASTA, Wavefront OBJ, gettext catalogs),
  archives and images of discs (ZIP, 7z, RAR, tar, ar and cpio, ISO 9660 and UDF, CAB and CHM,
  LHA, ALZ and EGG, Electron's asar, git packs), the compressors found in and around them
  (deflate in gzip and zlib, LZMA and xz, bzip2, LZ4, Snappy, LZO, LZFSE, LZX, PPMd), and
  executables (PE, Mach-O, .NET, WebAssembly).
- **Self-describing archives.** A file's content is a *formula*, a small stack-machine program
  over stored blocks, which applies WebAssembly modules to undo what the compressor did. A
  decompressor needs no knowledge of the formats inside; known modules run natively, unknown
  ones in a sandbox.
- **Random access.** A file is extracted by reading little more than its own blocks, which the
  formula indexes. Archives can be extended by appending, and overlay archives describe a tree
  as changes to another.

The format is specified in [`spec/index.bs`](spec/index.bs) (an Editor's Draft, not yet
fixed). Aqueous is dedicated to the public domain, or under MIT-0 where that is not possible
([`LICENSE.txt`](LICENSE.txt)); see [`CREDITS.md`](CREDITS.md) for the projects its ideas
come from.

## Sneak peek

Matt Mahoney's 10 GB benchmark (79,431 files, 10.00 GB), on an Apple M4 (10 cores): `aqr` on
2026-11-08 at commit `f6f22f7`, the others on 2026-10-27:

| method | size | compression | decompression |
|---|---|---|---|
| `tar \| zstd -3 -T10` | 4.68 GB | 33 s | |
| `tar \| zstd -3 -T10 --long=31` | 4.17 GB | 36 s | |
| zpaq 7.15 | 3.70 GB | (as distributed) | |
| `aqr -9` | 2.362 GB | 359 s (28 MB/s, 5.0 GB peak) | 38.6 s (259 MB/s, 2.6 GB peak) |
| **`aqr`** (level `mid`) | **2.481 GB** | 167 s (60 MB/s, 2.5 GB peak) | 29.1 s (344 MB/s, 2.5 GB peak) |
| `aqr -2` (level `low`) | 3.030 GB | 93.5 s (107 MB/s, 3.0 GB peak) | 12.1 s (824 MB/s, 2.0 GB peak) |
| `aqr -1` (zstd alone) | 3.881 GB | 19.4 s\* (517 MB/s, 3.8 GB peak) | 4.7 s (2.12 GB/s, 1.6 GB peak) |
| `aqr -0` (uncompressed) | 8.980 GB | 15.5 s\* (645 MB/s, 1.8 GB peak) | 3.3 s\* (2.99 GB/s, 1.8 GB peak) |

Compression includes the round-trip check of every file. (\* I/O bound: by reading the input
(the archive, in decompression), not by compressing or decompressing.) Some formats alone, as
`archive / input` against `zstd -3` of the same files:

| format | aqr | zstd -3 |
|---|---|---|
| JPEG | 0.778 ~ 0.794 | 0.913 |
| PNG | 0.802 ~ 0.855 | 0.997 |
| MP3 | 0.852 ~ 0.915 | 0.968 |
| PE executables | 0.295 ~ 0.303 | 0.381 |
| SQLite | 0.210 ~ 0.219 | 0.366 |
| text (enwik9) | 0.233 ~ 0.235 | 0.312 |
| FASTA (hg19) | 0.209 | 0.309 |

Archives include their decoder modules. By format, *x ~ y*: *y* with each file an archive of
its own, its modules and their tables in it; *x* without them, as in an archive of many files
that share them. How they were measured (2026-11-01), and the formats taken or not yet taken,
are in [`docs/file-formats.md`](docs/file-formats.md).

## Building

```sh
cargo build -r -p aqr
target/release/aqr -c -o out.aqr some/directory   # compress
target/release/aqr -K out.aqr                     # test
target/release/aqr -t out.aqr                     # list (-v: blocks, formulas, metadata)
target/release/aqr -t out.aqr a/                  # list what is under a path
target/release/aqr -I out.aqr                     # all it records: blocks, segments
target/release/aqr -I out.aqr a/b.txt             # a path in each segment, its formula in full
target/release/aqr -d out.aqr                     # extract
target/release/aqr -d out.aqr a/b.txt --rename b.txt   # extract a path alone (to b.txt)
```

On Windows, `aqr-win` (`cargo build -r -p aqr-win`, beside `aqr.exe`; `aqr-win --register`)
adds Explorer's verbs and a window of how far the work is: [crates/aqr-win](crates/aqr-win/README.md).
On macOS, `Aqueous.app` (`crates/aqr-mac/bundle.sh`, into `target/dist`) adds Finder's services
and opens `.aqr` files, its progress shown on the files in Finder: [crates/aqr-mac](crates/aqr-mac/README.md).

For binaries to distribute, build with `cargo build --profile dist -p aqr` (into `target/dist`):
fat LTO, a few minutes for 0.3–0.7% fewer instructions than `-r`.

The workspace's tests are slow in debug builds; run them with `cargo rt` (`cargo test --profile
release-test`: optimised and incremental). The tests of aqr-flate, aqr-png and aqr-pdf compress
with [flate-zoo](https://crates.io/crates/flate-zoo)'s encoders, which want a C/C++ compiler; with
MSVC, the backends that do not work with it (zlib-ng, zlib-chromium, Go's and a few others) are
left out.

Rust's `x86_64-pc-windows-msvc` targets Windows 10 on; for Windows 7 and 8, build with
`cargo +nightly win7 -p aqr` (the tier-3 `x86_64-win7-windows-msvc`, std built with it and the C
runtime linked statically) into `target/x86_64-win7-windows-msvc/dist`.

## Disclaimer

This project makes heavy use of LLM-based coding agents. A human set the ideas and the goals;
AI did the implementation and the experiments.
