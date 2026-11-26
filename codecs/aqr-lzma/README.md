# aqr-lzma

LZMA streams (LZMA1: `.lzma` files, 7z folders) and LZMA2 streams (`.xz` files, 7z folders)
split into their data and a small record of the parse, and rebuilt bit for bit. Meant for archives that hold such streams whose data is
also elsewhere (a `.tar.lzma` of files installed beside it, a 7z of a tree), so that the data
is stored once and the stream costs only its record. No encoder is reproduced: the record
codes the packets given the data, so any encoder's stream has one, and those of encoders
with a binary-tree match finder and an optimal parser (XZ Utils, 7-Zip) are small. The
`FORMAT.md` specifies the record, the modules `lzma-recons` and `lzma2-recons` and `lzma-decode`, and how
they work.

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

## What is taken

- `.lzma` files and their streams (LZMA1), `.xz` files' LZMA2 blocks (the filters before LZMA2, aqr-bcj's,
  from the data before them), and 7z folders (aqr-7z). Streams with or without an end marker; any
  encoder's, the record small for those with a binary-tree match finder and an optimal parser
  (XZ Utils, 7-Zip).
- aqr-compress takes them from level 4 only (up to level 3 they are kept as they are: rebuilding
  LZMA is slow), in a batch with the files the stream holds (a tar's members, or the whole), so that
  their data is stored once.
- Not taken: a stream alone (nothing gained: the record costs 10-18% of it); an encoder's own
  parse is not reproduced.

## Results

Apple M4, 2026-11-02. Speeds are of the data, per thread (4 threads for the `.tar.lzma`, 2 for the
`.xz`, 1 for the 7z folder).

| streams | streams MB | data MB | records MB | of the streams | split | rebuild |
|---|---|---|---|---|---|---|
| the 123 `.tar.lzma` of the 10gb benchmark (XZ Utils `-6`, 8 MiB dictionary) | 73.08 | 428.6 | 9.84 | 13.5% | 39 MB/s | 34 MB/s |
| folder 0 of its `mingw480/*.7z` (7-Zip, 64 MiB dictionary) | 23.43 | 249.4 | 4.31 | 18.4% | 56 MB/s | 50 MB/s |
| its 6 `.tar.xz` (LZMA2) | 0.465 | 1.331 | 0.047 | 10.1% | 36 MB/s | 27 MB/s |
| a tar of its `mingw480/libexec` (58.0 MB), `xz -6 -T1` | 19.29 | 58.0 | 3.07 | 15.9% | 30 MB/s | 23 MB/s |
| the same, `-T4` (blocks of 24 MiB) | 19.49 | 58.0 | 3.03 | 15.5% | 31 MB/s | 24 MB/s |

- The members of those `.tar.lzma` (421 MB) and the data of that 7z folder are files of the
  benchmark, so with the data shared the streams cost about 14 MB instead of 96.5 MB.
- Modules' memory by dictionary (the plan, for data over it): 4 KiB 12.8 MiB, 1 MiB 25.8 MiB,
  4 MiB 65 MiB, 8 MiB (XZ Utils `-6`) 118 MiB, 16 MiB 223 MiB, 32 MiB 433 MiB, 64 MiB (7-Zip's)
  853 MiB. aqr-compress bounds an application at 256 MiB, so 7-Zip's 64 MiB is coded with a match
  finder reaching 8 MiB (plan 186 MiB; records of the two folders 6.06 -> 7.60 MB): the 7z of
  `mingw480` (its tree beside it) archives to 50.10 MB, where the stream stored whole gave 78.89 MB.
  A tree of 367 MB beside its `xz -6` (100.4 MB): 187.2 -> 96.9 MB.
- A stream over 64 MiB of data is coded in parts that split and rebuild side by side (the 7z's
  374.9 MB, 10 threads: split 10.8 s, rebuild 12.1 s; fresh contexts cost 1.6 KB on the 249.4 MB
  folder). Natively the match finders of a process take at most 1 GiB of chains at once.

## Tests

```sh
cargo rt                           # fixtures of XZ Utils, streams without end marker, damaged records
python3 -I scripts/gen-fixtures.py # rewrites tests/data (needs xz)
```

The tests of damaged and random records catch an overflow only in a debug build
(`cargo test`, about a minute), where it panics; in release it gives a record that fails.

## Left

- **Small `.tar.lzma` files.** 87 of the 10gb's (12.2 MB) stay as they are: most are small (the tar under
  256 KiB), so `together` does not see them; the large ones (`vim` 3.1 MB, `w32api` 1.1 MB) probably
  hold files found nowhere else (not rechecked).
- **7z beside its tree.** The 7z of `mingw480` costs 8.0 MB beside the tree, 6.51 MB of it records;
  a plain tar of 8,238 files (272.6 MB) costs 17.0 MB beside the tree (why it shares less is not seen).
- **Serial rebuild of a 7z folder.** The two folders (250 MB, 124 MB) are one stream each, rebuilt in
  turn by one match finder in a leaf (the longest path of unpacking the 10gb: 8.5 s of the 7z's 17 s).
  Two would save about 4 s for 0.8 GB more; admitting long leaves first, or running a leaf's
  applications side by side, would win back the late start.
- **Files not taken are still split.** The split of a stream whose members are not taken is thrown
  away after (19.8 MB's worth on the 10gb).
- **Far matches.** A distance beyond the finder's reach is coded as not found (records +25% for a
  64 MiB dictionary against 8 MiB reach); a long-distance index was measured and not kept.
