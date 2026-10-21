# aqr-jpeg (`jrc`)

A lossless JPEG recompressor: files are restored bit-exactly (same family as Lepton and
brunsli). Speed comes first. The goal is at least 100 MB/s single-threaded for both
compression and decompression; compression ratio is only pursued where it does not cost
performance.

The starting point was an evaluation of lepton_jpeg_rust. At about 19 MB/s single-threaded it
was far below the target, so this is a new design from scratch.

## Current performance

Apple M4 (10 cores: 4 P + 6 E), release build with `RUSTFLAGS="-C target-cpu=native"`.
Ratio is output size / input size.

| Set | Ratio | `-j 1` compress / decompress | All cores compress / decompress |
|---|---|---|---|
| sequential (camera and phone photos etc., 101 MB) | 0.8360 | 106 / 89 MB/s | 300–390 / 300–385 MB/s |
| progressive (41 MB) | 0.8933 | 43 / 47 MB/s | 90–122 / 120–168 MB/s |

All-core figures are ranges because they varied with thermal state and background load.

## Usage

```sh
RUSTFLAGS="-C target-cpu=native" cargo build --release

jrc c in.jpg out.jrc                 # compress
jrc d out.jrc back.jpg               # decompress
jrc bench [-n iters] file...         # roundtrip check + ratio / per-phase times / throughput
jrc info file...                     # parse and decode, per-scan information
jrc pscan [-n iters] file...         # per-scan decode / encode times of progressive files
jrc fuzz [-n N] [-s seed] file...    # mutated inputs: roundtrips must stay exact, no panics
jrc mutenc [-n N] [-s seed] file...  # roundtrip of valid non-canonical re-encodings
```

Every command takes `-j threads` (default: all cores).

Auxiliary binaries:
- `decbench`: loops only the model decoder, the model encoder (`enc`) or the Huffman
  re-encoder (`huff`).
- `huffbench`: Huffman decoding throughput, and a coefficient comparison against a
  `tools/coefdump.c` (libjpeg) dump.

### Debug / test environment variables

| Variable | Effect |
|---|---|
| `JRC_VERIFY_PROGRESSIVE` | Re-encodes every progressive scan and checks the inline verdict; panics if a scan judged reproducible re-encodes differently |
| `JRC_VERIFY_PARALLEL` | Checks parallel Huffman decoding (coefficients and verdicts) against serial decoding; panics on any difference |
| `JRC_DEBUG_CANON` | Prints why a progressive scan was judged non-canonical |
| `JRC_DEBUG_SPEC` | Prints speculative-decoding phase times and fallback reasons |
| `JRC_DEBUG_TABLES` | Prints the size breakdown of the serialised model tables |

`compress` catches panics and stores the input instead, so an oracle failure shows up in fuzz
results as `internal panic`; the input is saved as `fuzz_panic_<round>.jpg`.

## Layout

| File | Contents |
|---|---|
| `src/jpeg.rs` | Parsing, destuffing, fast Huffman decoding (serial / restart-interval parallel / speculative parallel), canonicality checks, sequential scan re-encoding (parallel chunks) |
| `src/progressive.rs` | Progressive decoding (jdphuff semantics) with inline canonicality checks, scan-script validation, row pipeline across scans, re-encoding (jcphuff semantics) |
| `src/fast.rs` | Entropy stage: the "P1" model, table normalisation and serialisation, 4-way rANS, stripes |
| `src/container.rs` | Container format (spec: [FORMAT.md](FORMAT.md)), top-level compress / decompress, recursive compression of embedded JPEGs, stored fallback |
| `src/variant.rs` | Test generator: re-encodes the same coefficients in valid but non-canonical ways |
| `src/model.rs` | The earlier, heavier model (reference only; only its `Phases` struct is still used) |
| `tests/regress.rs` | Regression inputs |

## Design

### 1. Huffman decoding (compression side)

- **Destuffing prepass**: 0xFF00 → 0xFF and RST splitting are done first with memchr, so the
  hot loop never deals with markers.
- **Bit reader**: MSB-first 64-bit buffer with a branchless refill.
- **Lookup tables**:
  - Combined code + extra-bits LUT (`FAST_BITS = 12`).
  - For AC, a Pair LUT that yields two symbols at once.
  - The AC loop is unrolled 4×. One refill guarantees 56 bits, enough for 4 × 12-bit
    lookups; this is pinned with a `const assert`.
- **State handling**: hot-loop state lives in scalar locals; the slow path is only called on a
  copy, so the state variables never have their address taken.
- **Inline canonicality checks**: every degree of freedom an encoder has beyond the
  coefficients is checked while decoding:
  - EOB position, (r,0) symbols, padding bits (all 1s, or consistently all 0s)
  - exact consumption of each segment, RST numbering
  - number of trailing RSTs, `|DC| ≤ 16383`
  - A scan that passes is reproduced bit-exactly by `encode_scan`.
- **Truncation**: in a scan whose data ends early, the K complete MCUs are modelled and the
  remaining bits are stored verbatim.

### 2. Entropy model "P1" (`fast.rs`)

Blocks are processed in raster order per component, coefficients in zigzag order.

- **Nonzero count**: number of nonzero AC coefficients per block (alphabet 64). Context: the
  average of the counts of the blocks above and to the left.
- **AC coefficients**: for every position up to the last nonzero one, the cat (= bit length of
  |x|) is coded.
  - Context: (luma/chroma, position k, half-logarithmic class of the remaining nonzero count,
    11 classes).
  - Sign and remaining bits are stored as raw bits.
- **DC**: predicted from boundary continuity with the blocks above and to the left
  (Lakhani-style, coefficient domain).
  - Only row 0 and column 0 coefficients are involved. The sums are fixed-point i32, the
    final scaling and rounding basic f32 operations, and the DCT basis a constant table (no
    libm), so the prediction is exact and platform independent.
  - Context: neighbour residual sizes and a bucket of the disagreement (uncertainty) between
    the two predictions.
- **Tables**: static per-file histograms (two pass).
  - Each group gets the precision shift (0–8) that minimises table bits + data bits.
  - A group whose own table is not worth its cost inherits, with a 1-bit flag, the
    distribution of its parent (nz: component × log2 bucket; AC: (component, k);
    DC: (component, neighbour bucket)) or, for AC, of its grandparent
    (component × 10 position bands).
- **rANS**:
  - 4-way interleaved 64-bit rANS, 32-bit IO, `PROB_BITS = 12`.
  - The encoder uses reciprocals; it is kept `#[inline(never)]` and writes into a
    preallocated buffer without pushes.
  - The decoder finds symbols via a 64-entry coarse LUT per context group followed by a short
    refinement loop, and renormalises branchlessly with `select_unpredictable`.
  - Both candidate contexts of the next position are computed ahead, off the dependency chain.
  - Bounds are checked once per block, with padded buffers.
- **Stripes**: the image is split into horizontal stripes of whole MCU rows
  (about 32K blocks each, at most 256).
  - Each stripe has its own rANS and raw streams; the tables are shared.
  - Contexts do not cross stripe boundaries, which costs about 0.04 percentage points of
    ratio.
  - The stripe count depends only on the image size, so the output does not depend on the
    number of threads.

### 3. Re-encoding (decompression side)

- A branchless MSB bit writer fills an unstuffed buffer; stuffing and RST insertion are a
  separate pass.
- Progressive follows libjpeg's jcphuff exactly:
  - EOBRUN is flushed at 0x7FFF.
  - Up to 1000 correction bits are buffered.
  - Hot state is kept in locals and macros.

### 4. Progressive

- **Decoding**: libjpeg jdphuff semantics. Supports DC first / refine, AC first (EOBRUN) and
  AC refine (history masks).
- **Inline canonicality checks**: the verdict is made while decoding, without re-encoding at
  compression time:
  - EOB runs never cross a segment or the end of the scan
  - a run is cut only where forced (0x7FFF, or BE > 937 in refinement)
  - a ZRL is followed by a nonzero coefficient
  - padding and markers
- **Scan-script validation**: libjpeg's `coef_bits` rule.
  - Every coefficient bit must be coded exactly once: first by a first scan (ah = 0), then by
    refinements continuing from the previous al.
  - From the first scan that breaks the rule on, scans are stored raw. Otherwise an earlier
    scan could not be reproduced from the final coefficients.
- **Fallback**: the first scan that fails to decode and all later ones are raw (later scans
  depend on earlier ones).

### 5. Parallelism (rayon)

The single-threaded path is unchanged, and the parallel paths are designed to give exactly the
serial result.

| Stage | Method |
|---|---|
| Model encode / decode | Per stripe in parallel. Histograms are summed, then the tables are built once |
| Sequential re-encoding | MCU chunks encoded in parallel (DC predictions are known from the coefficients) → per-chunk bit shift and stuffing → concatenation. With RST, chunks are whole restart intervals, aligned, so no shifting is needed |
| Sequential decoding, with RST | Groups of restart intervals in parallel |
| Sequential decoding, no RST | Speculative decoding, see below |
| Progressive decoding | Row pipeline across scans, see below |
| Progressive re-encoding | Scans encoded in parallel (independent given the final coefficients) |

**Speculative decoding** (scans without RST):
- The destuffed data is split into K chunks by byte offset. Each chunk is decoded as if an
  MCU started at its beginning, recording at every MCU start (bit position, block index,
  running DC differences, running count of non-canonical blocks).
- Huffman codes have no guaranteed self-synchronisation, but they resynchronise quickly in
  practice. A serial step then continues the true decoding past each chunk boundary until it
  reaches an MCU start at a bit position the next chunk recorded.
- From there on the speculative result equals the true one, so its blocks are copied into
  place with the DC differences summed into absolute values.
- Measured synchronisation happens within 1–10 MCUs. Anything unusual (errors, truncation, no
  synchronisation) falls back to serial decoding.

**Progressive row pipeline**:
- Workers take scans in order. Each scan publishes, per component, how many block rows it has
  finished; a later scan waits until the earlier scans of the same component have finished
  the rows it needs.
- So each block is only ever accessed by one scan at a time, in scan order.
- If any scan fails, everything is decoded again serially.

### 6. Container and robustness

The full specification is in [FORMAT.md](FORMAT.md).

```text
magic 69 04 7E 8E 9D E0, u16 version: 0 = sequential, 1 = progressive, 2 = stored
then streams (varint length + data); each stream's meaning follows from the earlier ones:
  head: u64 xxh3 of the original (checked after decompression; a mismatch is an error),
        embedded JPEGs (gap, len, kind: 0 = nested container stream / 1 = copy of an earlier one)
  nested containers, one per embedded JPEG of kind 0
  cut blob                     -- headers, markers, trailer without scan data or embedded JPEGs
  scan info: per scan flags (zero padding, raw, truncated, boundary) + RST count / truncation
             point, stripe count
  raw scan data / truncated tails
  model tables, then per stripe: rANS words, raw bits
```

- `compress` never fails. Whatever cannot be reproduced (unsupported or non-canonical coding,
  corruption, internal panics) is stored raw per scan, or else the whole input is stored.
- Embedded JPEGs of at least 1 KB (Exif / Photoshop thumbnails, MPF and vendor secondary
  images, ...) are compressed recursively, up to depth 3.
- Edge cases handled: truncated files, trailing RSTs, dangling trailing 0xFF, fill bytes.

## Verification

- `jrc bench`: every file is roundtripped and compared byte for byte.
- `jrc fuzz`: inputs mutated by bit flips, truncation, deletion, insertion and marker-byte
  injection must roundtrip exactly without panics. Corrupted containers are also decompressed
  and must not panic.
- `jrc mutenc`: variants that re-encode the same coefficients with valid non-canonical choices
  (ZRL placement, padding, fill, trailing RSTs, EOBRUN splits, ...) must roundtrip exactly.
- With both oracles (`JRC_VERIFY_PROGRESSIVE`, `JRC_VERIFY_PARALLEL`) enabled: 6000 fuzz
  inputs, 600 mutenc rounds and a 208 MB corpus, all passing.
- `cargo test --release`: `tests/regress.rs`, currently one progressive scan-script violation
  found by fuzzing.

## Tried and dropped

| Attempt | Outcome |
|---|---|
| Pair tokens, run-cat tokens | Worse ratio |
| Dependency-free contexts, two-pass AC decoding, `min(rem, 8)` | No gain |
| AC context from neighbour magnitudes | −0.22 pp ratio but −6% / −13% speed (table working set exceeds L1) |
| NEON symbol search (find16) | Slower due to GPR transfers → replaced by the coarse LUT |
| Branch-free select_bit, combined refinement LUT | No gain |
| Modelling quantisation tables (libjpeg quality tables) | Gain looked small; set aside |
| qoi_benchmark_suite corpus | Not JPEG; not used |

## Open issues and limitations

- **Progressive compression parallelism is capped** by the chain of refinement scans of one
  component. Refinement decoding needs each block's history mask, and a speculative decoder
  cannot know which block it is in, so a single scan cannot be split.
- **Progressive re-encoding**: each scan is encoded serially. It could be split by
  reconstructing the EOBRUN and BE state at chunk boundaries from the blocks before them.
- **Stored ratio**: fuzz and mutenc store a large share of inputs verbatim (393 of 600 mutenc
  rounds). The version before parallelisation gives the same numbers; the cause has not been
  investigated yet.
- **Sample photos**: `samples/` holds personal photos (with GPS / Exif) and is excluded from
  version control via `.gitignore`.
