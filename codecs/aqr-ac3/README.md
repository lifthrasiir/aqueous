# aqr-ac3

AC-3 (Dolby Digital, ATSC A/52) audio: each syncframe parsed down to its side information,
exponents and quantized mantissas, which are coded again by an adaptive context model, and written
back bit for bit. The module is `ac3-recons`; [FORMAT.md](FORMAT.md) specifies the stream and the
module, and how it works.

AC-3's syntax and its bit allocation are in the public standard, A/52 (the tables of
`src/ac3_tables.rs` are generated from it by `scripts/ac3-gen.py`, and were extracted from the
standard's text by `scripts/ac3-probe/extract_tables.py`); no other implementation's code is
used. ffmpeg served only as an oracle to compare against.

```sh
cargo run --profile release-test -p aqr-ac3 --example aqr-ac3-check -- FILE.avi [CHUNKS]   # the AVI's audio coded, decoded, compared
```

```rust
use aqr_ac3::formula;
// None where the stream does not come to less than 97% by the model.
let coded = formula::encode_with(&stream, threads).unwrap();
// the module `formula::MODULE` (`ac3-recons`) rebuilds `stream` from `coded` (class `formula::CODED`).
let back = formula::decode(&[], &[&coded], stream.len() as u64, threads).unwrap();
```

## What is taken

The input is a byte stream: the audio chunks of an AVI one after the other, or an `.ac3` file.
Every syncframe of `bsid` 8 that parses to its exact end with right checksums is coded; the
bytes between frames (zeros before the first frame, a frame cut across chunks, damage) are kept
as they are. Used by [aqr-div3](../aqr-div3/README.md) for the audio of DivX 3 AVI files (format
tag `0x2000`). Not taken: E-AC-3 and other `bsid`s, frames with `auxdatae` set or padding over
32 bits (kept as bytes).

## Results

The audio of `mari.avi` (5.1, 384 kbps, 206,306,429 bytes of 103,584 chunks), 2026-11-11: 87.6%
(87.4% in one chunk), rebuilt byte for byte; coding all of it on 4 threads took 7.7 s.

## Tests

- `tests/edge.rs`: syncframes written by the test (silent blocks, valid checksums) of every
  channel layout and frame size and their round trips, the stream's records around damaged and
  false frames, dense false syncwords, damaged or lying coded streams.
- `tests/overread.rs`: a coded stream cut short must end in an error, a table of many small
  chunks costs what they hold, the checksum is the table's and fast.

## Left

- **Other syntaxes.** No E-AC-3, no alternate bit stream syntax (`bsid` 6); other containers
  than AVI and bare `.ac3` (MKV, MP4, VOB) are not yet taken.
- **Frames with `auxdatae`** set, or padding over 32 bits, are kept as they are.
- **Contexts.** Side information and exponents are coded under contexts of the previous value
  of the same field, the mantissas under the last mantissa of the same bit allocation pointer;
  the same channel's neighbours (the previous block's exponents, the other channels' mantissas)
  would gain more.
