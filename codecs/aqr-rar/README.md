# aqr-rar

RAR archives (RAR5, and RAR 1.5–4.x with RAR 2.9's algorithm, that of RAR 3.x and 4.x) taken
apart into the contents of their files: each compressed stream (a solid group's files, or a
file alone) is split into the data it decodes to and a record of how its compressor wrote it,
and rebuilt bit for bit by the module `rar5-recons` or `rar3-recons`; stored files are their
contents. The rest of the archive (the headers, the files not taken) is kept as it is. What it is
for is the files: each file's contents are a content of the archive, recompressed by their
formats and stored once where the same files are archived along with it.

```sh
cargo run --release -p aqr-rar --example aqr-rar-check -- [-v] FILE_OR_DIR...   # streams decoded, encoded again, split and joined
```

```rust
use aqr_rar::{formula, recons3, recons5};
// None where it is no RAR archive, or nothing is rebuilt
let f = formula::encode(&rar, threads).unwrap();
// a stream alone (a solid group's files' packed data and sizes): its data and record
let sp = recons5::split(&parts, &sizes, v1, dict)?;
assert!(recons5::join(&sp.data, &sp.record)? == parts.concat());
```

`FORMAT.md` specifies the records and the modules, and how they work. No RAR code was read for aqr-rar: the
streams' formats are described in `docs/rar5-decoding.md` and `docs/rar3-decoding.md`, written
in a clean room (one agent read libarchive's BSD-licensed reader and RARLAB's technotes and
wrote those notes without writing code; the code was written from the notes alone, by others
who never saw libarchive). RARLAB's `rar` 7.23 and 6.24 and libarchive's `bsdtar` were the
black boxes. unrar's source, whose licence forbids using it towards a RAR compressor, was
read by no one. The PPMd model is ported from lzma-sdk's public-domain Ppmd7 (Shkarin's PPMd
var.H), with RAR's range coder.

## What is taken

Found anywhere in data (by its signature and first header: self-extracting programs' archives
too). Not taken, kept as they are: files split across volumes, encrypted files and headers, RAR
1.5 and 2.0's algorithms, filters other than the standard ones (RAR's VM programs in general;
Itanium's), streams that do not decode or write again as they were (older encoders'
run-length coding of tables, as one of libarchive's test archives has it).

## Results

Measured on 2026-11-01 on an M4.

`aqr-rar-check`, one thread:

| set | streams | packed | data | record | split, join |
|---|---|---|---|---|---|
| the 10gb corpus's RAR archives (83, RAR 3.x by older WinRARs; mostly PPMd) | 105 | 18.52 MB | 52.41 MB | 0.72% of the mattmahoney.net ones (49,846 B of 6.95 MB) | 30.7 s, 26.5 s (all) |
| rar 7.23 and 6.24's RAR5 fixtures (`docs/rar5-decoding.md` §4.1) | 104 | 27.16 MB | 81.21 MB | 847,960 B (3.12%) | 8.7 s, 7.8 s |
| rar 6.24's RAR 3.x fixtures (`docs/rar3-decoding.md` §9.1) | 41 | 10.35 MB | 25.41 MB | 230,808 B (2.23%) | 3.5 s, 3.0 s |

`aqr` on the 10gb corpus's 83 RAR archives (22.25 MB, 13.44 MB stored: many are the same), all
in one archive, all threads (before: `aqr` without aqr-rar, which stores them):

| level | before | after | compressing | testing |
|---|---|---|---|---|
| mid | 13,440,126 B | 13,440,298 B (none taken) | 0.2 → 5.2 s, 0.32 GB peak RSS | 0.2 s |
| 9 | 13,440,264 B | 13,251,570 B (−1.40%) | 0.2 → 5.9 s, 1.71 GB | 0.4 s |

Streams with PPMd blocks are taken from level 9 only (aqr-compress's `ppmd_on`): most of the
corpus is high-order PPMd, whose data aqr's coders compress no better, and rebuilding PPMd is
slow. At level 9 three archives are taken (`paq8hp5l.rar` 204 → 119 KB). A small RAR5 archive of
rar 7.23's default `-m3` (516 KB) is not taken either: its contents archive smaller than the
record (3%) and the module's block (23 KB) cost.

## Tests

- `tests/recons.rs`: the fixtures of `tests/data` (generated data archived by rar 7.23 and 6.24:
  solid groups, E8, delta and audio filters, PPMd) split and joined, their records damaged;
  streams cut short refused.
- `tests/formula.rs`: the fixtures rebuilt (every compressed stream by its module, stored files
  their contents); archives cut short or followed by data; the budget.
- `tests/ppmd.rs`: PPMd round trips (orders 2 to 64, models restarting), models continued
  across blocks; rar 6.24's PPMd streams decoded and encoded again (where `third_party/rar-dl`
  is there).
- aqr-decompress `tests/wasm.rs` (`rar_same_as_wasm`): natively and as Wasm alike, damaged
  records alike.

## Left

- **Volumes.** A stream over several archives is not taken.
- **Older WinRARs' code lengths.** Not Huffman's lengths as RAR 6.24 builds them: 181 of 614 main
  tables of the 10gb corpus, 8.6% of its record.
- **RAR5's parse.** RAR writes literals where matches are, as a parser weighing costs does: most
  of a record of 3%.
- **Speed.** PPMd is rebuilt twice to split and once to join (the port retires 1.55x the
  instructions of lzma-sdk's C), and streams that do not pay are split all the same.
- **The modules' sizes.** Blocks of 23 KB and 38 KB in every archive, which small archives do not
  pay back.
