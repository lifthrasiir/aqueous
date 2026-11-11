# aqr-ac3

AC-3 (Dolby Digital, ATSC A/52) audio: each syncframe parsed down to its side information,
exponents and quantized mantissas, which are coded again by an adaptive context model, and written
back bit for bit. The bit allocation (A/52 7.2) is run on the exponents so that the number of bits
of every mantissa is known; the two checksums of a frame are derived, not coded. The module is
`ac3-recons`; [FORMAT.md](FORMAT.md) specifies the stream and the module.

The input is a byte stream: the audio chunks of an AVI one after the other, or an `.ac3` file. It is
cut into syncframes and **the bytes between them**: whatever is not a frame that parses to its exact
end with right checksums (the zeros before the first frame, a frame cut across chunks, a frame of
E-AC-3 or another `bsid`, damage) is kept as it is. The syntax is walked by one function over
[aqr-walk](../../crates/aqr-walk/README.md)'s `Io`, serving coding, decoding and the dry parse. The
stream is coded in chunks of about 16 MiB, each with a model of its own, so natively they are
coded and decoded at once.

AC-3's syntax and its bit allocation are in the public standard, A/52 (the tables of
`src/ac3_tables.rs` are generated from it by `scripts/ac3-gen.py`, and were extracted from the
standard's text by `scripts/ac3-probe/extract_tables.py`); no other implementation's code is
used. ffmpeg served only as an oracle to compare against.

Used by [aqr-div3](../aqr-div3/README.md) for the audio of DivX 3 AVI files (format tag `0x2000`);
other containers (MKV, MP4, VOB, bare `.ac3`) are not yet taken.

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

## Results

Measured on 2026-11-11 on the audio of `mari.avi` (5.1, 384 kbps, 206,306,429 bytes of 103,584
chunks): 180,634,109 bytes (87.6%; 87.4% in one chunk), rebuilt byte for byte. Coding all of it on
4 threads took 7.7 s.

## Left

- Only `bsid` 8 (AC-3 proper); no E-AC-3, no alternate bit stream syntax (`bsid` 6).
- Frames with `auxdatae` set, or padding over 32 bits, are kept as they are.
- Side information and exponents are coded under contexts of the previous value of the same field;
  the mantissas under the last mantissa of the same bit allocation pointer. Using the same
  channel's neighbours (the previous block's exponents, the other channels' mantissas) would gain more.
- No SIMD is used: the range coder is serial, and the bit allocation and CRC were not hot.
