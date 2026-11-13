# aqr-msadpcm format

This document specifies the module `msadpcm-recons` of aqr-formula entry formulas, which
rebuilds Microsoft ADPCM blocks (`WAVE_FORMAT_ADPCM`, of WAV files and of XACT's wave banks) from
a **record** of their headers and nibbles, coded with a binary arithmetic coder under adaptive
bits in contexts of the decoder's state. Any bytes of whole blocks are rebuilt bit for bit;
XACT's blocks cost 53% to 79% of them (§5).

The document is normative and complete: the module can be implemented from it alone. Sections
marked *informative* describe the encoder's choices; a decoder needs none of them.

External reference: aqr-lzo's FORMAT.md §5.1 (codecs/aqr-lzo), for the binary arithmetic code.
The adaptive bits are aqr-ima's (codecs/aqr-ima, FORMAT.md §4), restated in §4.

Contents:

1. [Conventions](#1-conventions)
2. [Blocks](#2-blocks)
3. [The module](#3-the-module)
4. [The record](#4-the-record)
5. [Encoding](#5-encoding-informative) *(informative)*

## 1. Conventions

- Integers are exact unless marked otherwise; `>>` of a signed integer rounds toward minus
  infinity. `bitlen(x)` is the number of bits of `x` (0 for 0).
- **varint**: unsigned LEB128, least significant group first, at most 10 bytes.
- `zz(d)` of a signed `d` is `2d` for `d ≥ 0`, `−2d − 1` for `d < 0`; `unzz` its inverse.
- `clamp(x, a, b)` is `x` held within `a` and `b`; `i16(x)` is `x` modulo `2^16` as a signed
  16-bit integer.

## 2. Blocks

A layout has `C` channels (1 to 8) and `B` bytes per block (more than `7C`, at most `2^20`, with
`2(B − 7C)` a multiple of `C`). A block is:

- the **header**: the predictors of the channels (a byte each, at `c`), then their deltas (16
  bits signed, little-endian, at `C + 2c`), their samples 1 (at `3C + 2c`), their samples 2 (at
  `5C + 2c`);
- then `N = 2(B − 7C)` nibbles, nibble `j` of channel `j mod C`, in byte `7C + j / 2`: its high
  4 bits for even `j`, its low 4 for odd.

The **decoder state** of a channel is its coefficients `(a1, a2)`, a delta `δ` and two samples
`s1` and `s2`. A header sets it: the coefficients the predictor's of `[(256, 0), (512, −256),
(0, 0), (192, 64), (240, 0), (460, −208), (392, −232)]`, `(0, 0)` for a predictor of 7 or more;
`δ = clamp(delta, 16, 2^24)`; `s1` and `s2` the samples 1 and 2. A nibble `v` steps it:

```
e = v − 16 where v ≥ 8, else v
s = clamp(((s1 × a1 + s2 × a2) >> 8) + e × δ, −32768, 32767)
s2 = s1; s1 = s
δ = clamp((A[v] × δ) >> 8, 16, 2^24)
A = [230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230]
```

(This is Microsoft's decoder, `δ` held within bounds rather than wrapping in 16 bits: the state
gives only contexts here.) A delta's **bucket** is `min(bitlen(δ) − 5, 15)`, 16 for a channel of
no state.

## 3. The module

- **Module**: name `msadpcm-recons`, version 1.
- **Params**: `varint C`, `varint B` (as §2 requires). Nothing after.
- **Resources**: none.
- **Inputs**: exactly 1: `record` (class `msadpcm/record`), §4.
- **Output**: the blocks, `len` bytes, a multiple of `B`.

## 4. The record

The record is one binary arithmetic code (aqr-lzo §5.1) of **adaptive bits**, all of them fresh
at its start; a decoder that reads more than 4 bytes past its end fails. An adaptive bit has a
probability `p1` of a 1 (16 bits, initially 32768) and a count `n` (initially 0); a decision is
coded with `p1`, then with `s = [1, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 4, 5, 5, 5, 6][n]`: `p1 +=
(65535 − p1) >> s` for a 1, `p1 −= (p1 − 1) >> s` for a 0, and `n` grows by 1 while below 15. A
value of `n` bits by a **tree** `t` is coded highest bit first, bit `k` with `t[m]`, `m = 1` then
`2m + bit`. A **difference** `d` by a set `X` in context `x` is `z = zz(d)`: for `n` from 0 while
`n < 18`, whether `n < bitlen(z)` with `X.len[x][n]` (the run ends at the first 0); then, where
the run's length `L > 0`, `z`'s bits below the leading one, bit `L − 2` first down to 0, bit `k`
with `X.bits[L][k]`; `d = unzz(z)`.

For each block in order, for each channel `c` in order (its **end** its state at the end of the
block before, none in the first block), its header:

1. the predictor: whether it is the channel's predictor in the block before, with
   `same_predictor[h]`, `h` 1 where there was one, else 0 (with none, the decision is coded and
   always 0); where not, its 8 bits by the tree `predictor` (256 adaptive bits);
2. the delta: `i16(base + d)`, `d` a difference by `delta` in context the end's bucket (16 where
   none), `base` `min(δ, 32767)` of the end (0 where none);
3. sample 2: `i16(p2 + d)`, `d` a difference by `s2` in context `b` the bucket of `clamp(delta,
   16, 2^24)`, `p2 = clamp(2 s1 − s2, −32768, 32767)` of the end's samples (0 where none);
4. sample 1: `i16(p1 + d)`, `d` a difference by `s1` in context `b`, `p1 = clamp(2 × sample 2 −
   e1, −32768, 32767)`, `e1` the end's `s1` (0 where none).

Then the block's nibbles `j` from 0 to `N − 1`, of channel `c = j mod C`, each by the tree of 16
adaptive bits `nibble[((b × 17 + q) × 17 + r) × 16 ..]`, `b` the bucket of the channel's state
before it, `q` the channel's nibble before in the block (16 for its first, `j < C`), `r` nibble
`j − 1` where `c > 0` (16 for channel 0); each then steps the channel's state, which after the
block is its end. The sets `delta`, `s2` and `s1` have `len` of 17 × 19 and `bits` of 19 × 18
adaptive bits; `nibble` has 17 × 17 × 17 × 16.

The decoder writes each block as §2 lays it out. It fails where `len` is not a multiple of `B`,
or it reads past the record (above).

## 5. Encoding *(informative)*

`aqr_msadpcm::formula::blocks` takes the whole blocks at the start of data: of a WAV file's
`data` chunk (aqr-raw), of a wave of an XACT wave bank (aqr-bundle's `xact-xwb`, whose format
gives the layout: `(wBlockAlign + 22) × C`).

Measured on the ADPCM waves of five XACT wave banks of three games (`aqr-msadpcm-check`, one
thread on Windows): A Virus Named TOM's music, 73.10 MB of blocks: the record 0.786 of them;
its sound effects, 5.30 MB: 0.527 (zstd -19: 0.822 of both); encoded at 24 MB/s and decoded at
19 MB/s. A context of the samples' slope (their difference in deltas) took nothing off.
