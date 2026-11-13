# aqr-msadpcm

Microsoft ADPCM blocks (`WAVE_FORMAT_ADPCM`: of WAV files, through aqr-raw, and of XACT's wave
banks, through aqr-bundle's `xact-xwb`) as a record of their headers and nibbles, and back bit
for bit: the module `msadpcm-recons`; `FORMAT.md` specifies it.

The record is aqr-ima's kind of code: a binary arithmetic code of adaptive bits in contexts of
the decoder's state.

- A header's predictor is coded as the channel's in the block before, or its 8 bits.
- Its delta is coded against the channel's at the end of the block before, and its two samples
  against the samples before them, extrapolated.
- Each nibble is coded by a tree of 4 bits, in a context of the delta's magnitude, the channel's
  nibble before, and the other channel's nibble of the same sample.

Any bytes of whole blocks come back.

```sh
cargo run --release -p aqr-msadpcm --example aqr-msadpcm-check -- FILE.xwb...   # record against blocks, speed
```

## How it does

Measured 2026-11-13 on Windows, one thread (`aqr-msadpcm-check`):

| | blocks | record | zstd -19 |
|---|---|---|---|
| A Virus Named TOM's music (XACT, stereo, 140-byte blocks) | 73.10 MB | 0.786 | 0.822 (with the effects) |
| its sound effects | 5.30 MB | 0.527 | |

It encodes at 24 MB/s and decodes at 19 MB/s. Through `aqr` (16 threads), the five wave banks
of three games (A Virus Named TOM, Stardew Valley, EDGE; 581 MB) come to 438.05 → 383.19 MB
(−12.5%), 3.8 → 24.3 s.

A context of the samples' slope (their difference in deltas) took nothing off.

## Tests

`cargo rt -p aqr-msadpcm` (an encoder of the tests' own, any bytes, layouts, records cut short);
aqr-raw's `tests/msadpcm.rs` (ffmpeg's `adpcm_ms` WAV files); aqr-bundle's
`tests/packs_xact_xwb.rs` (a bank's ADPCM waves).
