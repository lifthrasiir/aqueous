# aqr-fits format

This document specifies the modules `fits-recons-image`, `fits-recons-rice` and
`fits-recons-table` of aqr-formula entry formulas, which rebuild the data of a FITS file's
header and data units (HDUs), and how the encoder takes a FITS file apart around them. It is
meant to be complete: an implementation of the modules can be written from it alone. Sections
marked *(encoder)* describe the choices this implementation makes when it builds formulas; the
modules do not depend on them.

FITS itself is the FITS Standard 4.0 (IAU FITS Working Group, 2018), cited by its sections.

Contents:

1. [Conventions](#1-conventions)
2. [The formula](#2-the-formula-encoder) *(encoder)*
3. [Arithmetic coding](#3-arithmetic-coding)
4. [The pixel model](#4-the-pixel-model)
5. [`fits-recons-image`](#5-fits-recons-image)
6. [Rice tiles](#6-rice-tiles)
7. [`fits-recons-rice`](#7-fits-recons-rice)
8. [`fits-recons-table`](#8-fits-recons-table)

## 1. Conventions

- Integers are unsigned unless they are marked signed. Signed integers are two's complement;
  `>>` on a signed value is an arithmetic shift (floor), and `/` on signed values truncates
  toward zero. Arithmetic is exact unless marked *wrapping*.
- **varint**: unsigned LEB128, least significant group first, at most 10 bytes. A writer
  always uses the shortest form.
- `bitlen(v)`: the bits of `v` up to its highest set bit; `bitlen(0) = 0`.
- **zigzag** of a signed `s` of `B` bits: `(s << 1) ^ (s >> (B − 1))` in `B` bits, which maps
  0, −1, 1, −2, … to 0, 1, 2, 3, …; its inverse `(z >> 1) ^ −(z & 1)`.
- **MSB-first bits**: a stream of bytes read from the first, each from its highest bit; past
  its end, it reads zeros. A writer pads the last byte with zero bits.
- A *pixel* is a number of `B` = 8, 16 or 32 bits, held as its bits. Its *value* is the
  number as unsigned for `B` = 8 and as signed otherwise.

## 2. The formula *(encoder)*

A FITS file is taken where it starts with a primary header: `SIMPLE  = `, then spaces up to
`T` in byte 30 (§4.4.1.1), and a whole block of 2880 bytes. Its HDUs are read one after
another: a header is the 2880-byte blocks up to the one with the `END` card; its data is
`|BITPIX| / 8 × GCOUNT × (PCOUNT + NAXIS1 × … × NAXISn)` bytes (NAXIS1 left out for random
groups, none for NAXIS = 0), padded to a whole block. Reading stops at the first HDU that is
not whole in the file (or does not start with `XTENSION= `); the bytes from there on are kept
as they are.

The formula is the concatenation, for each HDU, of its header (a slice of the stream
`fits/header`, text), its data, and its padding (a slice of `fits/padding`); then the bytes
after the HDUs (a slice of `fits/data`). Data of 2048 bytes or more is

- an application of `fits-recons-rice` (§7) for a tile-compressed image (§10.1) of
  `ZCMPTYPE = 'RICE_1'` (or `'RICE_ONE'`) whose tiles are in the heap in order, at least one
  of them rebuilt;
- an application of `fits-recons-image` (§5) for the image of a primary or `IMAGE` HDU (no
  random groups, `PCOUNT = 0`, `GCOUNT = 1`) of `BITPIX` 8, 16, 32 or −32, where the coded
  streams are smaller than the data through zstd at level 3;
- an application of `fits-recons-table` (§8) for the rows of a binary table of two rows or
  more and 2048 bytes or more, whose fields fit `NAXIS1`, followed by the rest of the data (gap
  and heap) as it is (a slice of `fits/data`);

and else as it is: a slice of `fits/text` for an ASCII table, of `fits/data` otherwise.

## 3. Arithmetic coding

The coded streams below are binary arithmetic codes (carry-less, 32 bits):

```text
x1 = 0, x2 = 2^32 − 1;  the decoder reads 4 bytes into x (zeros past the end)
bit with probability p1 of a 1 (16 bits):
  xmid = x1 + (((x2 − x1) × p1) >> 16)
  1: x2 = xmid;  0: x1 = xmid + 1        (the decoder reads 1 iff x ≤ xmid)
  while x1 and x2 agree in their top byte:
    the encoder writes x2 >> 24; x1 <<= 8; x2 = x2 << 8 | 0xFF; the decoder shifts in a byte
end: the encoder writes (x1 >> 24) + 1
```

A decoder that has read more than 4 bytes past the end of the stream when it is done fails.

Each **adaptive bit** has `p1` (initially 32768) and a count `c` (initially 0). After coding a
bit `b`: `p1 += floor((t − p1) × RATE[c] / 65536)` with `t = 65535` for a 1 and 0 for a 0,
`RATE[i] = floor(131072 / (2i + 3))`, then `p1` is clamped to 32 … 65504 and `c`, if below 30,
grows by 1. A **tree** of `n` bits (an array of `2^n` adaptive bits) codes `v < 2^n` highest
bit first, bit `i` with the adaptive bit numbered by the bits above it (`m = 1`, then
`m = 2m + bit`; `v = m − 2^n` at the end).

## 4. The pixel model

The model codes an image of pixels of `B` bits, signed or not (§1), in rows of `W` pixels (the
last row perhaps shorter), row by row and in each row from the left. Some pixels may be
*known* (§7): the decoder has them already; they are not coded but are taken through the model
all the same. The output is two streams: `model`, an arithmetic code (§3), and `raw`, MSB-first
bits.

**State**, all fresh at the start of the image:

- `len[48]`: trees of 6 bits;
- `second[33 × 12]`: adaptive bits;
- `bias[729]`: pairs `(sum, n)` of signed 64-bit integers, all `(0, 0)`;
- `noise`, a signed 64-bit integer, 16;
- `E_prev`, `E_cur`: arrays of `W + 2` small integers, all 0.

**Prediction** of the pixel at column `x` of row `y`, from the values (§1) of pixels coded
before it: in the first row, `a` is the pixel left of it (0 for the first pixel) and
`b = c = d = a`; in the others, `b` is the pixel above it, `a` the pixel left of it, `c` above
left and `d` above right, each of the three being `b` where it is outside the image.

```text
med   = median of a, b and a + b − c   (that is, min(max(a + b − c, min(a, b)), max(a, b)))
q(g)  = let v = (|g| × 32) / (noise + 16) in
        k = 0 for v = 0, 1 for v < 8, 2 for v < 32, 3 for v < 128, else 4;
        4 − k for g < 0, else 4 + k
ctx   = (q(d − b) × 9 + q(b − c)) × 9 + q(c − a)
corr  = floor((sum + floor(n / 2)) / n) for (sum, n) = bias[ctx] if n > 0, else 0
pred  = med + corr
t     = |a − c| + |b − c| + |b − d| + 2 × (2^E_cur[x] + 2^E_prev[x + 1] + 2^E_prev[x + 2] − 3)
l     = bitlen(t)
act   = min(47, l if l < 2 else 2l − 1 + ((t >> (l − 2)) & 1))
```

**Residual.** Of pixel `p`: `e = p − pred` wrapping in `B` bits, taken as signed of `B` bits,
and `z` its zigzag (§1); the pixel back is `pred + e` wrapping in `B` bits. A pixel not known
is coded as:

```text
n = bitlen(z)                         the tree len[act]; n > B is an error
if n ≥ 2:
  the bit (z >> (n − 2)) & 1          the adaptive bit second[n × 12 + floor(act / 4)]
  the bits of z below it, n − 2       in raw, highest first
```

(`z` is `n`: 0 or 1 for `n` < 2; else the leading 1, the bit coded and the raw bits.)

**Update**, after every pixel `p`, known or not, of residual `z` and error `err = value(p) −
pred`:

```text
(sum, n) = bias[ctx];  sum += clamp(err, −2^40, 2^40);  n += 1
if n ≥ 64: sum = sum / 2; n = n / 2                    (truncating)
noise += (min(z >> 1, 2^40) × 16 − noise) >> 6
E_cur[x + 1] = bitlen(z >> 1)
```

After each row, `E_prev` and `E_cur` are swapped (the values left in the new `E_cur` are never
read before they are written).

At the end, the arithmetic decoder must not have read past its stream (§3), nor the raw bits
past theirs.

## 5. `fits-recons-image`

- **Module**: name `fits-recons-image`, version 1.
- **Params**: `varint W` (the width, at least 1), then `u8 BITPIX` as a signed byte: 8, 16,
  32 or −32. Nothing else may follow.
- **Resources**: none.
- **Inputs**: `model` and `raw` (§4).
- **Output**: the image's data, of the declared length, a multiple of `|BITPIX| / 8`: its
  pixels big-endian, of `B = |BITPIX|` bits, signed but for `BITPIX = 8`. For `BITPIX = −32`
  (IEEE floats), the pixel the model codes for bits `f` is `f ^ ((f >> 31 as signed) >> 1 as
  unsigned)`: the other bits of a negative float flipped, which orders the floats as the
  signed numbers (and is its own inverse).

The pixels are those of §4 with `W`, none known. A length of more than 256 × (the inputs'
bytes + 64) is an error.

## 6. Rice tiles

A tile of `RICE_1` (§10.4.1) is a stream of MSB-first bits for `N` pixels of `BYTEPIX` = 1, 2
or 4 bytes (`B` = 8 × `BYTEPIX` bits) in blocks of `BS` pixels (the last one perhaps shorter):

```text
FB = 3, 4, 5            the bits of a block's code, for BYTEPIX = 1, 2, 4
RAW = 7, 15, 26         the code of a block stored as it is, for BYTEPIX = 1, 2, 4
```

The *differences* of the pixels `p_0 … p_{N−1}` are `z_i` = zigzag of `p_i − p_{i−1}` wrapping
in `B` bits (§1), `p_{−1} = p_0` (so `z_0 = 0`). The stream is:

```text
p_0                     B bits
each block:
  code                  FB bits
  code = 0:             nothing (its differences are all 0)
  code = RAW:           each z, B bits
  0 < code < RAW:       each z, of k = code − 1: floor(z / 2^k) zero bits, a one bit, then
                        the k low bits of z
```

then zero bits to the end of the byte. Codes over `RAW` are an error; so is, decoding, a
difference that is not 0 for `z_0`, or that is `2^B` or more, or a block read past the end of
the stream. An encoder may pick any code for
a block that codes it (0 only for zero differences), and this crate keeps the codes as they
are (§7).

## 7. `fits-recons-rice`

- **Module**: name `fits-recons-rice`, version 1.
- **Params**, varints:

  ```text
  BYTEPIX, BS                  of the tiles (§6): BYTEPIX 1, 2 or 4; 1 ≤ BS ≤ 65536
  D                            the image's axes, 1 to 999
  D times: Z_k, T_k            ZNAXISk and ZTILEk, each at least 1
  R, M                         bytes of a row of the table, and its rows (NAXIS1, NAXIS2)
  F, Q                         the offset in a row of the COMPRESSED_DATA field, and 1 if it
                               is a Q descriptor (else 0, a P descriptor)
  H, L                         the heap's offset in the data unit (THEAP), and its bytes
  ```

  Nothing else may follow. `M` must be the number of tiles, the product of `ceil(Z_k / T_k)`;
  `R × M ≤ H ≤ L`; `F` plus the descriptor's bytes (8 for P, 16 for Q) at most `R`; the
  image's pixels, the product of `Z_k`, fewer than 2^40, and a tile's fewer than 2^32. `L` is
  the declared length.
- **Resources**: none.
- **Inputs**: `rows`, `residue`, `rebuilt`, `codes`, `model`, `raw`.
- **Output**: the data unit, `L` bytes.

**Tiles.** Tile `i` (from 0) is the one at `i` in the order of the tiles along axis 1 first,
then axis 2, …; along axis `k` the tile starts at `j_k × T_k` and has `min(T_k, Z_k − j_k ×
T_k)` pixels. Its pixels are in the same order (axis 1 first), from the image whose pixel
`(x_1, …, x_D)` is pixel `x_1 + Z_1 × (x_2 + Z_2 × …)`. The image is taken as rows of `W = Z_1`
pixels for the model (§4), of `B = 8 × BYTEPIX` bits, signed but for `BYTEPIX = 1`.

**Spans.** `rows` is the table, `R × M` bytes. Row `i`'s descriptor at `F` is two big-endian
numbers, each of 4 bytes (P) or 8 (Q): the stream's length `n_i` and its offset `o_i`; tile
`i`'s stream is the bytes `H + o_i` to `H + o_i + n_i` of the data unit, which must be within
`L`. The streams that are not empty must come in the order of the tiles, apart, after the
table.

**Layout.** `rebuilt` is a bit per tile, the lowest of each byte first, `ceil(M / 8)` bytes:
whether the tile's stream is rebuilt. The data unit is the table, then the bytes from `R × M`
to `L` in which the streams of the rebuilt tiles are cut out, those being `residue` (which
must be used up exactly).

**Pixels.** Each tile not rebuilt whose stream is not empty and decodes as a stream of §6
(`N` the tile's pixels, `BYTEPIX`, `BS`) gives its pixels to the image; the other pixels of
tiles not rebuilt are 0. These are the known pixels of §4; the pixels of the rebuilt tiles are
decoded from `model` and `raw`. An image of more than 256 × (the bytes of `model` and `raw`
+ 64) pixels is an error.

**Codes.** `codes` is an arithmetic code (§3) of the codes of the blocks of the rebuilt tiles,
in the order of the tiles and of their blocks. State: 32 trees of 5 bits, fresh. The code of a
block is coded with the tree numbered by the code that codes the block's differences in the
fewest bits: 0 if they are all 0; else, of RAW (`|block| × B` bits) and the codes `k + 1` for
`k` = 0 to RAW − 2 (`Σ (floor(z / 2^k) + 1 + k)` bits), the one of the fewest bits, RAW on a
tie with it and else the smallest.

**Rebuilding.** Each rebuilt tile's stream is the stream of §6 of its pixels with its codes; it
must be of the length its descriptor gives, and it goes in its place. Every code must be used.

## 8. `fits-recons-table`

- **Module**: name `fits-recons-table`, version 1.
- **Params**: `varint M`, the rows; then, for each column of a row, in order, `varint len` (its
  bytes) and `u8 w | delta × 128`: the bytes of its elements `w` (1, 2, 4 or 8, dividing
  `len`) and whether they are differences.
- **Resources**: none.
- **Inputs**: `planes`.
- **Output**: the rows of a binary table, `M × R` bytes for `R` the columns' bytes (the
  declared length; `R` at least 1).

An element is a big-endian number of `w` bytes. `planes` is, for each column, for each byte of
its elements from the most significant, for each element of the row, for each row, that byte
of the element as stored: the element itself, or with `delta` its difference from the element
of the row above, wrapping in `w` bytes (the row above the first is zeros). It must be exactly
`M × R` bytes.

*(encoder)* A column is a field of `TFORMn` (§7.3.1); its elements are of 2 bytes for `I`, 4
for `J`, `E`, `C` and `P`, 8 for `K`, `D`, `M` and `Q`, and 1 otherwise. `delta` is taken
where the planes' bytes have the smaller order-0 entropy that way, each byte position of each
element counted apart.
