# aqr-lzma

LZMA streams (LZMA1: `.lzma` files, 7z folders) and LZMA2 streams (`.xz` files, 7z folders)
split into their data and a small record of the parse, and rebuilt bit for bit. Meant for archives that hold such streams whose data is
also elsewhere (a `.tar.lzma` of files installed beside it, a 7z of a tree), so that the data
is stored once and the stream costs only its record. No encoder is reproduced: the record
codes the packets given the data, so any encoder's stream has one, and those of encoders
with a binary-tree match finder and an optimal parser (XZ Utils, 7-Zip) are small. The
record's format is in [FORMAT.md](FORMAT.md).

```sh
cargo build --release
aqr-lzma bench [-t THREADS] [-v] FILE...    # .lzma or .xz files, or raw LZMA streams as FILE@OFFSET+LENGTH:PROPS:DICT:SIZE
```

```rust
use aqr_lzma::formula;
// None where `file` is not a `.lzma` file, or its stream does not decode.
let f = formula::encode(&file, 1 /* threads */).unwrap();
// `f` rebuilds `file` when evaluated with `formula::decode` as the module `formula::MODULE`.

use aqr_lzma::recons;
// size: None where it ends with the end marker; a stream of more than 64 MiB of data in parts,
// coded and decoded side by side on up to `threads` threads (the same record whatever they are)
let sp = recons::split(&stream, props, dict, size, limit, threads)?;
assert_eq!(recons::join(&sp.data, &sp.record, threads).unwrap(), stream[..sp.used]);

use aqr_lzma::lzma2;
// a split for each segment (from a chunk resetting the dictionary), each rebuilt on its own
let segs = lzma2::split(&stream, dict, limit, threads)?;
let f = formula::encode_xz(&file, threads).unwrap(); // an .xz file: `formula::LZMA2` on its blocks
```

## Results

Measured on 2026-11-02 on an Apple M4 (4 performance and 6 efficiency cores), load about 2.
Speeds are of the data, per thread: with 4 threads (the performance cores) for the `.tar.lzma`,
2 for the `.xz`, 1 for the 7z folder (one stream); with 10, the per-thread figures halve and the
wall time stays. In parentheses, those of the match finder before its buckets (below), measured
just before on the same machine.

| streams | streams MB | data MB | records MB | of the streams | split | rebuild |
|---|---|---|---|---|---|---|
| the 123 `.tar.lzma` of the 10gb benchmark (XZ Utils `-6`, 8 MiB dictionary) | 73.08 | 428.6 | 9.84 | 13.5% | 39 (19) MB/s | 34 (17) MB/s |
| folder 0 of its `mingw480/*.7z` (7-Zip, 64 MiB dictionary) | 23.43 | 249.4 | 4.31 | 18.4% | 56 (25) MB/s | 50 (24) MB/s |
| its 6 `.tar.xz` (LZMA2; the records with the rest of the files) | 0.465 | 1.331 | 0.047 | 10.1% | 36 (27) MB/s | 27 (22) MB/s |
| a tar of its `mingw480/libexec` (58.0 MB), XZ Utils `-6 -T1` (one block) | 19.29 | 58.0 | 3.07 | 15.9% | 30 (12) MB/s | 23 (10) MB/s |
| the same, `-6 -T4` (blocks of 24 MiB) | 19.49 | 58.0 | 3.03 | 15.5% | 31 (13) MB/s | 24 (11) MB/s |

- Decoding and encoding the packets alone run at 75–300 MB/s. The rest is the match finder.
  Its walks along the chains were one cache miss after another: 70% of the time on the 7z
  folder, 1.2 µs a match. Now the chains of 4 and 16 bytes keep the newest 16 positions of each
  hash on one cache line (from 1 MiB of data; `mf::RECENT`), and inserting asks for those lines
  32 positions ahead (the data is all at hand). The candidates are the same, and so are the
  records. Splitting that folder now takes 1.4 s inserting, 1.1 s for the longest matches,
  0.85 s for the nearest ones and 0.7 s for the rest (4.4 s; 9.8 s before).
- The record is mostly the packet types (36%) and the lengths of matches (42%); distances are
  16%, as a match's distance is almost always the nearest one matching for its length. Deeper
  search lowers lengths and distances, slowly: 16 candidates per chain rather than 4 make the
  records about 6% smaller on the `.tar.lzma`.
- Every member of those `.tar.lzma` (421 MB) and the data of that 7z folder are files of the
  benchmark, so with the data shared the streams cost about 14 MB instead of 96.5 MB.
- Memory: natively the data, the stream and `12 × S` bytes of chains, `S` the dictionary size
  rounded up to a power of 2 (at most the data's), and 132.5 MiB of tables (12.5 MiB for less
  than 1 MiB of data, without the buckets). The modules never keep the buckets: with one
  position of each hash their tables are 12.5 MiB at any length, and the records are byte for
  byte the same (the `.tar.lzma` of the 10gb and the 7z folders above, checked), at the cost of
  a rebuild about twice as slow in the match finder (32 -> 19 MB/s on the 7 largest
  `.tar.lzma`, one thread, natively with one position; the known hash runs natively with 16
  in the encoder). That brought the `.tar.lzma` of 8 MiB dictionaries under the encoder's
  bound of 256 MiB per application (up to 354 MiB needed before), where they were stored
  whole.
- The modules are part modules (FORMAT.md, "The module as a part module"): the data goes
  through a window that holds the dictionary `R`, an eighth more and 66 KiB ahead (all of it
  where it is shorter; the match finder and the model look no farther), the record is read and
  the stream written in pieces, so that a module takes `12 × S` + 12.5 MiB + about `1.13 R` +
  0.1 MiB, and 1.1 MiB of initial memory, whatever the lengths (they were the data, the
  stream and the record held whole besides: 584 MiB for a 220 MB `.xz` of an 8 MiB dictionary).
  By the dictionary (the plan, for data over it; `lzma_plans_hold` checks it to the page,
  with 67 MiB of data too): 4 KiB 12.8 MiB, 1 MiB 25.8 MiB, 4 MiB 65 MiB, **8 MiB (XZ
  Utils `-6`) 118 MiB**, 16 MiB 223 MiB, 32 MiB 433 MiB, 64 MiB (7-Zip's) 853 MiB. A stream
  shorter than its dictionary holds less (a ring of `min(S, N)`, a window of `N`).
- The match finder's reach may be less than the dictionary (the record's `reach` byte; FORMAT.md,
  "The record"): the encoder takes the largest power of 2 `F` (at most the dictionary) whose
  plan fits its bound on an application (256 MiB by default; `plan::finder_for`), and the
  dictionary itself where that fits (the record is then what it was, apart from the byte). The
  rings follow `F`, the data window stays the dictionary's, so that what a far match leaves
  (the literal after it, a far repeat) reads the data and nothing extra is stored; a
  distance beyond `F` is coded as a distance not found, as ever. 7-Zip's 64 MiB: `F` = 8 MiB, a
  plan of 186 MiB (`F` = 16 MiB would be 282), the two folders' records 6.06 -> 7.60 MB; the
  7z of `mingw480` (its tree beside it) archives to 50.10 MB where the stream was given up for
  the bound, 78.89 MB (48.56 MB with a bound of 2 GiB). The 7 largest `.tar.lzma` (8 MiB
  dictionaries, which fit) have the same archive, a byte more for each record. A long-distance
  index for the matches beyond `F` was measured and not kept (docs/streaming-encoder-design.md).
  Measured on 2026-11-08 (M4, `-9`, the bound of 256 MiB): a tree of 367 MB beside its
  `xz -6` (8 MiB dictionary, one block of 369 MB of data, 100.4 MB) archives to 96.9 MB where
  it was 187.2 MB (the stream stored whole, 1059 MiB needed), the same as with no bound; the
  7 largest `.tar.lzma` of the 10gb, 41,740,374 -> 41,741,808 B (the module's block is 1.4 KB
  larger); the whole 10gb, 2,361,761,993 -> 2,361,767,581 B. The match finder's indexing by the
  window's base costs the split and the rebuild 4.7% of their instructions natively.
- A stream of more than 64 MiB of data is coded in parts of 64 MiB, each with contexts of its
  own, which split and rebuild side by side (FORMAT.md, "Parts"). Natively, the match finders
  of a process take at most 1 GiB of chains at once (one of a 64 MiB dictionary), or as much
  as `mf::allow_chains` lets in: aqr-compress lets in 2 GiB (two) as it compresses. On the 7z
  above (all its folders, 374.9 MB of data, 10 threads, two match finders at once): split
  15.5 -> 10.8 s, rebuild 15.8 -> 12.1 s, records 6,496,078 -> 6,498,275 bytes. Starting a
  part's match finder (the 64 MiB before it inserted) takes 0.23 s; fresh contexts cost 1.6 KB
  on the 249.4 MB folder.

## Tests

```sh
cargo rt                           # fixtures of XZ Utils, streams without end marker, damaged records
python3 -I scripts/gen-fixtures.py # rewrites tests/data (needs xz)
```

The tests of damaged and random records catch an overflow only in a debug build
(`cargo test`, about a minute), where it panics; in release it gives a record that fails.

## Status

The module `lzma-recons` (natively and as a Wasm module of aqr-modules, a part module) and
`.lzma` files in aqr-compress are done: in an archive of several files, a `.lzma` file is put in a batch with
the files its stream holds (the members of a tar archive, or the whole), so that their data is
stored once. So are the module `lzma2-recons` and `.xz` files (their blocks of LZMA2 alone),
which aqr-compress takes alike, anywhere in data too; blocks of filters before LZMA2 (aqr-bcj's:
the branch converters, Delta) from the data before them. 7z archives are aqr-7z's, on these
modules and aqr-bcj's. aqr-compress takes them (`.lzma`, `.xz` and 7z) from level 4 only
(`format_on`): up to level 3 they are kept as they are, as rebuilding LZMA is slow.
