# 💧 Aqueous

Aqueous is an archive format and an archiver, `aqr`, built around lossless recompression.
Most files people keep are already compressed, by encoders far weaker than what a modern
machine can afford: JPEG's and MP3's Huffman codes, deflate in PNG, ZIP, PDF and gzip, LZ4 and
Snappy in browser profiles and databases. Aqueous undoes that compression, codes the contents
with stronger models, and rebuilds the original files bit for bit on extraction.

- **Recompressors** for about twenty format families: images (PNG, JPEG, PSD, BMP and PNM,
  BCn textures in DDS), audio (MP3, FLAC, WAV, AIFF), documents and data (PDF, SQLite, text,
  FASTA, Wavefront OBJ), containers and general-purpose compression (gzip and zlib, ZIP, LZ4,
  LZO, LZMA, Snappy, tar, ar and cpio, git checkouts) and executables (PE and Mach-O). Each
  lives in its own crate under `codecs/`, with a `FORMAT.md` describing what it stores.
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
2026-10-31 at commit `921778d`, the others on 2026-10-27:

| method | size | compression | decompression |
|---|---|---|---|
| `tar \| zstd -3 -T10` | 4.68 GB | 33 s | |
| `tar \| zstd -3 -T10 --long=31` | 4.17 GB | 36 s | |
| zpaq 7.15 | 3.70 GB | | |
| **`aqr`** (level `mid`) | **2.552 GB** | 161 s (62 MB/s, 3.3 GB peak) | 38.8 s (258 MB/s, 2.6 GB peak) |

Compression includes the round-trip check of every file. Some formats alone, as
`archive / input` against `zstd -3` of the same files:

| format | aqr | zstd -3 |
|---|---|---|
| JPEG | 0.785 | 0.913 |
| PNG | 0.802 | 0.997 |
| MP3 | 0.853 ~ 0.890 | 0.971 |
| PE executables | 0.326 | 0.381 |
| SQLite | 0.227 | 0.366 |
| text (enwik9) | 0.253 | 0.312 |
| FASTA (hg19) | 0.218 | 0.309 |

The 10 GB archive includes its decoder modules. The figures by format are tentative: they were
measured before the modules were embedded in archives. How they were measured, and the
formats taken or not yet taken, are in
[`docs/file-formats.md`](docs/file-formats.md).

## Building

```sh
cargo build -r -p aqr
target/release/aqr -c -o out.aqr some/directory   # compress
target/release/aqr -t out.aqr                     # test
target/release/aqr -d out.aqr                     # extract
```

For binaries to distribute, build with `cargo build --profile dist -p aqr` (into `target/dist`):
fat LTO, a few minutes for 0.3–0.7% fewer instructions than `-r`.

The workspace's tests are slow in debug builds; run them with `cargo rt` (`cargo test --profile
release-test`: optimised and incremental).

Rust's `x86_64-pc-windows-msvc` targets Windows 10 on; for Windows 7 and 8, build with
`cargo +nightly win7 -p aqr` (the tier-3 `x86_64-win7-windows-msvc`, std built with it and the C
runtime linked statically) into `target/x86_64-win7-windows-msvc/dist`.

## Disclaimer

This project makes heavy use of LLM-based coding agents. A human set the ideas and the goals;
AI did the implementation and the experiments.
