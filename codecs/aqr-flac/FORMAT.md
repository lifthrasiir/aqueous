# The `flac-recons` module

This document specifies the module `flac-recons` of entry formulas (`aqr_flac::formula`),
which restores FLAC files from their coded streams. It is meant to be complete together
with aqr-raw's `FORMAT.md`, whose frames, residual model, rANS coder and built-in tables
(its §3–§5 and Appendix A) the module shares: a decoder, and an encoder whose output a
decoder accepts, can be written from the two. WAV, AIFF and headerless PCM are aqr-raw's
(`pcm-recons`, in the same document). Sections marked *(encoder)* describe the choices this implementation
makes. A decoder does not need them, but an encoder must follow them to produce the same
streams as this implementation.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [Frames, the residual model and rANS](#3-frames-the-residual-model-and-rans)
6. [FLAC streams](#6-flac-streams)
8. [Choosing the input](#8-choosing-the-input-encoder) *(encoder)*
9. [Validation](#9-validation)
10. [Appendix B: CRCs](#appendix-b-crcs)

(§4, §5, §7 and Appendix A were the residual model, rANS, PCM and the tables, which are
aqr-raw's now; the numbers are kept so that the references between sections stay.)

## 1. Conventions

- Integers are unsigned unless they are marked signed. Signed integers are two's complement.
  `>>` on a signed value is an arithmetic shift. Fixed-width arithmetic wraps unless
  stated otherwise.
- `u16le`, `u32le` and `u64le` are little-endian integers.
- **varint**: unsigned LEB128. Each byte holds 7 bits, least significant group first, and
  bit 7 is set on every byte except the last. A reader takes at most 10 bytes and drops
  bits beyond 64. If the 10th byte still has bit 7 set, the input is malformed.
- **zigzag** maps a signed value to an unsigned one: `zz(v) = (v << 1) ^ (v >> (W-1))`,
  `unzz(u) = (u >> 1) ^ -(u & 1)`, where W is the width. Byte streams use 64-bit zigzag
  (`zz64`). The residual model uses 32-bit zigzag (`zz32`).
- **Bit streams** are MSB-first: the first bit is bit 7 of byte 0. Writing an `n`-bit value
  puts its most significant bit first. A finished stream is padded with zero bits to a
  whole byte.
- `bitlen(v)` is the number of significant bits of `v > 0` (`bitlen(1) = 1`).
  `floor_log2(v) = bitlen(v) - 1`.
- **zstd** means one Zstandard frame (RFC 8878).

## 2. The module

The module is `flac-recons`, version 3 (`aqr_flac::formula`), in entry formulas
(aqr-formula). `formula::encode` makes the formula and `formula::decode` is the module.
Its output, the **original**, has the declared length `len`: a FLAC file.

**Params**: none. Any params are an error. (Version 2 took one byte, the kind: 0 for FLAC
and 1 for PCM, which is aqr-raw's `pcm-recons` now.)

**Resources**: exactly one, the frequency tables of aqr-raw's `FORMAT.md`, Appendix A (the
resource of `pcm-recons` too), checked as that document says.

**Inputs**: nine slots, always in this order, each possibly empty. Their classes are
`flac/prefix`, `flac/layout`, `flac/junk`, `flac/headers`, `flac/subframes`, `flac/warmup`,
`flac/rans`, `flac/raw0` and `flac/raw1`.

| # | Slot | Content |
|---|---|---|
| 0 | prefix | bytes before the first frame (§6.1) |
| 1 | layout | runs of frames and junk (§6.2) |
| 2 | junk | bytes between frames (§6.2) |
| 3 | frame headers | §6.3 |
| 4 | subframe parameters | `sub` of aqr-raw §3.1 (§6.4) |
| 5 | warm-up, constant and verbatim samples | `warm` of aqr-raw §3.1 (§6.4) |
| 6 | rANS words | the W words of the rANS coder (aqr-raw §5) as `u32le`, so a length that is not a multiple of 4 is an error |
| 7 | raw bit stream 0 | its bytes |
| 8 | raw bit stream 1 | its bytes |

The output length must be `len`; anything else is an error.

## 3. Frames, the residual model and rANS

The file's audio is a sequence of FLAC frames, which are the frames of aqr-raw's
`FORMAT.md` §3. Their residuals go through its residual model (§4 there) into the rANS
words (§5 there) and the two raw bit streams, one model state for the whole file. In this
document, §3, §3.1, §4 and §4.6 refer to those sections of aqr-raw's.

## 6. FLAC streams

The FLAC payload reproduces the original file byte for byte. The file splits into a prefix
(everything up to the end of the metadata), then a sequence of FLAC frames and junk.

### 6.1 Prefix (stream 0)

The prefix is the original's bytes up to the first byte after the last metadata block:

1. If the file starts with `ID3` and is at least 10 bytes long, skip an ID3v2 tag:
   `p = 10 + size + (d[5] & 0x10 ? 10 : 0)`, where `size` is the syncsafe integer in bytes
   6..9 (7 bits per byte, big-endian).
2. `fLaC` must follow at `p`.
3. Metadata blocks follow, each with a 4-byte header: bit 7 means last, bits 0..6 give the
   type, then a 24-bit big-endian length. The prefix ends after the last block.

At least one STREAMINFO block (type 0, length ≥ 18) must be present. If there are several,
the last one counts. Its sample size, `((u64be at body offset 10) >> 36 & 31) + 1`, is the
stream's `bps`, used by frame headers whose sample size code is 0.

A decoder must parse its prefix stream this way and reach exactly the stream's end.

### 6.2 Layout (stream 1) and junk (stream 2)

The rest of the file alternates between runs of frames and pieces of junk. The layout
stream is a sequence of varints, starting and ending with a run:

```text
run_0, junk_len_0, run_1, junk_len_1, …, run_K
```

```text
decode:
  loop:
    run = varint;  decode `run` frames (§6.5)
    if the layout stream is exhausted: stop
    n = varint;  copy the next n bytes of the junk stream to the output
```

Runs can be 0, for example when the file starts with junk. The junk stream is all junk
bytes concatenated.

*(encoder)* Segmentation: from the end of the prefix, at each position `p`, try to parse a
frame (§6.6). If one parses, it is the next frame and `p` moves to its end. If not, the
junk extends from `p` to the first position `q > p` holding a `0xFF` byte where a frame
parses, or to the end of the file. The layout then records the run of frames before it and
its length.

### 6.3 Frame headers (stream 3)

A FLAC frame header is: 2 bytes of sync code and blocking strategy, 1 byte of block size
and sample rate codes, 1 byte of channel assignment and sample size code, the coded
frame or sample number (1..7 bytes), an optional block size (1 or 2 bytes, §6.6), an
optional sample rate (1 or 2 bytes), then the CRC-8.

A frame header here means the header bytes from the sync code up to the CRC-8, excluding
the CRC-8 (5..15 bytes). Per frame, stream 3 holds either:

- `0x00`: the header is the prediction from the previous frame's header, or
- `L` (1..255) followed by `L` header bytes.

The prediction from the previous header `P` (none at the first frame, where the prediction
is empty and never matches):

```text
predict(P, prev_block_size):
  if len(P) < 5: return empty
  (v, n) = coded_number(P)                  # P[4 .. 4+n]
  step = (P[1] & 1) ? prev_block_size : 1   # variable block size: sample number; else frame number
  return P[0..4] ++ put_coded_number(v + step) ++ P[4+n ..]

coded_number(P):                            # P is a valid header (§6.6), so n is 1..7
  b = P[4];  n = max(leading_ones(b), 1)
  v = (n == 1) ? b : b & (0x7f >> n)
  for c in P[5 .. 4+n]:  v = v << 6 | (c & 0x3f)
  return (v, n)

put_coded_number(v):                        # v as u64
  if v < 0x80: return [v]
  n = 2;  while n < 7 and v ≥ 2^(5n+1): n += 1
  lead = (n == 7) ? 0xfe : (0xff00 >> n) & 0xff
  return [ (lead | (v >> 6(n-1))) & 0xff ] ++ [ 0x80 | ((v >> 6i) & 0x3f) for i = n-2 down to 0 ]
```

The encoder emits `0x00` exactly when the prediction equals the header. The decoder
appends the CRC-8 (Appendix B) to the header bytes and parses the result as a FLAC frame
header (§6.6, header rules), using STREAMINFO's `bps` for sample size code 0. This gives
the frame's block size, channel assignment and sample size. It then remembers the header
and block size for the next prediction. The prediction carries across junk.

### 6.4 Subframes (streams 4 and 5)

As in §3.1, with the Rice parameters.

### 6.5 Decoding a frame

1. Read the header (§6.3) and the subframe parameters (§6.4).
2. Decode the residuals of the residual channels (§4.6), `N - order` per channel.
3. For each escaped partition, every residual must fit in `b` signed bits (`b = 0`: the
   residual must be 0).
4. Write the FLAC frame:
   - the header bytes, then their CRC-8;
   - per channel, the subframe header byte `typ << 1 | (wasted > 0)` (its top bit is zero).
     If `wasted > 0`, it is followed by `wasted - 1` zero bits and a one bit;
   - constant: the value in `bits` bits. Verbatim: `N` samples in `bits` bits.
     Fixed / LPC: `order` warm-up samples in `bits` bits. LPC then adds `precision - 1` in
     4 bits, the shift in 5 bits (two's complement) and the coefficients in `precision`
     bits each;
   - fixed / LPC: `method` in 2 bits, `partition_order` in 4 bits, then each partition:
     - escaped: all ones in `4 + method` bits, `b` in 5 bits, the residuals in `b` bits
       each;
     - Rice: `k` in `4 + method` bits, then each residual as `u = zz32(r)`: `u >> k` zero
       bits, a one bit, then the low `k` bits of `u`;
   - zero bits to a byte boundary, then the CRC-16 (big-endian) of all the frame's bytes
     from the sync code.

Rice codes are unique given `k`, so the bytes are those of the original frame.

### 6.6 What counts as a frame *(encoder)*

A FLAC frame at position `p` is accepted only if all of the following hold. Anything else
is junk. A decoder's output does not depend on these rules, but the encoder's
segmentation does.

Header:

- `d[p] = 0xFF` and `d[p+1] & 0xFE = 0xF8`.
- Block size code ≠ 0, sample rate code ≠ 15, channel assignment ≤ 10, sample size
  code ≠ 3, and the reserved bit (bit 0 of byte 3) is 0.
- The sample size (code 0 means STREAMINFO's; codes 1, 2, 4, 5, 6, 7 mean 8, 12, 16, 20,
  24, 32) is in 4..32.
- The coded number is 1..7 bytes. Its first byte has 0 or 2..7 leading ones, and the
  continuation bytes match `10xxxxxx`.
- Block size: code 1 → 192, 2..5 → `576 << (code-2)`, 6 → next byte + 1,
  7 → next u16be + 1, 8..15 → `256 << (code-8)`. Sample rate code 12 adds 1 byte;
  13 and 14 add 2.
- The CRC-8 byte matches.

Subframes:

- The padding bit is 0. The type is valid (§3). If the wasted flag is set, the unary
  count has fewer than 32 leading zeros, and `wasted < full`.
- `order ≤ N`. The LPC precision field is not 15. The method is 0 or 1. `N` is divisible
  by the partition count, and `N / 2^partition_order ≥ order`.
- Every Rice-coded residual has `u < 2^32` (it fits in i32 after unzigzag).
- The frame lies within the file (padding included). The bits up to the byte boundary are
  zero, and the CRC-16 matches.

## 8. Choosing the input *(encoder)*

The input is coded if it has FLAC metadata (§6.1). Otherwise there is no formula, and the
input is not coded; nor is there one if coding fails. The formula is made whether or not
the coded form is smaller than the input.

## 9. Validation

A decoder should reject the following rather than produce output:

- Params, or a number of inputs other than nine; rANS words of a length
  that is not a multiple of 4; streams cut short (a varint or a byte stream).
- FLAC: a prefix that does not parse to its own end (§6.1); a header that is not valid
  (§6.6, header rules) or a prediction with no previous header; a subframe type outside
  §3; `wasted ≥ full` or a wasted flag with `wasted = 0`; an order larger than `N`; a
  sample out of its `bits` range; an LPC precision outside 1..15 or a shift outside
  -16..15; a coefficient out of range; method > 1; bad partitions; a Rice parameter ≥ 15
  (method 0) or ≥ 31 (method 1); an escape byte with bits 5..6 set; escaped residuals out
  of range (§6.5).
- rANS words exhausted (aqr-raw §5.2), or a raw bit stream read past its end.
- An output whose length is not `len`.

This implementation also bounds its work on corrupt input. It rejects `len > 64 × S +
2^20`, where S is the total size of the slots, and stops writing when the output would
exceed `len`. A Rice code can be up to 2^32 bits long.

## Appendix B: CRCs

- **CRC-8** (frame header): polynomial `0x07`, initial value 0, MSB-first, no reflection,
  no final XOR. It covers the header bytes from the sync code.
- **CRC-16** (frame): polynomial `0x8005`, initial value 0, MSB-first, no reflection, no
  final XOR. It covers every byte of the frame from the sync code up to the CRC itself.
