# aqr-ima format

This document specifies the module `ima-recons` of aqr-formula entry formulas, which rebuilds IMA
ADPCM blocks from a **record** of their headers and nibbles, coded with a binary arithmetic coder
under adaptive bits in contexts of the decoder's state. Any bytes of whole blocks are rebuilt bit
for bit; IMA ADPCM's own blocks cost about 58% of them (FSB5's, §5).

The document is normative and complete: the module can be implemented from it alone. Sections
marked *informative* describe the encoder's choices; a decoder needs none of them.

External reference: aqr-lzo's FORMAT.md §5.1 (codecs/aqr-lzo), for the binary arithmetic code
and its adaptive bits.

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

## 2. Blocks

A layout has `C` channels (1 to 255) and `B` bytes per channel (8 or more, a multiple of 4); a
block is `C × B` bytes:

- a **header** per channel `c`, at `4c`: the predictor (16 bits, little-endian, signed), the
  step index (a byte), a byte reserved;
- then the nibbles, `N = 2(B − 4)` per channel, in **words** of 4 bytes: word `w` of channel `c`
  at `4C + 4(wC + c)`; nibble `j` of channel `c` is in word `j / 8`, byte `(j mod 8) / 2` of it,
  its low 4 bits for even `j`, its high 4 for odd.

Xbox ADPCM (of FSB5 banks) is `B = 36`; Microsoft's IMA ADPCM (of WAV files) has `B` its block
alignment divided by its channels.

The **IMA decoder state** is a predictor `p` (−32768 to 32767) and an index `i` (0 to 88); a
header sets it to its predictor and `min(index, 88)`. A nibble `v` steps it, with `STEP` the 89
step sizes of IMA ADPCM (7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41,
45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307,
337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707,
1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845,
8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
32767):

```
s = STEP[i]
d = s >> 3 (+ s where v & 4) (+ s >> 1 where v & 2) (+ s >> 2 where v & 1)
p = clamp(p − d where v & 8, else p + d, −32768, 32767)
i = clamp(i + [−1, −1, −1, −1, 2, 4, 6, 8][v & 7], 0, 88)
```

## 3. The module

- **Module**: name `ima-recons`, version 1.
- **Params**: `varint C`, `varint B` (as §2 requires; `B` at most 2^20). Nothing after.
- **Resources**: none.
- **Inputs**: exactly 1: `record` (class `ima/record`), §4.
- **Output**: the blocks, `len` bytes, a multiple of the block's.

## 4. The record

The record is one binary arithmetic code with adaptive bits (aqr-lzo §5.1), all of them fresh
at its start; a decoder that reads more than 4 bytes past its end fails. A value of `n` bits by a
**tree** `t` is coded highest bit first, bit `k` with `t[m]`, `m = 1` then `2m + bit`.

For each block in order, for each channel `c` in order (its **end** the decoder state at the end
of its nibbles in the block before, none in the first block; its **bucket** `min(i / 6, 14)` of
its end's index, 15 where none):

1. Where `c > 0`: whether the channel's header and words are those of channel `c − 1`, with the
   adaptive bit `same[c][x]`, `x` the same decision of channel `c` in the block before (0 or 1;
   2 in the first block). Where they are, they are copied, and channel `c`'s nibbles and end are
   those of `c − 1`; on to the next channel.
2. Its header:
   - the step index: whether it is the end's index plus 1 (with the end none, 0), with
     `idx_next[bucket]`; where not, its 8 bits by the tree `idx` (256 adaptive bits);
   - the predictor, as `z = zz(predictor − base)`, `base` the end's predictor (0 where none):
     for `n` from 0 while `n < 17`, whether `n < bitlen(z)` with `pred_len[bucket][n]` (the run
     ends at the first 0); then, where `bitlen(z) = L > 0`, its bits below the leading one, bit
     `L − 2` first down to 0, bit `k` with `pred_bits[L][k]`; the predictor is `base + unzz(z)`
     as 16 bits;
   - the reserved byte: whether it is 0, with `reserved_zero`; where not, its 8 bits by the tree
     `reserved` (256).
3. Its nibbles `j` from 0 to `N − 1`, from the state its header sets, each by the tree of 16
   adaptive bits `nibble[((b × 17 + q) × 17 + r) × 16 ..]`, `b` the bucket of the state before
   the nibble (`min(i / 6, 14)`), `q` the nibble before in the channel's block (16 for the
   first), `r` channel `c − 1`'s nibble `j` in this block (16 for channel 0); each then steps the
   state. The state after the last is the channel's end.

The decoder writes each block as §2 lays it out. It fails where `len` is not a multiple of the
block's, or it reads past the record (above).

## 5. Encoding *(informative)*

`aqr_ima::formula::blocks` takes the whole blocks at the start of data. FMOD's encoder starts
each block afresh from its source: a header's predictor is the source's sample, near the end's
predictor (mostly within two steps), and its index is the end's plus one (all but 0.03% of
blocks); the reserved byte is 0; half of its stereo blocks have equal channels.

Measured on The Crew's FSB5 banks (aqr-fsb's tool, one thread on Windows), 817 samples of 300
banks, 100.2 MB of blocks: the record is 58.17% of them, encoded at 23.4 MB/s and decoded at
20.2 MB/s. (xz -9e gives about 67% of such data.)
