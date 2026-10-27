# aqr-obj

Lossless Wavefront OBJ recompression: an OBJ file is stored smaller, and restored bit for
bit. Meant as a component of a general-purpose compressor, so speed comes first: the target
is 100 MB/s single-threaded on an Apple M4, both ways. A small rANS coder is the entropy
backend of the value streams; the archive (aqr-formula) compresses the other streams.

```sh
cargo build --release
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

Any input is accepted: lines that do not parse are kept as text, and a block that fails
verification is stored whole.

## Results

Measured on 2026-10-24 at `da98d11` on an Apple M4, one thread, with `aqr-obj bench -t 1`
(aqr-obj is not in aqr-compress yet). Ratios are tentative, since the decoder modules were not
in the archives then. Memory is the peak RSS of the process (input, archive and output
included) over the input size.

| set | files | MB | ratio | zstd -3 | compress | decompress |
|---|---|---|---|---|---|---|
| mcguire | 4 | 225.1 | 0.102 | 0.234 | 142.9 MB/s | 352.6 MB/s |
| writers | 8 | 142.7 | 0.081 | 0.240 | 101.2 MB/s | 298.4 MB/s |
| c3d | 6 | 30.5 | 0.132 | 0.309 | 101.3 MB/s | 274.1 MB/s |
| threejs | 4 | 4.3 | 0.119 | 0.262 | 98.2 MB/s | 280.6 MB/s |
| all | 22 | 403 | **0.097** | 0.242 | **121 MB/s** | **324 MB/s** |

The files are taken as for every crate (`docs/file-formats.md`), files over 300 MB left out.
Memory, with one process per file, is 24 MB + 3.1× the file (a linear fit).
`aqr-obj bench` reports the size of an archive of the formula (`formula::encode`,
then `aqr_formula::aqr::encode`) and the speed of making and decoding it. The other
compressors' sizes per file are cached by `scripts/baseline.sh`.

### Threads

`-t N` (`Options::threads`, the threads of `formula::decode`): the output is the same
whatever the number of threads, and one thread runs the code unchanged. Coded blocks carry
state over, so the line coding and decoding stay on one thread, in order; around them, a
pool parses the lines of the blocks ahead (number modes, render checks), verifies, and
codes the streams; decoding, it unpacks the streams of the blocks ahead and prints the
numbers of decoded blocks (the decoder emits values, `block::Deferred`). The line coding
(predictors, corner caches) and the line decoding are the limit. Going further needs
independent segments (state reset every so many blocks); not done.

### Memory

The heap beyond input and output does not grow with the file: blocks of 4 MB, rings of the
last 65,536 lines per kind, a table of 65,536 vertices, and templates up to 4 MB.

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
  `scripts/compare.py` puts them side by side.

## How

A file is cut into blocks of 4 MB at line ends. Each block becomes a set of streams, each
kept as is or rANS coded, whichever is smaller (the archive compresses them further);
state (previous values, templates, recent lines and faces) carries over from block to
block, but its size does not depend on the file's.

### Lines and templates (`src/block.rs`)

A `v`, `vt`, `vn`, `vp`, `f` or `l` line becomes a template and values. The template holds
everything but the values (separators, number formats, corner patterns, line end), so a
line costs its template number (one byte, nearly free once compressed). Any other line, or
one that does not parse back to itself, is kept as text.

### Numbers ([`aqr-num`](../aqr-num), `src/style.rs`)

A number is an integer at a scale (digits after the point), printed in a mode (fixed,
C's `%g` and `%e`, Python's `repr`, JavaScript's `toString`, Rust's `Display`), which may
print from a float: the value is then the shortest decimal that rounds to the `f32` or
`f64`. A `%.17g` number costs what its double holds, not 17 digits.

Shortest decimals come from Żmij; exact digits of a double (`%.17g`, `%.18e`) from 128-bit
integer arithmetic. Only IEEE basic operations are used, so decoding gives the same bytes
everywhere. The encoder picks a mode per kind of line from a sample of each block (by the
digits it would code and its scale changes), checks every number by printing it back, and
falls back to fixed or `%g` (or text) for numbers that are printed otherwise.

The number code is the `aqr-num` crate (its README specifies the modes); it knows
nothing of OBJ. `src/style.rs` samples the lines of a block per kind and detects unit
normals.

### Prediction (`src/block.rs`, `src/recent.rs`)

- Equal lines: a `v`, `vt` or `vn` line equal to one of the last 65,536 of its kind is
  coded as the distance back (triangle soups repeat each vertex several times).
- Line predictors: per `v`, `vt`, `vn` line, the predictor that leaves the fewest
  residual bits among the previous numbers, the lines up to 16 back, and the
  parallelograms `x[-1] + x[-k] - x[-k-1]`; the encoder stays with the last one unless
  another saves more than a few bits.
- Unit normals: when the normals of a block have length 1, the largest component is
  predicted from the other two, `±sqrt(1 - a² - b²)`.
- Residuals go to streams per kind and position, rANS coded per value (`src/ans.rs`),
  unless they are kept as is (repeats, which the archive's zstd sees).

### Faces (`src/corner.rs`)

Per attribute (v, vt, vn), a move-to-front cache of the 32 last indices; an index is coded
as its cache position, as "one more than the largest so far", as one of two candidates,
or as an escape. The candidates come from a table of
the last face of each vertex (direct mapped, 65,536 entries): for `v`, the neighbours of
the previous corner's vertex there (a shared edge); for `vt` and `vn`, the index last
paired with this vertex, and the vertex's index plus the last offset between them.

## Format

The format is specified in [FORMAT.md](FORMAT.md).

## Measured, not kept

- An adaptive binary range coder for the values (LZMA-style; bit length through a tree,
  two top bits modelled): 1.5% smaller than rANS, but nine serial binary decisions per
  value made decoding slower.
- Contexts for the residuals (bit length of the previous one): no gain over a model per
  kind and position.
- rANS for the corner symbols and line ops: zstd wins (it sees the patterns).
- Larger move-to-front caches for corner indices (128 to 2,048): worse. An escape against
  "one more than the largest index": no gain.
- Relative coding of the unit normal selector (against the last one): slightly larger.
- A line predictor window of 8 or 12: 2-5% faster compression, 0.2-0.4% larger. Staying
  with the last predictor unless another saves 8 or 16 bits: mixed.
- Faster digit output (digit pairs, tokens assembled on the stack) and branch-free
  cache search: slower or no change in A/B runs.

## Testing

```sh
cargo test --release
../target/release/aqr-obj fuzz -n 200 $(find corpus -name '*.obj' -size -5M)
```

- `tests/roundtrip.rs`: edge cases (no line end, CRLF, tabs, odd numbers, relative
  indices, long lines, more templates than the table holds, binary junk), every writer
  style mixed in one file, a mesh using every corner symbol, blocks stored whole in
  between (what the encoder forgets then); through the formula, under several block
  sizes and threads.
- `tests/formula.rs`: the formula's structure, and damaged inputs (errors or other
  output, never panics).
- Unit tests: rANS; those of the number formats are in `aqr-num`.
- `aqr-obj fuzz`: mutated files (bytes, tokens, line copies, line ends, digits) must
  round-trip through the formula with verification off, small and large blocks, some
  stored whole, whatever the threads.

## Layout

| File | Contents |
|---|---|
| `src/formula.rs` | The module: formula of a file, and its decoder |
| `src/codec.rs` | Blocks, their streams, verification, threads |
| `src/block.rs` | Streams, templates, line encoder and decoder |
| `src/style.rs` | Number mode per kind of line (encoder; the modes are `aqr-num`) |
| `src/recent.rs` | Equal-line references, line predictor window |
| `src/corner.rs` | Face corner indices |
| `src/ans.rs` | rANS for value streams |
| `src/varint.rs` | Stream cursor (LEB128 from `aqr-common`); heap accounting is `aqr_common::mem` |
| `src/main.rs` | Bench, stats, fuzz |
| `examples/` | Experiments behind the choices above |
| `scripts/` | Corpus, baselines, comparison, profiling (`profile.py` over samply) |

## Next steps

- Compression speed of `%.17g` files: the mode check and the verification both print
  each number from exact digits; one of them could compare values instead.
- Connectivity: faces in random order (decimated scans) cost about the logarithm of the
  vertex count per index; coding a vertex against the faces around its neighbours needs
  more state than the bounded tables keep.
- Scale changes of `%g` numbers (2.7% of the output) could be predicted from the
  predictor's magnitude.
- `.mtl` files, and OBJ inside archives, are left to the general compressor.
