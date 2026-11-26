# aqr-flac

A lossless FLAC recompressor: FLAC files are coded smaller and restored bit for bit, as the
module `flac-recons` of entry formulas (`aqr_flac::formula`, archived by aqr-compress).
The frames are parsed exactly here; their residuals go through aqr-raw's audio model
([`aqr_raw::audio`](../aqr-raw)), which also codes WAV, AIFF / AIFC and headerless PCM
(aqr-raw's `pcm-recons`; `aqr-flac` benches those too). Speed comes first: the target is
100 MB/s of input, single-threaded on an Apple M4, in both directions. [FORMAT.md](FORMAT.md)
specifies the module and [its design notes](FORMAT.md#design-notes) say how it works.

```sh
cargo build --release --example aqr-flac   # target/release/examples/aqr-flac

aqr-flac bench [-r FMT] [-t THREADS] [-n ITERS] [-v] FILE|DIR...  # formula and .aqr round trip check, size, speed, stream sizes (-t default 1); WAV / AIFF through aqr-raw
aqr-flac info FILE|DIR...
aqr-flac fuzz [-r FMT] [-t THREADS] [-n N] [-s SEED] FILE|DIR...  # mutated inputs must round-trip, the same with threads
aqr-flac-train [-a] OUT FILE|DIR...       # trains the built-in tables (aqr-raw's model/audio.bin), on FLAC, WAV and AIFF
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

## What is taken

FLAC files (any encoder: libFLAC 1.1.0-1.5.0 at all levels, lax streams, FFmpeg's; 8-32 bit,
1-8 channels, variable block sizes by header prediction). Frames that do not parse with a
matching CRC, tags and junk are kept verbatim. Not taken: FLAC in Ogg (aqr-ogg keeps its
layout only), more than 8 channels.

## Results

Each file archived at level `mid` with its round-trip check, then extracted, through
`aqr-show` (see the [file formats overview](../../docs/file-formats.md#how-it-was-measured)),
on an Apple M4, one thread, 2026-10-23 at `2abe944`. Ratios are tentative: the decoder modules
were not in the archives then, and the synthetic set's WAV files are aqr-raw's now, with the
same streams. Measured again with the modules in the archives on 2026-11-01
(`docs/file-formats.md`, 25 files, 450 MB): 0.946-0.948 of the input (`zstd -3` 0.999), the
resource 2.4 KB.

| Set | Files | MB | Size / original | `zstd -3` | Compressing | Decompressing |
|---|---|---|---|---|---|---|
| ia | 13 | 334.3 | 0.970 | 1.000 | 63.4 MB/s | 159.2 MB/s |
| synth | 15 | 66.8 | 0.821 | 0.992 | 77.3 MB/s | 198.4 MB/s |
| all | 28 | 401 | **0.945** | 0.999 | **65 MB/s** | **165 MB/s** |

Speed per MB falls with the bits per sample (about 30 cycles per sample each way). Memory is
9 MB + 11.5x the file (2026-10-24, `da98d11`). `zstd -19` saves 0.15% on FLAC files.

## Tests

- `tests/formula.rs` (`tests/data`): round trips of small synthetic files (8-32 bit, 1-6
  channels, several encoders, lax streams, silence, ID3 tags, junk, cut files), corrupted
  inputs of the module.
- `tests/threads.rs`: the same formula for 1, 2, 3 and 8 threads, threaded round trips (also
  on inputs of many batches).
- aqr-raw's own tests cover the model and PCM. `aqr-flac bench` checks every file's formula
  and `.aqr` archive; `aqr-flac fuzz` (bit flips, truncation, deletion, insertion; also with
  threads) passed on 1170 mutated inputs of the test data and corpus files.
- `tools/gen_testdata.sh` (also aqr-raw's WAV and AIFF test files), `tools/gen_synth.sh`
  (synthetic corpus from MP3s).

## Left

- **Side information.** Warm-up samples and LPC coefficients are only byte streams (0.5% of
  the output on CD material); predicting warm-up from the previous frame needs samples, which
  the FLAC path never reconstructs.
- **FLAC in Ogg.** The packets are FLAC's metadata blocks and frames, which aqr-flac would
  take once aqr-ogg joins them into a FLAC stream.
- **Variable block size streams** (flake, some FFmpeg settings) are handled by the header
  prediction but were not available for testing.
- **Memory.** The encoder holds the whole input, plus 4 bytes per residual for the rANS
  symbols; the module's own limit is a plan independent of file length.
