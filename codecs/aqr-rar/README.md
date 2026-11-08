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

`FORMAT.md` specifies the records and the modules. No RAR code was read for aqr-rar: the
streams' formats are described in `docs/rar5-decoding.md` and `docs/rar3-decoding.md`, written
in a clean room (one agent read libarchive's BSD-licensed reader and RARLAB's technotes and
wrote those notes without writing code; the code was written from the notes alone, by others
who never saw libarchive). RARLAB's `rar` 7.23 and 6.24 and libarchive's `bsdtar` were the
black boxes. unrar's source, whose licence forbids using it towards a RAR compressor, was
read by no one. The PPMd model is ported from lzma-sdk's public-domain Ppmd7 (Shkarin's PPMd
var.H), with RAR's range coder.

## How

A RAR5 stream is LZ77 (matches of 2 to 4,100 bytes within windows up to 1 TiB) under prefix
codes sent block by block, with filters (delta, x86 E8/E9, ARM) declared in the stream. RAR
2.9's stream adds PPMd blocks (PPMd var.H, matches and filter records through escapes) and
filters as RarVM programs, of which RAR only ever writes five standard ones (recognised by their
CRCs and run natively). `lz5` and `lz3` decode a stream into its data, its blocks as written and
its tokens or symbols, and write them again; a stream is taken where that gives it back.

The record (`recons5`, `recons3`) codes the tokens given the data, as aqr-lzo's and aqr-lha's
do: whether a match starts (against the longest match at the position and the next), then which
match: one of the four last distances, or one of the longest matches hash chains find, nearest
first, mostly the longest and whole; a match that is neither is its length and its rank among
the matches of that length. How a match is written (an explicit match, a repeated distance, the
last match again, a short match) is predicted by RAR's rule. Blocks cost next to nothing: RAR
cuts them where its buffer of 16,384 symbols fills (RAR5: a filter record counts 6 units and its
start's bytes), and its code lengths are Huffman's construction over the symbols each table
serves (the shallower subtree first among equal weights), which gives every table of RAR 6.24
and 7.23 seen. Filter records are predicted from what they do; the data the filters undo is the
contents filtered again. PPMd blocks need only their parameters (the escape byte predicted as
RAR picks it) and where their matches are; their symbols follow from the data.

## Status

Found anywhere in data (by its signature and first header: self-extracting programs' archives
too). Not taken, kept as they are: files split across volumes, encrypted files and headers, RAR 1.5 and 2.0's algorithms,
filters other than the standard ones (RAR's VM programs in general; Itanium's), streams that
do not decode or write again as they were (older encoders' run-length coding of tables, as one
of libarchive's test archives has it).

Left: volumes (a stream over several archives), older WinRARs' code
lengths (not Huffman's lengths as RAR 6.24 builds them: 181 of 614 main tables of the 10gb
corpus; 8.6% of its record), RAR5's parse (RAR writes literals where matches are, as a parser
weighing costs does: most of a record of 3%), speed (PPMd rebuilt twice to split, once to join;
the PPMd port retires 1.55× the instructions of lzma-sdk's C; streams that do not pay are split
all the same), the modules' sizes (blocks of 23 KB and 38 KB
in every archive, which small archives do not pay back).

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
slow. At level 9 each archive's formula is weighed against its bytes and three are taken
(`paq8hp5l.rar` 204 → 119 KB). At mid the time is one LZ stream of RAR archives (`all_HKCC.rar`,
4.8 MB) split and turned down. A RAR5 archive of rar 7.23's default `-m3` (wer.dll and a C
source, 516 KB) is not taken either: its contents archive to 485 KB at level 9, less than the
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
