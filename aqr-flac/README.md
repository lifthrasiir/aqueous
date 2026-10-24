# aqr-flac (`flr`)

A lossless FLAC recompressor: FLAC files are coded smaller and restored bit for bit, as the
module `flac-recons` of entry formulas (`aqr_flac::formula`, archived by aqr-compress).
The frames are parsed exactly here; their residuals go through aqr-raw's audio model
([`aqr_raw::audio`](../aqr-raw)), which also codes WAV, AIFF / AIFC and headerless PCM
(aqr-raw's `pcm-recons`; `flr` benches those too).
Speed comes first: the target is 100 MB/s of input, single-threaded on an Apple M4, in both
directions. Compression ratio is pursued only where it does not cost that.

## Results

Measured on 2026-10-23 at `2abe944` on an Apple M4, one thread, through `aqr-show` (see the
[top-level README](../README.md#how-it-was-measured)): each file archived at level `mid` with
its round-trip check, then extracted. Ratios are tentative, since the decoder modules are not
in the archives yet. Memory is the peak RSS of that process (input, archive and output
included) over the input size.

| set | files | MB | ratio | zstd -3 | compress | decompress |
|---|---|---|---|---|---|---|
| ia | 13 | 334.3 | 0.970 | 1.000 | 63.4 MB/s | 159.2 MB/s |
| synth | 15 | 66.8 | 0.821 | 0.992 | 77.3 MB/s | 198.4 MB/s |
| all | 28 | 401 | **0.945** | 0.999 | **65 MB/s** | **165 MB/s** |

These were measured before the PCM path moved to aqr-raw: the WAV files of the synthetic set
are aqr-raw's now, with the same streams (see aqr-raw's README).

The resource (the built-in tables) costs about 1.6 KB per archive. Memory is 12.5× the file
(median of files of 8 MB or more).

The cost is about 30 cycles per sample each way, so speed per MB falls with the bits per
sample.

FFmpeg's WavPack encoder at its highest level was about 1–2% smaller than `flac -8` on
8 IA tracks decoded to WAV (and far slower); TTA was about the same as `flac -8`. zstd `-19` saves 0.15% on the FLAC
files.

The tables were trained on every second file of the IA, hi-res and synthetic sets (their
shape is nearly universal once scaled; 101 contexts × 72 tokens).

## Usage

```sh
cargo build --release

flr bench [-r FMT] [-t THREADS] [-n ITERS] [-v] FILE|DIR...  # formula and .aqr round trip check, size, speed, stream sizes (-t default 1); WAV / AIFF through aqr-raw
flr info FILE|DIR...
flr fuzz [-r FMT] [-t THREADS] [-n N] [-s SEED] FILE|DIR...  # mutated inputs must round-trip, the same with threads
flr-train [-a] OUT FILE|DIR...       # trains the built-in tables (aqr-raw's model/audio.bin), on FLAC, WAV and AIFF
```

`-r FMT` takes the input as headerless PCM (aqr-raw): sign, bits, endianness, channels
(`s16le:2`).

```rust
use aqr_flac::formula;
use aqr_formula::Apply;
let f = formula::encode(&input, 8).unwrap();   // None where the input is not FLAC
let out = f.eval(&|a: &Apply, inputs: &[&[u8]]| {
    let res: Vec<&[u8]> = a.resources.iter().map(|r| &r[..]).collect();
    formula::decode(&a.params, &res, inputs, a.len, 8).map_err(|e| aqr_formula::Error::new(e.0))
})?;
assert_eq!(out, input);
```

## Layout

| File | Contents |
|---|---|
| `src/flac.rs` | FLAC metadata, frame headers, subframes: exact parsing (CRC-checked) and writing; Rice coding; sample reconstruction |
| `src/codec.rs` | FLAC files: segmentation into frames and junk, header prediction, streams |
| `src/formula.rs` | The module `flac-recons` (specified in `FORMAT.md`): the streams as formula inputs |
| `../aqr-raw` | Frames (`audio::frame`), the residual model (`audio::model`, `audio::residual`), the threaded pipelines (`audio::par`), the built-in tables; WAV, AIFF and raw PCM |
| `../aqr-common` | Alias-table rANS (`rans64`, re-exported as `rans`), bit readers and writers (`msb`, as `bits`), LEB128 |
| `src/bin/train.rs` | Training of the built-in tables |
| `tests/threads.rs` | The same formula for 1, 2, 3 and 8 threads, threaded round trips (also on inputs of many batches) |
| `tests/formula.rs`, `tests/data` | Round trips of small synthetic files (8–32 bit, 1–6 channels, several encoders, lax streams, silence, ID3 tags, junk, cut files), corrupted inputs of the module |
| `examples/` | Benchmarks and analysis: `hot`, `dhot` (profiling loops), `lmsbench`, `entbench` (cycles per residual of each pass), `rt` (parse / write round trip), `breakdown` (where FLAC's bits go), `exp2` (model experiments with ideal code lengths) |
| `tools/` | `gen_testdata.sh` (also aqr-raw's WAV and AIFF test files), `gen_synth.sh` (synthetic corpus from MP3s), `profile.py` (samply + atos) |

## Design

### 1. Decomposition (FLAC)

- Frames must parse completely with a matching CRC-16 to be taken apart; anything else is
  junk, kept verbatim. Rice codes are unique, so writing the residuals with the same
  parameters reproduces the bits; the CRCs are recomputed.
- Headers are predicted from the previous one; a match costs one byte before the archive's
  zstd. Subframe parameters and stored samples go to small byte streams (0.5% of the output
  on CD material).
- Residuals (99% of a FLAC file) go through the model.

### 2. Residual model

FLAC's predictor stays: its residuals are the input of aqr-raw's residual model (an adaptive
sign-sign LMS stage in the residual domain, then a token under static rANS tables and raw
bits; see aqr-raw's README), so neither direction ever reconstructs samples. On real CD
material about 1.2 pp come from the entropy coding (adaptive scale against FLAC's
partitioned Rice parameters) and about 2.3 pp from the LMS stage.

### 3. Speed

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

`-t N` (the `threads` of `formula::encode` and `formula::decode`): the
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
- **Decompression**: subframe parameters → error decoding (rANS and raw bits, which do not
  depend on the LMS stage) → LMS stage, each on a thread, then a pool: FLAC frames are
  written (escape checks, Rice coding, CRCs) into per-batch buffers that the calling
  thread appends in order. (aqr-raw's PCM runs the same stages.)
- Errors travel down the pipeline in frame order, so a damaged file gives the same error
  as with one thread; a frame whose long Rice codes exceed a small budget is written again
  in order with the exact one.
- A panic on any thread reaches the caller (pool threads forward panics; the other threads
  see their channels close and stop), where `formula::encode` catches it.

### 4. Formula module

The format is specified in [FORMAT.md](FORMAT.md).

- `formula::encode` gives no formula for what it does not code, and for what makes the
  coder panic.
- `formula::decode` bounds every size it reads; corrupted input gives an error rather than
  a crash, a huge allocation or a long loop (Rice codes of garbage residuals can be 2^32
  bits long, so frame writing has a byte budget).

## Verification

- `flr bench`: every file's formula evaluates to the input, and so does its `.aqr` archive.
- `flr fuzz`: bit flips, truncation, deletion and insertion; the formula, if there is
  one, must evaluate back without panics, the same with threads (1170 mutated inputs of the test data and corpus files, also as raw PCM, when that was aqr-flac's).
- `cargo test --release`: `tests/formula.rs` and `tests/threads.rs` (and aqr-raw's own tests for the model and PCM).

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
- **Not covered** (not coded, or the containing bytes kept as junk): FLAC in Ogg, more than
  8 channels.
- **Memory**: the whole input, plus 4 bytes per residual for the rANS symbols.
- MD5 signatures are not checked (they are kept with the metadata).
