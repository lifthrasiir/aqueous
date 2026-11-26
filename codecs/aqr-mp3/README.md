# aqr-mp3

A lossless MP3 recompressor: MPEG audio Layer III files (MPEG-1, MPEG-2 and MPEG-2.5,
including free format) are stored smaller and restored bit for bit, in the family of
packMP3. Speed comes first: the target is 100 MB/s of MP3 file single-threaded on an
Apple M4, both ways; compression ratio is pursued only where it does not cost that.
[FORMAT.md](FORMAT.md) specifies the module `mp3-recons` and how it works.

```sh
cargo build --release --example aqr-mp3   # target/release/examples/aqr-mp3

aqr-mp3 bench [-n ITERS] [-t N] [-v] FILE|DIR... # round trip check, .aqr ratio, throughput, per-stream sizes
aqr-mp3 info FILE|DIR...                # frames, junk, tail / raw / gap bits, symbol counts
aqr-mp3 fuzz [-n N] [-s SEED] [-t N] FILE|DIR... # mutated inputs (flips, cuts, deletions, insertions)
cargo run --release --example aqr-mp3-memprobe FILE... # peak heap of encoding and decoding (LOG=MB: allocations)
train [-a] [-k K] OUT FILE|DIR...    # trains the built-in tables, K of them (500), on every file with -a, else every second
```

`-t N`: threads, default 1 (`fuzz` then also checks that the formula equals the one-thread
formula). The comparison with packMP3, `zstd -19` and `xz -9` is `tools/compare.sh PACKMP3
WORKDIR DIR...`.

```rust
let f = aqr_mp3::formula::encode(&mp3, 8);         // None: not worth a formula; the same for any number of threads
// f.eval(..) with aqr_mp3::formula::decode as the module gives mp3 back
```

## What is taken

Any input is accepted. MP3 frames of Layer III are recompressed; whatever is not (junk, tags,
main data that cannot be reproduced) is kept verbatim in a stream. Not taken: Layer I/II, and
MP3 inside other containers (MP4, MKV, WAV), which are stored as junk.

## Results

402 files of FMA (401 MB, about 1 MB each), archived at level `mid` through `aqr-show` with its
round-trip check, on an Apple M4, one thread, 2026-10-25 (see the
[file formats overview](../../docs/file-formats.md#how-it-was-measured)). Ratios are
tentative: the decoder modules were not in the archives then. Measured again with them in the
archives on 2026-11-01 (`docs/file-formats.md`, 394 files, 400 MB): 0.852 (shared) to 0.915 (worst)
of the input, `zstd -3` 0.968, the resource about 33 KB archived.

| Setting | Size / original | Compressing | Decompressing |
|---|---|---|---|
| One file per archive (worst) | 0.890 | | |
| Resource shared, as in an archive of many files (best) | 0.853 | 45 MB/s | 116 MB/s |
| `zstd -3` | 0.971 | | |

A single file pays for the built-in tables, the module's resource (130 KB raw, about 37 KB
archived), which every archive using the module stores once. Peak memory is about 61 MB + 6.4x
the file (input, archive and output included).

## Tests

- `tests/regress.rs` (`tests/data`): round trips of small LAME encodings (CBR, VBR, mono, CRC,
  MPEG-2, MPEG-2.5 8 kHz, free format, tags), with junk, cuts, concatenation.
- `tests/formula.rs`: the formula of the same files, large repetitions of them and non-MP3
  data; 1, 2 and 8 threads give identical formulas, round trips, the same errors for corrupted
  slots.
- `tests/part.rs`: the part module through the host's emulation against the whole decoding:
  files past the windows, junk, a gap of many pieces; damaged, cut and extended slots.
- `tests/memory.rs`: peak heap of a 16 MB file, encoding and decoding, within a few times the
  file over the tables.
- Unit tests: bit I/O, Huffman tables (a random round trip for each), rANS, reciprocals, range
  coder. `aqr-mp3 bench` and `fuzz` run on FMA (held-out set of 3489 files) and synthetic files.

## Left

- **Gap to packMP3.** About 3 pp of ratio (measured with the former container format).
  Per-file tables for the most used contexts are estimated at 0.5-0.6 pp but cost table
  construction at decompression.
- **Encoder diversity.** Tested on FMA (many encoders, clipped with `ffmpeg -acodec copy`) and
  LAME 3.100; no files from FhG, iTunes, Xing, Helix or Shine checked specifically. Oddities
  still round-trip, but may compress less.
- **Mixed blocks.** Decoded with minimp3's band layout; MPEG-2.5 8 kHz short blocks may differ
  between decoders (ffmpeg uses a different region boundary). Bits are reproduced exactly;
  only the model may be worse.
- **Memory.** About 61 MB of peak RSS whatever the file, 4 MB of it the encoder's inverse maps;
  the tables are also the resource every archive carries (about 38 KB archived). Decoding with
  more than one thread still holds the reservoir whole.
