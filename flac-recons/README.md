# flac-recons (`flr`)

A lossless FLAC recompressor: FLAC files are stored smaller and restored bit for bit.
WAV, AIFF / AIFC and headerless PCM go through the same model and come back byte for byte.
Speed comes first: the target is 100 MB/s of input, single-threaded on an Apple M4, in both
directions. Compression ratio is pursued only where it does not cost that.

## Results

Apple M4, one thread, `flr bench -n 2` (best of two runs per file):

| Set | Files | Input | Output / input | Compress | Decompress |
|---|---|---|---|---|---|
| FLAC, Internet Archive live recordings (16/44.1, libFLAC 1.1.x) | 94 | 2.31 GB | **0.9620** | **142.7 MB/s** | **157.3 MB/s** |
| FLAC, Internet Archive 24-bit (44.1–96 kHz) | 13 | 363 MB | 0.9705 | 218.0 MB/s | 241.0 MB/s |
| FLAC, synthetic (decoded MP3s, 12 encoder settings) | 120 | 457 MB | 0.9428 | 175.7 MB/s | 191.1 MB/s |
| WAV, the first 8 IA tracks decoded | 8 | 128 MB | 0.4511 | 185.1 MB/s | 192.3 MB/s |
| WAV, synthetic (16 and 24 bit) | 20 | 223 MB | 0.4194 | 208.1 MB/s | 188.6 MB/s |

Every file round-trips (`bench` also checks the coded path when the container stores the
file). Speed per MB falls with the bits per sample: the cost is about 30 cycles per sample
each way, so the most compressible 16-bit FLAC files (about 6 bits per sample) are coded at
100–110 MB/s.

### Comparison

The 8 WAV files above (128.1 MB):

| Tool | Output / WAV |
|---|---|
| `flr` on the WAV | **0.4511** |
| `flr` on the `flac -8` files | 0.4510 (0.9835 of the FLAC) |
| flac 1.5.0 `-8` | 0.4585 |
| zstd 1.5 `-19` | 0.7522 |

FFmpeg's WavPack encoder at its highest level was about 1–2% smaller than `flac -8` on these files (and far slower);
TTA was about the same as `flac -8`. zstd `-19` saves 0.15% on the FLAC files.

The tables were trained on every second file of the IA, hi-res and synthetic sets (their
shape is nearly universal once scaled; 101 contexts × 72 tokens).

## Usage

```sh
cargo build --release

flr c in.flac out.flr                # compress (FLAC, WAV, AIFF; anything else is stored)
flr c -r s16le:2 in.raw out.flr      # headerless PCM: sign, bits, endianness, channels
flr d out.flr back.flac              # decompress
flr c -t 4 in.flac out.flr           # -t THREADS (c, d: default the available cores)
flr bench [-r FMT] [-t THREADS] [-n ITERS] [-v] FILE|DIR...  # round trip check, ratio, speed, stream sizes (-t default 1)
flr info FILE|DIR...
flr fuzz [-r FMT] [-t THREADS] [-n N] [-s SEED] FILE|DIR...  # mutated inputs must round-trip, the same with threads
train [-a] OUT FILE|DIR...           # trains the built-in tables (model/builtin.bin)
```

```rust
let packed = flac_recons::compress(&input);            // never fails: stores what it cannot code
assert_eq!(flac_recons::decompress(&packed)?, input);  // checked against an xxh3 of the original
let opt = flac_recons::Options { threads: 8, ..Default::default() };
assert_eq!(flac_recons::compress_opt(&input, &opt), packed);       // the same bytes
assert_eq!(flac_recons::decompress_threads(&packed, 8)?, input);
```

## Layout

| File | Contents |
|---|---|
| `src/flac.rs` | FLAC metadata, frame headers, subframes: exact parsing (CRC-checked) and writing; Rice coding; sample reconstruction |
| `src/model.rs` | The residual model: sign-sign LMS stage (NEON), scale contexts, token / raw bit coding |
| `src/residual.rs` | The pipeline shared by FLAC and PCM: per-frame passes, state across frames, built-in tables, subframe parameter serialisation |
| `src/codec.rs` | FLAC files: segmentation into frames and junk, header prediction, streams |
| `src/pcm.rs` | WAV / AIFF / raw PCM: format detection, block LPC encoder, decoding |
| `src/container.rs` | Container format (specified in `FORMAT.md`), stored fallback, checksum, options |
| `src/par.rs` | Pipelines for more than one thread: batches, stages, pool, reordering |
| `../recons-common` | Alias-table rANS (`rans64`, re-exported as `rans`), bit readers and writers (`msb`, as `bits`), LEB128 |
| `src/bin/train.rs` | Training of the built-in tables |
| `tests/threads.rs` | The same container bytes for 1, 2, 3 and 8 threads, threaded round trips, the same result decompressing damaged containers (also on inputs of many batches) |
| `tests/regress.rs`, `tests/data` | Round trips of small synthetic files (8–32 bit, 1–6 channels, several encoders, lax streams, silence, ID3 tags, junk, cut files, WAV / AIFF / AIFC, raw PCM), corrupted containers |
| `examples/` | Benchmarks and analysis: `hot`, `dhot` (profiling loops), `lmsbench`, `entbench` (cycles per residual of each pass), `rt` (parse / write round trip), `breakdown` (where FLAC's bits go), `exp2` (model experiments with ideal code lengths), `corrupt` |
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
  match costs one byte before zstd. Subframe parameters and stored samples go to zstd'd
  byte streams (0.5% of the output on CD material).
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

`-t N` (`Options::threads`, `compress_opt`, `decompress_threads`): the output is the same
bytes whatever the number of threads, and one thread runs the serial code. The residual
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
  in order with the exact one. Side streams are zstd'd / unzstd'd on threads of their own,
  and the checksum of the input is taken on another thread while compressing.
- `compress_opt` never fails: a panic on any thread still falls back to stored (pool
  threads forward panics; the other threads see their channels close and stop).

### 5. Container

[FORMAT.md](FORMAT.md) specifies the format in full.

```text
magic(6) version(u16 LE)
  magic: 69047e7f569c (base64 aQR+f1ac) FLAC, 69047ec1abf7 (aQR+wav3) WAV / AIFF,
         69047ea5c9bf (aQR+pcm/) raw PCM or anything not recognised
version 0 (stored): the input verbatim
version 1 (coded):
  u64 LE length, u64 LE xxh3 of the original
  6 byte streams (FLAC: prefix, layout, junk, headers, subframes, warm-up samples;
                  PCM: header, format and length, trailing bytes, stereo modes, subframes, samples)
                 each varint(len << 1 | zstd) [varint raw len] data; zstd level 3
  varint rANS word count, rANS words, varint length of raw stream 0, raw streams 0 and 1
```

- `compress` never fails: panics and anything larger than the input fall back to stored.
- Decompression checks the length and the xxh3 and bounds every size it reads; corrupted
  input gives an error rather than a crash, a huge allocation or a long loop (Rice codes of
  garbage residuals can be 2^32 bits long, so frame writing has a byte budget).

## Verification

- `flr bench`: every file round-trips through the container and through the coded path.
- `flr fuzz`: bit flips, truncation, deletion and insertion; both paths must round-trip
  without panics (1170 mutated inputs of the test data and corpus files, also as raw PCM).
- `cargo test --release`: bit I/O, rANS, `tests/regress.rs` and `tests/threads.rs`.

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

- **Side information**: warm-up samples and LPC coefficients are only zstd'd (0.5% of the
  output on CD material); warm-up samples could be predicted from the previous frame, but
  that needs the samples, which the FLAC path never reconstructs.
- **Encoder variety**: tested with libFLAC 1.1.0–1.5.0 (all levels, lax streams) and FFmpeg's
  encoder. Variable block size streams (flake, some FFmpeg settings) are handled by the
  header prediction but were not available for testing.
- **Not covered** (stored, or the containing bytes kept as junk): FLAC in Ogg, float WAV,
  more than 8 channels, compressed AIFC.
- **Memory**: the whole input, plus 4 bytes per residual for the rANS symbols.
- MD5 signatures are not checked (they are kept with the metadata).
