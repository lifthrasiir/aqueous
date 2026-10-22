# aqr-flac (`flr`)

A lossless FLAC recompressor: FLAC files are coded smaller and restored bit for bit, as the
module `flac-recons` of entry formulas (`aqr_flac::formula`, archived by aqr-compress).
WAV, AIFF / AIFC and headerless PCM go through the same model and come back byte for byte.
Speed comes first: the target is 100 MB/s of input, single-threaded on an Apple M4, in both
directions. Compression ratio is pursued only where it does not cost that.

## Results

The cost is about 30 cycles per sample each way, so speed per MB falls with the bits per
sample: the most compressible 16-bit FLAC files (about 6 bits per sample) are coded at
100–110 MB/s on an Apple M4, one thread. The ratio and speed tables of earlier versions were
measured the standalone file format that this module replaced; they need re-measuring as
`.aqr` archives with `flr bench`.

FFmpeg's WavPack encoder at its highest level was about 1–2% smaller than `flac -8` on
8 IA tracks decoded to WAV (and far slower); TTA was about the same as `flac -8`. zstd `-19` saves 0.15% on the FLAC
files.

The tables were trained on every second file of the IA, hi-res and synthetic sets (their
shape is nearly universal once scaled; 101 contexts × 72 tokens).

## Usage

```sh
cargo build --release

flr bench [-r FMT] [-t THREADS] [-n ITERS] [-v] FILE|DIR...  # formula and .aqr round trip check, size, speed, stream sizes (-t default 1)
flr info FILE|DIR...
flr fuzz [-r FMT] [-t THREADS] [-n N] [-s SEED] FILE|DIR...  # mutated inputs must round-trip, the same with threads
train [-a] OUT FILE|DIR...           # trains the built-in tables (model/builtin.bin)
```

`-r FMT` takes the input as headerless PCM: sign, bits, endianness, channels (`s16le:2`).

```rust
use aqr_flac::formula;
use aqr_formula::Apply;
let f = formula::encode(&input, 8).unwrap();   // None where the input is not coded
let out = f.eval(&|a: &Apply, inputs: &[&[u8]]| {
    formula::decode(&a.params, inputs, a.len, 8).map_err(|e| aqr_formula::Error::new(e.0))
})?;
assert_eq!(out, input);
```

## Layout

| File | Contents |
|---|---|
| `src/flac.rs` | FLAC metadata, frame headers, subframes: exact parsing (CRC-checked) and writing; Rice coding; sample reconstruction |
| `src/model.rs` | The residual model: sign-sign LMS stage (NEON), scale contexts, token / raw bit coding |
| `src/residual.rs` | The pipeline shared by FLAC and PCM: per-frame passes, state across frames, built-in tables, subframe parameter serialisation |
| `src/codec.rs` | FLAC files: segmentation into frames and junk, header prediction, streams |
| `src/pcm.rs` | WAV / AIFF / raw PCM: format detection, block LPC encoder, decoding |
| `src/formula.rs` | The module `flac-recons` (specified in `FORMAT.md`): choice of the kind, options, the streams as formula inputs |
| `src/par.rs` | Pipelines for more than one thread: batches, stages, pool, reordering |
| `../aqr-common` | Alias-table rANS (`rans64`, re-exported as `rans`), bit readers and writers (`msb`, as `bits`), LEB128 |
| `src/bin/train.rs` | Training of the built-in tables |
| `tests/threads.rs` | The same formula for 1, 2, 3 and 8 threads, threaded round trips (also on inputs of many batches) |
| `tests/formula.rs`, `tests/data` | Round trips of small synthetic files (8–32 bit, 1–6 channels, several encoders, lax streams, silence, ID3 tags, junk, cut files, WAV / AIFF / AIFC, raw PCM), corrupted inputs of the module |
| `examples/` | Benchmarks and analysis: `hot`, `dhot` (profiling loops), `lmsbench`, `entbench` (cycles per residual of each pass), `rt` (parse / write round trip), `breakdown` (where FLAC's bits go), `exp2` (model experiments with ideal code lengths) |
| `tools/` | `gen_testdata.sh`, `gen_synth.sh` (synthetic corpus from MP3s), `profile.py` (samply + atos) |

## Design

### 1. Decomposition (FLAC)

- **Segmentation**: everything before the first frame (an ID3v2 tag, `fLaC`, metadata
  blocks, pictures) is kept as is. Then frames are parsed one after the other; a frame must
  parse completely, have zero padding bits and a matching CRC-16. Anything else is junk up
  to the next position where a frame parses (found through `0xFF` bytes), kept verbatim.
- **Frames** are taken apart into header bytes, subframe parameters (type, wasted bits,
  warm-up samples, LPC precision / shift / coefficients, residual coding method,
  partition order, Rice parameters or escape sizes) and residuals. Rice codes are unique, so
  writing the residuals with the same parameters reproduces the bits; CRC-8 and CRC-16 are
  recomputed.
- **Headers** are predicted from the previous one (frame or sample number advanced); a
  match costs one byte before the archive's zstd. Subframe parameters and stored samples go
  to small byte streams (0.5% of the output on CD material).
- **Residuals** (99% of a FLAC file) go through the model.

### 2. Residual model

FLAC's predictor stays: its residuals are the input. A second, adaptive stage is applied
in the residual domain, so neither direction ever reconstructs samples:

- **LMS stage**: sign-sign LMS over the last 32 residuals of the channel, weights in 1/4096
  (i16), step 4, history clamped to i16 after a shift that follows the scale of the
  residuals (for 24-bit material). The weight update is applied two samples late (delayed
  LMS), which costs 0.02 pp but takes the update out of the dependency chain.
- **Entropy coding** of the error `e`: zigzag value `u`; a running mean of `u` (decay 1/16;
  the newest value enters one sample late) gives a scale context (two per octave) and a
  shift `s` (about two bits less than the mean); `u >> s` is a token (0–15 directly, then
  two per bit length) coded with rANS under static tables by (scale, first channel or not);
  the escape mantissa and the low `s` bits are raw bits. The bits read depend only on the
  token and the context, not on the bits themselves.
- Gains on real CD material, by part: about 1.2 pp from the entropy coding (adaptive scale
  against FLAC's partitioned Rice parameters), about 2.3 pp from the LMS stage.

### 3. PCM

Blocks of 4096 samples become FLAC-like frames: stereo decorrelation (left / side,
side / right, mid / side) chosen by the sum of second differences, wasted bits, constant
blocks, and per channel a quantised LPC predictor (Tukey-windowed autocorrelation,
Levinson–Durbin, 14-bit coefficients). The order (up to 16) is chosen per block from the
Levinson prediction errors against four times the cost of the extra coefficients and
warm-up samples; the LMS stage takes over much of what a higher order would gain. The
residuals then go through the same model, and decoding reconstructs the samples with
FLAC's synthesis.

### 4. Speed

- **Separate passes per frame**: Rice decoding, the LMS stage, the token / raw bit coding
  and Rice encoding each run over the whole frame, so that each loop is small and its
  dependency chains are short.
- **Rice decoding** takes up to three codes from each 64-bit window without branches (the
  second and third are kept if they fit).
- **LMS**: two channels run in one loop (independent chains); NEON with four independent
  accumulators (LLVM otherwise folds the products into one serial multiply-accumulate chain);
  the newest sample's term is added in scalar code, so decoding's chain from one sample to the
  next is a scalar multiply-add; delayed weight updates. About 11 cycles per residual each
  way.
- **Entropy coding**: rANS with flat alias tables (every context at a fixed stride, one
  load per symbol), bounds checks hoisted to a per-frame check, raw bits in two streams by
  channel parity (two independent decode chains), branchless bit writers. The per-residual
  work is a trait method rather than a closure, so that it is always inlined and the coder
  state stays in registers (a non-inlined closure kept it in memory: −25%). About 12 cycles
  per residual to decode, 7 + 4 to encode.

### Threads

`-t N` (`Options::threads` of `formula::encode_with`, the `threads` of `formula::decode`): the
formula is the same whatever the number of threads, and one thread runs the serial code. The residual
model carries state across frames (LMS weights and history, scale means, the rANS symbol
order, the raw bit writers), so it is not split across the file; it is split by stage.
Frames go between threads in batches of about 65,536 samples (at most 64 items), through
bounded channels; a fixed number of batches circulates, so memory stays bounded.

- **FLAC compression**: parsing (CRC checks, Rice decoding, header prediction, subframe
  parameters) on the calling thread → LMS stage → coding, on two threads. The LMS stage
  needs the scale means from before each frame; it keeps them itself with the same
  recurrence as the coder (a few operations per residual). rANS encoding, at the end,
  stays serial.
- **PCM compression**: a pool makes the frames of the blocks ahead (stereo choice, LPC,
  residuals); one thread takes them in order (subframe parameters, LMS stage), the calling
  thread codes them.
- **Decompression**: subframe parameters → error decoding (rANS and raw bits, which do not
  depend on the LMS stage) → LMS stage, each on a thread, then a pool: FLAC frames are
  written (escape checks, Rice coding, CRCs) into per-batch buffers that the calling
  thread appends in order; PCM samples are reconstructed straight into their place in the
  output.
- Errors travel down the pipeline in frame order, so a damaged file gives the same error
  as with one thread; a frame whose long Rice codes exceed a small budget is written again
  in order with the exact one.
- A panic on any thread reaches the caller (pool threads forward panics; the other threads
  see their channels close and stop), where `formula::encode_with` callers catch it.

### 5. Formula module

[FORMAT.md](FORMAT.md) specifies the module in full. Params: one byte, 0 for FLAC and 1 for
PCM (WAV, AIFF and raw samples alike). Inputs: nine slots, the 6 byte streams (FLAC: prefix,
layout, junk, headers, subframes, warm-up samples; PCM: header, format and length, trailing
bytes, stereo modes, subframes, samples), the rANS words (`u32le`) and raw bit streams 0
and 1. The output length is the formula's; the archive carries the checksum and compresses
the slots.

- `formula::encode` gives no formula for what it does not code, and for what makes the
  coder panic.
- `formula::decode` bounds every size it reads; corrupted input gives an error rather than
  a crash, a huge allocation or a long loop (Rice codes of garbage residuals can be 2^32
  bits long, so frame writing has a byte budget).

## Verification

- `flr bench`: every file's formula evaluates to the input, and so does its `.aqr` archive.
- `flr fuzz`: bit flips, truncation, deletion and insertion; the formula, if there is
  one, must evaluate back without panics, the same with threads (1170 mutated inputs of the test data and corpus files, also as raw PCM).
- `cargo test --release`: bit I/O, rANS, `tests/formula.rs` and `tests/threads.rs`.

## Tried and dropped

Ratios are of the FLAC residual bits on 8 IA files (`examples/exp2.rs`, ideal adaptive code
lengths), unless noted.

| Attempt | Outcome |
|---|---|
| Re-predicting in the sample domain (order-1 filter + LMS 32 + 16 on decoded mid / side) | −3.0% against −3.6% for LMS on FLAC's residuals; and it needs synthesis both ways |
| FLAC's Rice parameter as (part of) the context | No gain over the running mean |
| Two-speed scale contexts, the other channel's magnitude as context | No gain |
| LMS cascades (32 + 16, 256 + 16), float NLMS | At most 0.06 pp better than one 32-tap sign-sign stage |
| 16 / 24 taps | −0.2 / −0.1 pp |
| Cross-channel taps | < 0.1 pp |
| Predicting from samples up to t−2 (to break the decode chain) | −1 pp; the newest term in scalar code does it for free |
| Sign + magnitude raw bits | Same ratio as zigzag, but the decoder's bit position depended on the bits read |
| Branchless escape decoding | Slower: the branch is well predicted and lets the CPU run ahead |
| PCM: fixed LPC orders 2–32 | Order 4 was best on real recordings, 8 on low-passed material; higher orders lose to their coefficient cost once the LMS stage follows |

## Open issues and limitations

- **Side information**: warm-up samples and LPC coefficients are only byte streams (0.5% of the
  output on CD material); warm-up samples could be predicted from the previous frame, but
  that needs the samples, which the FLAC path never reconstructs.
- **Encoder variety**: tested with libFLAC 1.1.0–1.5.0 (all levels, lax streams) and FFmpeg's
  encoder. Variable block size streams (flake, some FFmpeg settings) are handled by the
  header prediction but were not available for testing.
- **Not covered** (not coded, or the containing bytes kept as junk): FLAC in Ogg, float WAV,
  more than 8 channels, compressed AIFC.
- **Memory**: the whole input, plus 4 bytes per residual for the rANS symbols.
- MD5 signatures are not checked (they are kept with the metadata).
