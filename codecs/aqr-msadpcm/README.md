# aqr-msadpcm

Microsoft ADPCM blocks (`WAVE_FORMAT_ADPCM`: of WAV files, through aqr-raw, and of XACT's wave
banks, through aqr-bundle's `xact-xwb`) as a record of their headers and nibbles, and back bit
for bit: the module `msadpcm-recons`. [FORMAT.md](FORMAT.md) specifies it and how it works
(aqr-ima's kind of code: adaptive bits in contexts of the decoder's state).

```sh
cargo run --release -p aqr-msadpcm --example aqr-msadpcm-check -- FILE.xwb...   # record against blocks, speed
```

## What is taken

Any bytes of whole blocks.

## Results

2026-11-13, Windows, one thread (`aqr-msadpcm-check`):

| Input | Record / blocks | `zstd -19` |
|---|---|---|
| A Virus Named TOM's music (XACT, stereo, 140-byte blocks), 73.10 MB | 0.786 | 0.822 (with the effects) |
| its sound effects, 5.30 MB | 0.527 | |

Compressing 24 MB/s, decompressing 19 MB/s. Through `aqr` (16 threads), the five wave banks of
three games (A Virus Named TOM, Stardew Valley, EDGE; 581 MB) come to 438.05 -> 383.19 MB
(-12.5%), 3.8 -> 24.3 s.

## Tests

- `cargo rt -p aqr-msadpcm`: `tests/codec.rs` (an encoder of the tests' own, any bytes, layouts,
  records cut short) and `tests/edge.rs` (8 channels, every predictor index, limits, records
  that lie about the length, damaged records).
- aqr-raw's `tests/msadpcm.rs` (ffmpeg's `adpcm_ms` WAV files); aqr-bundle's
  `tests/packs_xact_xwb.rs` (a bank's ADPCM waves).

## Left

- Nothing known.
