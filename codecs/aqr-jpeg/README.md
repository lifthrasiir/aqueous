# aqr-jpeg

A lossless JPEG recompressor: files are restored bit-exactly (same family as Lepton and
brunsli). Speed comes first. The goal is at least 100 MB/s single-threaded for both
compression and decompression; compression ratio is only pursued where it does not cost
performance.

The starting point was an evaluation of lepton_jpeg_rust. At about 19 MB/s single-threaded it
was far below the target, so this is a new design from scratch.

## Current performance

Apple M4 (10 cores: 4 P + 6 E), release build with `RUSTFLAGS="-C target-cpu=native"`.
Ratio is output size / input size.

Measured on 2026-10-24 on an Apple M4, one thread, through `aqr-show` (see the
[file formats overview](../../docs/file-formats.md#how-it-was-measured)): each file archived at level `mid` with
its round-trip check, then extracted. Ratios are tentative, since the decoder modules were not
in the archives then. Memory is the peak RSS of that process (input, archive and output
included) over the input size. This build is without `target-cpu=native`.

| set | files | MB | ratio | zstd -3 | compress | decompress |
|---|---|---|---|---|---|---|
| `samples/` (camera and phone photos) | 13 | 60.0 | 0.752 | 0.908 | 50.1 MB/s | 91.4 MB/s |
| codec-corpus jpeg-conformance | 44 | 74.7 | 0.805 | 0.906 | 52.4 MB/s | 123.5 MB/s |
| codec-corpus ultrahdr-conformance | 14 | 29.7 | 0.788 | 0.919 | 48.7 MB/s | 104.4 MB/s |
| codec-corpus zune | 20 | 18.4 | 0.795 | 0.955 | 12.6 MB/s | 26.7 MB/s |
| all (with imageflow) | 96 | 186 | **0.785** | 0.913 | **39 MB/s** | **82 MB/s** |

zune's large progressive and subsampled test images are the slow ones. Memory is about
28 MB + 8.2× the file (it was 56 MB + 25× while the coefficients were held whole). Sequential
files are coded a stripe at a time (section 6), so their memory no longer follows the pixels:
a 1.3 MB file of 7680×4320 takes 54 MB (it took 363 MB). Progressive files still hold their
coefficients whole: a 1.4 MB one of that size takes 259 MB.

## Usage

```sh
RUSTFLAGS="-C target-cpu=native" cargo build --release

aqr-jpeg bench [-n iters] file...         # roundtrip check (formula, .aqr archive) + ratio / throughput
aqr-jpeg info file...                     # parse and decode, per-scan information
aqr-jpeg pscan [-n iters] file...         # per-scan decode / encode times of progressive files
aqr-jpeg fuzz [-n N] [-s seed] file...    # mutated inputs: roundtrips must stay exact, no panics
aqr-jpeg mutenc [-n N] [-s seed] file...  # roundtrip of valid non-canonical re-encodings
```

Every command takes `-j threads` (default: all cores).

Auxiliary binaries:
- `aqr-jpeg-decbench`: loops only the model decoder, the model encoder (`enc`) or the Huffman
  re-encoder (`huff`).
- `aqr-jpeg-huffbench`: Huffman decoding throughput, and a coefficient comparison against a
  `tools/coefdump.c` (libjpeg) dump.

### Debug / test environment variables

| Variable | Effect |
|---|---|
| `JRC_VERIFY_PROGRESSIVE` | Re-encodes every progressive scan and checks the inline verdict; panics if a scan judged reproducible re-encodes differently |
| `JRC_VERIFY_PARALLEL` | Checks parallel Huffman decoding (coefficients and verdicts) against serial decoding; panics on any difference |
| `JRC_VERIFY_STRIPED` | Checks stripe-wise decoding and modelling (scan info, model) and re-emission (scan bytes) against holding the coefficients whole; panics on any difference |
| `JRC_DEBUG_CANON` | Prints why a progressive scan was judged non-canonical |
| `JRC_DEBUG_SPEC` | Prints speculative-decoding phase times and fallback reasons |
| `JRC_DEBUG_STRIPED` | Prints how a scan was decoded stripe by stripe (serially, by restart intervals, from located starts) and the outcome |
| `JRC_DEBUG_TABLES` | Prints the size breakdown of the serialised model tables |

`formula::encode` catches panics and leaves the input uncoded, so an oracle failure shows up
as a file that is not coded.

## Layout

| File | Contents |
|---|---|
| `src/jpeg.rs` | Parsing, destuffing, fast Huffman decoding (serial / restart-interval parallel / speculative parallel, whole or stripe by stripe), canonicality checks, sequential scan re-encoding (parallel chunks or stripes) |
| `src/progressive.rs` | Progressive decoding (jdphuff semantics) with inline canonicality checks, scan-script validation, row pipeline across scans, re-encoding (jcphuff semantics) |
| `src/fast.rs` | Entropy stage: the "P1" model, table normalisation and serialisation, 4-way rANS, stripes |
| `src/codec.rs` | The codec behind the formula module: blob and scan info, embedded JPEGs, model and scan re-emission glue |
| `src/formula.rs` | The formula module `jpeg-recons` (spec: [FORMAT.md](FORMAT.md)): `encode`, `decode`, recursive coding of embedded JPEGs |
| `src/variant.rs` | Test generator: re-encodes the same coefficients in valid but non-canonical ways |
| `src/model.rs` | The earlier, heavier model (reference only; only its `Phases` struct is still used) |
| `tests/regress.rs` | Regression inputs |
| `tests/memory.rs` | Peak heap use of a large sequential file (counting allocator) |

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

- **Nonzero count**: number of nonzero AC coefficients per block, in the context of its
  neighbours' counts.
- **AC coefficients**: for every position up to the last nonzero one, the cat (= bit length of
  |x|) is coded, in the context of the position and the remaining nonzero count. Sign and
  remaining bits are stored as raw bits.
- **DC**: predicted from boundary continuity with the blocks above and to the left
  (Lakhani-style, coefficient domain).
  - Only row 0 and column 0 coefficients are involved. The sums are fixed-point i32, the
    final scaling and rounding basic f32 operations, and the DCT basis a constant table (no
    libm), so the prediction is exact and platform independent.
  - Context: neighbour residual sizes and the disagreement (uncertainty) between the two
    predictions.
- **Tables**: static per-file histograms (two pass).
  - Between the passes a stripe's symbols are kept compact: per block the nonzero-count and
    DC symbols (u16) and the last nonzero position, per AC symbol one byte (its context's
    remaining-count class and its cat; the position follows from the order). They are
    expanded per stripe, 63 lanes per block, right before rANS coding.
  - Each group gets the precision shift (0–8) that minimises table bits + data bits.
  - A group whose own table is not worth its cost inherits, with a 1-bit flag, the
    distribution of its parent or, for AC, of its grandparent.
- **rANS**:
  - 4-way interleaved 64-bit rANS, 32-bit IO, `PROB_BITS = 12`.
  - The encoder uses reciprocals and renormalises branchlessly (the word is always stored and
    kept only when renormalising); it is kept `#[inline(never)]` and writes backwards into a
    buffer of typical size (a word per 8 symbols), which is only checked, and grown if
    needed, once per 1024 symbols.
  - The decoder finds symbols via a 64-entry coarse LUT per context group followed by a short
    refinement loop, and renormalises branchlessly with `select_unpredictable`.
  - Both candidate contexts of the next position (the previous coefficient zero or not) are
    computed and looked up ahead, then one is picked: the lookups are off the dependency chain.
  - Bounds are checked once per block, with padded buffers.
- **Stripes**: the image is split into horizontal stripes of whole MCU rows.
  - Each stripe has its own rANS and raw streams; the tables are shared.
  - Contexts do not cross stripe boundaries, which costs about 0.04 percentage points of
    ratio.
  - The stripe count depends only on the image size, so the output does not depend on the
    number of threads.

### 3. Re-encoding (decompression side)

- A branchless MSB bit writer fills an unstuffed buffer; stuffing and RST insertion are a
  separate pass. Sequential blocks put their codes two at a time.
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
| Stripe-wise sequential decoding | With RST, each stripe from the start of its restart interval; without, the stripes' starts are located by speculative decoding that keeps only MCU positions, then windows of stripes are decoded with DC predictions relative to their start and rebased in order |
| Stripe-wise re-encoding | Each stripe encoded as it is decoded from its rANS streams; a stripe's first MCU, whose DC predictions continue the previous stripe, is coded serially afterwards, then the parts are shifted and stuffed in parallel |
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

### 6. Memory

Sequential files with a single scan, which most files are, never hold their coefficients
whole: they are decoded, modelled and re-emitted a stripe at a time (`jpeg::decode_striped`,
`fast::Modeller`, `jpeg::encode_striped`). Each worker holds a stripe or two (32K blocks of
136 bytes each), and what follows the file is the input, its destuffed scan, the compact
symbols and the output. Truncated scans are handled stripe-wise too, and a scan found not to
be reproducible is left undecoded from there on. Progressive files and files with several
scans hold their coefficients whole as before. Buffers kept per thread for reuse are dropped
above 16 MB.

### 7. Formula module and robustness

The format is specified in [FORMAT.md](FORMAT.md).

- `formula::encode` never fails: it gives no formula for what cannot be reproduced
  (unsupported coding, corruption, internal panics) as a whole, and codes scans that are not
  canonical raw.
- Embedded JPEGs of at least 1 KB (Exif / Photoshop thumbnails, MPF and vendor secondary
  images, ...) are coded recursively, up to depth 3.
- Edge cases handled: truncated files, trailing RSTs, dangling trailing 0xFF, fill bytes.

## Verification

- `aqr-jpeg bench`: every file is roundtripped through its formula and an `.aqr` archive, and compared
  byte for byte.
- `aqr-jpeg fuzz`: inputs mutated by bit flips, truncation, deletion, insertion and marker-byte
  injection must roundtrip exactly without panics. Corrupted archives are also decoded
  and must not panic.
- `aqr-jpeg mutenc`: variants that re-encode the same coefficients with valid non-canonical choices
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
| Edge (row 0 / column 0) signs as rANS symbols, predicted from boundary continuity once the block's other AC coefficients are known (Lepton-style) | −0.60 pp ratio but −26% / −23% speed (compression / decompression); predicted from the neighbours only, −0.36 pp and about as slow. The extra symbols (+21%) alone cost −14% / −7%, the prediction the rest |
| Edge cat context from that prediction (estimated) | −0.32 pp, but about three times the AC context groups |
| Separate 7×7 and edge nonzero counts (estimated) | +0.35 pp (worse) on its own |
| Lepton-style pixel-domain DC prediction (estimated) | −0.04 pp |
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
- **Uncoded share**: fuzz and mutenc leave a large share of inputs uncoded. The cause has not
  been investigated yet.
- **Sample photos**: `samples/` holds personal photos (with GPS / Exif) and is excluded from
  version control via `.gitignore`.
