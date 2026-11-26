# aqr-obj

Lossless Wavefront OBJ recompression: an OBJ file is stored smaller, and restored bit for
bit, at about 120 MB/s compressing and 320 MB/s decompressing on one thread. `FORMAT.md`
specifies the module `obj-recons` and how it works.

```sh
cargo build --release --example aqr-obj   # target/release/examples/aqr-obj
aqr-obj bench [-n ITERS] [-B BLOCK] [-x] [-t N] [-c|-d] [-v] FILE...   # archive size, round trip check, throughput
aqr-obj stats FILE...                                  # line templates and lines kept as text
aqr-obj fuzz [-n N] [-s SEED] FILE...                  # mutated files
```

```rust
use aqr_obj::formula::{self, MODULE};
// None for an empty file, or where no block could be coded.
let f = formula::encode(&obj, 1 /* threads */).unwrap();
// `f` rebuilds `obj` when evaluated with `formula::decode` as the module `MODULE`.
```

## What is taken

- A file is taken as OBJ by its text (`formula::starts`: its first 64 KiB are text of
  statements, three vertices or more), not by the name `.obj`, which object files (COFF, OMF)
  have too. Any input is accepted: lines that do not parse are kept as text, and a block that
  fails verification is stored whole. Module: `obj-recons` (the crate without the default
  feature `encode` is the module's code alone, `module`, which aqr-modules builds into Wasm).
- Numbers as the common writers print them (fixed, `%g`, `%e`, `%.17g`, Python's `repr`,
  JavaScript's `toString`, Rust's `Display`, Blender's), through `aqr-num`.
- A file of more than 64 MiB is cut into portions coded side by side on the threads.
- Not taken: `.mtl` files, and OBJ inside archives, are left to the general compressor.

## Results

Apple M4, one thread, `aqr-obj bench -t 1`, 2026-10-24 (`da98d11`; ratios are tentative, the
decoder modules not being in the archives then). Memory is the peak RSS of the process (input,
archive and output included): 24 MB + 3.1x the file, one process per file. The files are taken
as for every crate (`docs/file-formats.md`), files over 300 MB left out.

| set | files | MB | ratio | zstd -3 | compress | decompress |
|---|---|---|---|---|---|---|
| mcguire | 4 | 225.1 | 0.102 | 0.234 | 142.9 MB/s | 352.6 MB/s |
| writers | 8 | 142.7 | 0.081 | 0.240 | 101.2 MB/s | 298.4 MB/s |
| c3d | 6 | 30.5 | 0.132 | 0.309 | 101.3 MB/s | 274.1 MB/s |
| threejs | 4 | 4.3 | 0.119 | 0.262 | 98.2 MB/s | 280.6 MB/s |
| all | 22 | 403 | **0.097** | 0.242 | **121 MB/s** | **324 MB/s** |

The files of 100 MB or more of McGuire's archive, `aqr -c` one archive a file (2026-11-10,
Ryzen 7 9700X, 16 threads), each extracted byte for byte (1.6 s the largest):

| file | MB | archive | compress | heap / resident peak |
|---|---|---|---|---|
| San_Miguel-san-miguel | 1,143.0 | 73.37 MB (6.4%) | 3.67 s | 880 MB / 1.79 GB |
| powerplant | 817.9 | 20.02 MB (2.4%) | 2.51 s | 523 MB / 1.35 GB |
| San_Miguel-san-miguel-low-poly | 628.0 | 49.19 MB (7.8%) | 2.16 s | 590 MB / 1.14 GB |
| rungholt-rungholt | 275.7 | 8.27 MB (3.0%) | 1.46 s | 405 MB / 674 MB |
| hairball | 236.1 | 14.89 MB (6.3%) | 1.18 s | 317 MB / 527 MB |
| salle_de_bain | 123.9 | 6.34 MB (5.1%) | 0.92 s | 217 MB / 293 MB |
| erato | 107.6 | 10.15 MB | 0.67 s | 175 MB / 259 MB |

Decoding slows with many portions (aqr-formula evaluates a Concat's parts in turn); cutting
into portions costs 0.4-0.7% against the whole file's formula.

## Corpus

`corpus/` is not in the repository; the scripts rebuild it.

- `scripts/fetch-corpus.sh`: the [McGuire Computer Graphics Archive](https://casual-effects.com/data/)
  scenes and scans (`mcguire/`, 21 files, 3.9 GB: San Miguel 1.1 GB, Power Plant 818 MB,
  Rungholt, Hairball, Sponza, Buddha, Dragon, interiors; exporters such as 3ds Max,
  ZBrush, Mineways), [common-3d-test-models](https://github.com/alecjacobson/common-3d-test-models)
  (`c3d/`, 17 scans, mostly `%.17g` from libigl), and the three.js example models
  (`threejs/`, 7 files, one with vertex colors).
- `scripts/gen-writers.py`, `gen-writers.mjs`, `gen-blender.py`: three of those meshes
  (igea, xyzrgb_dragon, crytek_sponza; conference for Blender) printed the ways common
  writers print numbers (`writers/`, 27 files): Python `repr` of doubles and of widened
  floats, numpy's `%.18e`, `%.17g`, `%.9g` and `%e` of floats, Rust's `Display` of `f32`,
  JavaScript's `toString` of `Float32Array` values (three.js' exporter), and Blender 5.2's
  exporter.
- `scripts/baseline.sh`: the other compressors, cached per tool in `cache/`;
  `scripts/compare.py` puts them side by side. `examples/` hold the experiments behind the
  choices in `FORMAT.md`'s design notes.

## Tests

```sh
cargo rt -p aqr-obj
../../target/release/examples/aqr-obj fuzz -n 200 $(find corpus -name '*.obj' -size -5M)
```

- `tests/roundtrip.rs`: edge cases (no line end, CRLF, tabs, odd numbers, relative indices,
  long lines, more templates than the table holds, binary junk), every writer style mixed in
  one file, a mesh using every corner symbol, blocks stored whole in between; through the
  formula, under several block sizes and threads.
- `tests/formula.rs`: the formula's structure, and damaged inputs (errors or other output,
  never panics).
- `tests/edge.rs`: huge indices and numbers, mixed line ends, portions cut inside them, and
  damaged streams.
- `aqr-obj fuzz`: mutated files (bytes, tokens, line copies, line ends, digits) must
  round-trip through the formula with verification off, small and large blocks, some stored
  whole, whatever the threads.

## Left

- **`%.17g` speed.** The mode check prints each number from exact digits (twice where the
  15-digit guess misses, most `%.17g` numbers).
- **Connectivity.** Faces in random order (decimated scans) cost about the logarithm of the
  vertex count per index; coding a vertex against the faces around its neighbours needs more
  state than the bounded tables keep.
- **`%g` scale changes.** 2.7% of the output; could be predicted from the predictor's
  magnitude.
