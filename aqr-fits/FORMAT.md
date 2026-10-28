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
5. [The pyramid](#5-the-pyramid)
6. [`fits-recons-image`](#6-fits-recons-image)
7. [Rice tiles](#7-rice-tiles)
8. [`fits-recons-rice`](#8-fits-recons-rice)
9. [`fits-recons-table`](#9-fits-recons-table)

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

- an application of `fits-recons-rice` (§8) for a tile-compressed image (§10.1) of
  `ZCMPTYPE = 'RICE_1'` (or `'RICE_ONE'`) whose tiles are in the heap in order, at least one
  of them rebuilt;
- an application of `fits-recons-image` (§6) for the image of a primary or `IMAGE` HDU (no
  random groups, `PCOUNT = 0`, `GCOUNT = 1`) of `BITPIX` 8, 16, 32 or −32, where the coded
  streams are smaller than the data through zstd at level 3;
- an application of `fits-recons-table` (§9) for the rows of a binary table of two rows or
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

An image is coded in one of two *modes*: 0, by prediction (this section), or 1, as a pyramid
(§5), which takes no known pixels and whole rows only. *(encoder)* Both are tried on the
image's first rows (about 2^20 pixels, all of them for a smaller image), and the smaller is
taken.

In mode 0, the model codes an image of pixels of `B` bits, signed or not (§1), in rows of
`W` pixels (the last row perhaps shorter), row by row and in each row from the left. Some
pixels may be *known* (§8): the decoder has them already; they are not coded but are taken
through the model all the same. The output is two streams: `model`, an arithmetic code (§3),
and `raw`, MSB-first bits.

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

## 5. The pyramid

In mode 1 the image, `W` × `H` values (§1, `H` its rows), is taken apart level by level into
the sums and differences of 2×2 blocks, an H-transform: images that went through one with
quantised coefficients (hcompress's, as the Digitized Sky Survey's) have coefficients of a
few values each. Values and coefficients are signed 64-bit integers, wrapping.

**Levels.** Level 0 is the image. A level of `w` × `h` values (`w`, `h` ≥ 4, and fewer than 24
levels before it) at phase `(px, py)` (each 0 or 1) has blocks of `bw = floor((w − px) / 2)` ×
`bh = floor((h − py) / 2)`: block `(i, j)` is the values at rows `py + 2i`, `py + 2i + 1` and
columns `px + 2j`, `px + 2j + 1`, `a00 a01` over `a10 a11`. The values outside the blocks are
the level's *border*. Of a block,

```text
hx = a11 + a10 − a01 − a00     hy = a11 − a10 + a01 − a00     hc = a11 − a10 − a01 + a00
h0 = a00 + a01 + a10 + a11     p = hc & 1
c0 = hc,  c1 = (hx − p) >> 1,  c2 = (hy − p) >> 1             (its three coefficients)
```

and `h0 >> 2` is the value of the next level's `(j, i)`, a level of `bw` × `bh`. The levels end
at the first that is not taken apart (a side under 4, or 24 levels); it is the *top*. Back, of
a block from `c0`, `c1`, `c2` and the next level's value `m`:

```text
hx = 2 c1 + p,  hy = 2 c2 + p,  h0 = 4 m + ((hx + hy − hc) mod 4)       (mod: 0 to 3)
a00 = (h0 − hx − hy + hc) >> 2    a01 = (h0 − hx + hy − hc) >> 2
a10 = (h0 + hx − hy − hc) >> 2    a11 = (h0 + hx + hy + hc) >> 2
```

*(encoder)* A level's phase is the one of the smallest sum of `2 bitlen(zigzag(c)) + 1` over its
coefficients, the first of (0, 0), (1, 0), (0, 1), (1, 1) on a tie.

**Numbers.** Every number `v` is coded in a context `ctx` and a context `mctx`: `z` its zigzag
(64 bits), `n = bitlen(z)` with the tree of 6 bits `len[ctx]`; for `n ≥ 2`, the bits of `z`
after its leading one, highest first: the first `min(n − 1, 10)` with the adaptive bits
`mant[mctx][min(n, 23)]` (a tree: numbered by the bits before them, from 1), the others raw,
in `raw`, MSB first. State, fresh at the start: `len[264]` trees of 6 bits, `mant[26][24]`
trees of 10 bits, two adaptive bits `phase`.

```text
bucket(t) = min(23, bitlen(t) if bitlen(t) < 2 else 2 bitlen(t) − 1 + ((t >> (bitlen(t) − 2)) & 1))
```

**Order.** The phases of the levels, from level 0: `px` with `phase[0]`, `py` with `phase[1]`.
Then the top's values in order, each as its difference from the one before (0 before the
first), in `ctx = 240 + bucket(|d|)` of the difference `d` before it (0 at first), `mctx = 25`.
Then each level from the one below the top down to level 0:

- its blocks in order (`k = i × bw + j`), of each `c0`, `c1`, `c2` in order: coefficient `cb`
  in `ctx = (b × 3 + min(level, 2)) × 24 + bucket(t)` and `mctx = b × 8 + min(level, 7)`,
  where `t` is the sum of `|cb|` of the blocks left of it and above it (where they are), of
  `|c|` of the block's coefficients before it, and twice `|cb|` of its *parent*: at the level
  above (none for the top, nor for a level above with no blocks), the block
  `(min(floor(max(i − py', 0) / 2), bh' − 1), min(floor(max(j − px', 0) / 2), bw' − 1))` of its
  phase `(px', py')` and blocks `bw'` × `bh'`;
- the level's values in its blocks then follow (above);
- its border's values in order, each as its difference from its prediction, in `ctx = 216 +
  bucket(|d|)` of the difference `d` of the border value before it (0 at first), `mctx = 24`.
  The prediction of the value at `(x, y)` is the value below it if that is in a block, else the
  value above, else the value left, else the value right if in a block, else 0.

At the end, the decoder must not have read past `model` (§3) nor past `raw`. A value `v` is the
pixel `v` wrapping in `B` bits.

## 6. `fits-recons-image`

- **Module**: name `fits-recons-image`, version 1.
- **Params**: `varint W` (the width, at least 1), `u8 BITPIX` as a signed byte (8, 16, 32 or
  −32), then `u8 mode` (0 or 1, §4). Nothing else may follow.
- **Resources**: none.
- **Inputs**: `model` and `raw` (§4, §5).
- **Output**: the image's data, of the declared length, a multiple of `|BITPIX| / 8`: its
  pixels big-endian, of `B = |BITPIX|` bits, signed but for `BITPIX = 8`. For `BITPIX = −32`
  (IEEE floats), the pixel the model codes for bits `f` is `f ^ ((f >> 31 as signed) >> 1 as
  unsigned)`: the other bits of a negative float flipped, which orders the floats as the
  signed numbers (and is its own inverse).

The pixels are those of §4 or §5 with `W`, none known; for mode 1 the length must be of whole
rows. A length of more than 256 × (the inputs'
bytes + 64) is an error.

## 7. Rice tiles

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
are (§8).

## 8. `fits-recons-rice`

- **Module**: name `fits-recons-rice`, version 1.
- **Params**, varints:

  ```text
  BYTEPIX, BS                  of the tiles (§7): BYTEPIX 1, 2 or 4; 1 ≤ BS ≤ 65536
  D                            the image's axes, 1 to 999
  D times: Z_k, T_k            ZNAXISk and ZTILEk, each at least 1
  R, M                         bytes of a row of the table, and its rows (NAXIS1, NAXIS2)
  F, Q                         the offset in a row of the COMPRESSED_DATA field, and 1 if it
                               is a Q descriptor (else 0, a P descriptor)
  H, L                         the heap's offset in the data unit (THEAP), and its bytes
  ```

  then `u8 mode` (0 or 1, §4). Nothing else may follow. `M` must be the number of tiles, the
  product of `ceil(Z_k / T_k)`; `R × M ≤ H ≤ L`; `F` plus the descriptor's bytes (8 for P, 16
  for Q) at most `R`; the image's pixels, the product of `Z_k`, fewer than 2^40, and a tile's
  fewer than 2^32. `L` is the declared length.
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

**Pixels.** Each tile not rebuilt whose stream is not empty and decodes as a stream of §7
(`N` the tile's pixels, `BYTEPIX`, `BS`) gives its pixels to the image; the other pixels of
tiles not rebuilt are 0. These are the known pixels of §4; the pixels of the rebuilt tiles are
decoded from `model` and `raw` in the mode (mode 1 only where no pixel is known). An image of more than 256 × (the bytes of `model` and `raw`
+ 64) pixels is an error.

**Codes.** `codes` is an arithmetic code (§3) of the codes of the blocks of the rebuilt tiles,
in the order of the tiles and of their blocks. State: 32 trees of 5 bits, fresh. The code of a
block is coded with the tree numbered by the code that codes the block's differences in the
fewest bits: 0 if they are all 0; else, of RAW (`|block| × B` bits) and the codes `k + 1` for
`k` = 0 to RAW − 2 (`Σ (floor(z / 2^k) + 1 + k)` bits), the one of the fewest bits, RAW on a
tie with it and else the smallest.

**Rebuilding.** Each rebuilt tile's stream is the stream of §7 of its pixels with its codes; it
must be of the length its descriptor gives, and it goes in its place. Every code must be used.

## 9. `fits-recons-table`

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
