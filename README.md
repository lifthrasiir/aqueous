# obj-recons (`objr`)

Lossless Wavefront OBJ recompression: an OBJ file is stored smaller, and restored bit for
bit. Meant as a component of a general-purpose compressor, so speed comes first: the target
is 100 MB/s single-threaded on an Apple M4, both ways. zstd and a small rANS coder are the
entropy backends.

```sh
cargo build --release
objr c [-l LEVEL] [-B BLOCK] [-x] [-t N] in.obj out.objr   # compress
objr d [-t N] out.objr back.obj                            # decompress
objr bench [-n ITERS] [-t N] [-m] [-v] [-c|-d] FILE...     # round trip check, ratio, throughput
objr stats FILE...                                  # line templates and lines kept as text
objr fuzz [-n N] [-s SEED] FILE...                  # mutated files and containers
```

```rust
use obj_recons::container::{Options, Stats};
let packed = obj_recons::compress(&obj);               // never fails: stores what it cannot code
assert_eq!(obj_recons::decompress(&packed)?, obj);     // checked against an xxh3 of the original
// Streaming forms, with memory bounded whatever the file size:
obj_recons::container::compress_to(&obj, &Options::default(), &mut Stats::default(), &mut writer)?;
obj_recons::container::decompress_to(&packed, &mut writer)?;
```

Any input is accepted: lines that do not parse are kept as text, and a block that fails
verification is zstd-compressed whole.

## Results

Apple M4, one thread, thread CPU time, best of 3 (`objr bench -n 3`, through
`scripts/compare.py`); MB/s of OBJ file, both ways. Output size / input size; every file
round-trips. Other compressors on each file, summed (`scripts/baseline.sh`): zstd 1.5.7
(`--single-thread`; `-19` with `--long=27`), bzip2 1.0.8 `-9`, xz 5.8.4 `-9 -T1`.

| set | files | size | **objr** | compress | decompress | zstd -3 | zstd -19 | bzip2 -9 | xz -9 |
|---|---|---|---|---|---|---|---|---|---|
| mcguire (scenes, scans) | 21 | 3,897 MB | **0.0565** | 138 MB/s | 385 MB/s | 0.2140 | 0.1374 | 0.1844 | 0.1159 |
| writers (number styles) | 27 | 476 MB | **0.0729** | 103 MB/s | 314 MB/s | 0.2385 | 0.1603 | 0.2208 | 0.1414 |
| c3d (scans, `%.17g`) | 17 | 69 MB | **0.1447** | 93 MB/s | 257 MB/s | 0.3270 | 0.2380 | 0.2801 | 0.2172 |
| threejs | 7 | 6 MB | **0.1301** | 113 MB/s | 300 MB/s | 0.2873 | 0.2163 | 0.2769 | 0.1935 |
| **all** | 72 | 4,448 MB | **0.0598** | **132 MB/s** | **373 MB/s** | 0.2185 | 0.1415 | 0.1899 | 0.1203 |

Half the size of xz -9 and 58% smaller than zstd -19 overall; every file is smaller than
under any of them. Compression includes verification (each block is rebuilt from its
streams and compared; on a mismatch it is stored whole), about a third of its time; `-x`
skips it. The machine was not idle (desktop applications); figures are CPU time of the
thread.

The `%.17g` scans (c3d) compress below 100 MB/s: each number is printed twice (to check
the mode, then to verify), each time from exact digits of a double.

Two more references, on fewer files:

- [paq8px](https://github.com/hxim/paq8px) (`c84f576`) `-8`, on the 17 files under 5.2 MB
  (59 MB): 0.1189, against 0.1386 for objr, at about 20 KB/s.
- [Draco](https://github.com/google/draco) 1.5.7 `draco_encoder -cl 10 -qp 0`, which keeps
  geometry and connectivity but not the file (values as floats, its own order, no text):
  0.1052 on the 69 files it reads (3.7 GB), against 0.0573 for objr; on the three.js
  models 0.0966 against 0.1301.

### Threads

`-t N` (`Options::threads`, `decompress_threads`; `objr c` and `objr d` default to the
available cores): the output is the same bytes whatever the number of threads, and one
thread runs the code above unchanged. Coded blocks carry state over, so the line coding
and decoding stay on one thread, in order; around them, a pool parses the lines of the
blocks ahead (number modes, render checks), verifies, and codes the streams; decompressing,
it decodes the streams of the blocks ahead and prints the numbers of decoded blocks (the
decoder emits values, `block::Deferred`). Wall time, all 72 files (about ±10% between
runs on this machine):

| threads | compress | decompress |
|---|---|---|
| 1 (CPU time) | 125 MB/s | 346 MB/s |
| 2 | about 240 MB/s | about 450 MB/s |
| 4-10 | about 250 MB/s | about 490 MB/s |

The line coding (predictors, corner caches) and the line decoding are the limit; raising
the calling thread's QoS class changed nothing. Going further needs independent segments
(state reset every so many blocks): measured on the McGuire set, 0.35% larger with 64 MB
segments, 0.9% with 32 MB, 2.1% with 8 MB; not done.

### Memory

The heap beyond input and output does not grow with the file: at most about 36 MB
compressing and 20 MB decompressing (`objr bench -m`: 1.1 GB San Miguel 36 MB and 20 MB,
9.6 MB igea 22 MB and 14 MB, a 125 KB file 2.2 MB and 0.4 MB). That is blocks of 4 MB,
rings of the last 65,536 lines per kind, a table of 65,536 vertices, and templates up to
4 MB. `compress_to` and `decompress_to` write a block at a time; `objr c` and `objr d` use
them.

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

A file is cut into blocks of 4 MB at line ends. Each block becomes about 35 streams, each
compressed with zstd (level 3) or rANS, whichever is smaller; state (previous values,
templates, recent lines and faces) carries over from block to block, but its size does not
depend on the file's.

### Lines and templates (`src/block.rs`)

A `v`, `vt`, `vn`, `vp`, `f` or `l` line becomes a template and values. The template holds
everything but the values: keyword, separators (any blanks), each number's format or each
corner's pattern (`a`, `a/b`, `a//c`, `a/b/c`, relative indices), trailing blanks and the
line end. Templates are numbered by first use and defined where first used; a line costs
its template number (one byte, nearly free under zstd). Any other line, or one that does
not parse back to itself, is kept as text.

### Numbers (`src/num.rs`, `src/dec.rs`, `src/style.rs`)

A number is an integer at a scale (digits after the point), printed in a mode:

- fixed, `%.Nf`-like: the scale is in the template (trailing zeros are free);
- C's `%.Pg`: the scale follows from the magnitude; trailing zeros are dropped;
- `%.Pe`, Python's `repr`, JavaScript's `toString`, Rust's `Display`;
- any of the last four printed from a float: the value is the shortest decimal that
  rounds to the `f32` or `f64`, and is printed from that float (`%.17g`,
  `%.18e`, `repr(float(np.float32(x)))`). A `%.17g` number then costs what its double
  holds, not 17 digits.

Shortest decimals come from Żmij; exact digits of a double (`%.17g`, `%.18e`) from 128-bit
integer arithmetic. Only IEEE basic operations are used, so decoding gives the same bytes
everywhere. The encoder picks a mode per kind of line from a sample of each block (by the
digits it would code and its scale changes), checks every number by printing it back, and
falls back to fixed or `%g` (or text) for numbers that are printed otherwise.

### Prediction (`src/block.rs`, `src/recent.rs`)

- Equal lines: a `v`, `vt` or `vn` line equal to one of the last 65,536 of its kind is
  coded as the distance back (triangle soups repeat each vertex several times).
- Line predictors: per `v`, `vt`, `vn` line, the predictor that leaves the fewest
  residual bits among the previous numbers, the lines up to 16 back, and the
  parallelograms `x[-1] + x[-k] - x[-k-1]`; the encoder stays with the last one unless
  another saves more than a few bits.
- Unit normals: when the normals of a block have length 1, the largest component is
  predicted from the other two, `±sqrt(1 - a² - b²)`.
- Residuals go to streams per kind and position, rANS coded per value as bit length and
  two top bits, the other bits raw (`src/ans.rs`), unless zstd does better (repeats).

### Faces (`src/corner.rs`)

Per attribute (v, vt, vn), a move-to-front cache of the 32 last indices; an index is coded
as its cache position, as "one more than the largest so far", as one of two candidates,
or as the difference from the nearest cached index. The candidates come from a table of
the last face of each vertex (direct mapped, 65,536 entries): for `v`, the neighbours of
the previous corner's vertex there (a shared edge); for `vt` and `vn`, the index last
paired with this vertex, and the vertex's index plus the last offset between them.

## Format

```text
"OBJR" version(1) varint(length) u64-LE(xxh3 of the original)
blocks, until the length is reached:
  u8 mode, varint(raw length)
  mode 0 (whole): varint(coded length) zstd(block)
  mode 1 (coded): per stream (block::NSTREAMS):
                    varint(raw length) [u8(0 stored | 1 zstd | 2 rANS) varint(coded length) data]
```

Streams and template keys are documented in `src/block.rs`; rANS in `src/ans.rs`.

## Measured, not kept

- An adaptive binary range coder for the values (LZMA-style; bit length through a tree,
  two top bits modelled): 1.5% smaller than rANS, but nine serial binary decisions per
  value made decoding slower.
- Contexts for the residuals (bit length of the previous one): no gain over a model per
  kind and position.
- rANS for the corner symbols and line ops: zstd wins (it sees the patterns).
- Trying only last block's winner of zstd and rANS per stream: 2% larger.
- Larger move-to-front caches for corner indices (128 to 2,048): worse. An escape against
  "one more than the largest index": no gain.
- Relative coding of the unit normal selector (against the last one): slightly larger.
- Blocks of 16 MB: 2.6% smaller (with zstd alone), four times the memory.
- zstd level 5: 2.1% smaller for 6% slower compression; level 9: 3.4% for 13%.
- A line predictor window of 8 or 12: 2-5% faster compression, 0.2-0.4% larger. Staying
  with the last predictor unless another saves 8 or 16 bits: mixed.
- Faster digit output (digit pairs, tokens assembled on the stack) and branch-free
  cache search: slower or no change in A/B runs.

## Format stability

`scripts/format-ref.tsv` holds a hash of the output for every corpus file, and
`scripts/format-check.sh` checks that changes keep it (`--update` rewrites it).

## Testing

```sh
cargo test --release
target/release/objr fuzz -n 200 $(find corpus -name '*.obj' -size -5M)
```

- `tests/roundtrip.rs`: edge cases (no line end, CRLF, tabs, odd numbers, relative
  indices, long lines, more templates than the table holds, binary junk), every writer
  style mixed in one file, a mesh using every corner symbol, blocks stored whole in
  between (what the encoder forgets then), and 3,000 corrupted containers (errors, never
  panics).
- Unit tests: number formats against the C library's `%g`, exact digits of doubles
  against the standard library, writer renders, rANS.
- `objr fuzz`: mutated files (bytes, tokens, line copies, line ends, digits) must
  round-trip with verification off, small and large blocks, some stored whole;
  corrupted containers must fail cleanly.

## Layout

| File | Contents |
|---|---|
| `src/container.rs` | File format, blocks, zstd/rANS choice, verification |
| `src/block.rs` | Streams, templates, line encoder and decoder |
| `src/style.rs` | Number mode per kind of line (encoder) |
| `src/num.rs` | Fixed and `%g` numbers |
| `src/dec.rs` | Decimals, floats, writer renders |
| `src/recent.rs` | Equal-line references, line predictor window |
| `src/corner.rs` | Face corner indices |
| `src/ans.rs` | rANS for value streams |
| `src/varint.rs`, `src/mem.rs` | LEB128, heap accounting |
| `src/bin/objr.rs` | Command-line tool, bench, stats, fuzz |
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
