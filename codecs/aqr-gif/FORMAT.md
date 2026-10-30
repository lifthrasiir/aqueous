# aqr-gif format

aqr-gif splits a GIF file into a **skeleton**, the file without the LZW data of its images, and
a **record** of the images: where they are, their pixels (palette indices) under a context
model, the LZW codes given the pixels, and how the codes were cut into sub-blocks. From the
skeleton and the record the file is rebuilt bit for bit. This document describes the GIF
container as far as aqr-gif reads it (§2), GIF's LZW (§3), the skeleton and the record (§4), the
model of the pixels (§5), the codes (§6), and the [formula module](#8-formula-module).

The record does not reproduce an encoder: it codes any sequence of codes given the pixels,
against what an encoder of the longest match writes. The encoders seen (ImageMagick, ffmpeg,
Pillow, macOS's ImageIO, and those of the files of a 10 GB corpus) write the longest match and
clear where the dictionary is full; their codes cost a few bytes a file.

The document is normative and complete: an implementation of the module can be written from it
alone. Sections marked *informative* describe the encoders and the reference encoder's choices;
a decoder needs none of them.

External reference: the GIF89a specification (CompuServe, 1990), for the blocks and LZW.

Contents:

1. [Conventions](#1-conventions)
2. [The container](#2-the-container)
3. [LZW](#3-lzw)
4. [The skeleton and the record](#4-the-skeleton-and-the-record)
5. [The pixels](#5-the-pixels)
6. [The codes](#6-the-codes)
7. [Sub-blocks](#7-sub-blocks)
8. [Formula module](#8-formula-module)
9. [Encoders and the encoder](#9-encoders-and-the-encoder-informative) *(informative)*

## 1. Conventions

- Integers are unsigned unless signed is said. `le16` is a little-endian integer of 16 bits.
  `>>` of a signed integer is arithmetic (rounds towards −∞); `/` of signed integers rounds
  towards 0. `x & m` of a signed integer is of its two's complement. Arithmetic of `u32` marked
  *wrapping* is modulo 2^32.
- `bitlen(x)` is the number of bits of `x` (0 for 0). `clamp(x, a, b)` is `min(max(x, a), b)`.
- A **bit string** of bytes is read with bit `i` being bit `i mod 8` (least significant first)
  of byte `⌊i / 8⌋`; values are written into it from their least significant bit.
- `S[a..b)` are the bytes of `S` from `a` to `b − 1`.

## 2. The container

A GIF file starts with `GIF87a` or `GIF89a` and a logical screen descriptor (13 bytes in all):
the screen's width (`le16` at 6) and height (`le16` at 8), and the flags `F` at 10. A **color
table** of flags `F` is `3 × 2^((F & 7) + 1)` bytes of red, green and blue if `F & 0x80`, else
none. The global color table follows the descriptor.

A **chain** is a sequence of sub-blocks, each a size byte `n` and `n` bytes, ending with a size
byte 0 (the terminator).

Blocks follow, each starting with a byte:

- `0x21`, an extension: a label byte, then a chain.
- `0x2C`, an image: a descriptor of 10 bytes (the `0x2C`; left, top, width, height as `le16`;
  flags `G`), a local color table of flags `G`, the **minimum code size** byte `m`, then a
  chain of its LZW data. `G & 0x40` is set for an interlaced image.
- `0x3B`, the trailer, which ends the blocks.

Anything else, or a block not whole, ends the blocks. Where the chain of an image's data runs
past the end of the file, the image is taken with its data **cut** (its chain without a
terminator, its last sub-block perhaps short of its size), and the blocks end there.

The **colors** of an image are its local color table, or the global one where it has none (none
where neither is).

An **interlaced** image of `h` rows stores them in the order: every 8th from row 0, every 8th
from 4, every 4th from 2, every 2nd from 1.

## 3. LZW

An image's data is the bytes of its chain's sub-blocks, concatenated: a bit string of codes of
`m` from 2 to 8 (aqr-gif takes no other). `C = 2^m` is the clear code, `C + 1` the end code; codes
below `C` are **roots**, each the single pixel of its value.

The **dictionary** defines codes `C + 2` up to 4095, each a code defined before (its prefix) and
a pixel: its pixels are its prefix's and that one. Its state is `next` (the next code to define)
and `width` (of the next code read). **Clearing** sets `next = C + 2`, `width = m + 1`; the
dictionary starts cleared. **Defining** `(p, x)` where `next < 4096`: code `next` is `p`'s pixels
then `x`; `next += 1`; then if `next = 2^width` and `width < 12`, `width += 1`. Where
`next = 4096` nothing is defined.

A decoder reads codes of `width` bits, with `prev` (the code before, none at first):

```text
the clear code: clear; prev = none
the end code: the stream ends
else, where prev is none: the code must be a root; its pixels; prev = code
else: the code must be at most next
  define (prev, the first pixel of code) where code < next, else (prev, the first pixel of prev)
  the code's pixels; prev = code
```

The stream ends at the end code, where fewer than `width` bits are left, or before a code that
is not as above. The **end bit** of a stream is the bit after its last code read (the end code
included). Its **pad** is the bits of the end bit's byte from the end bit up (none where the end
bit is a multiple of 8), and the bytes after that byte are **after** the stream.

**Writing** codes (each a clear code, an end code or another) puts each into a bit string, of
the `width` of the dictionary before it, updating the dictionary as the decoder above does; the
end code ends the writing. The pad fills the last byte's bits above those written.

## 4. The skeleton and the record

The **skeleton** is the file with the chain of each image **coded** taken out (from its first
size byte up to and with its terminator, or the end of the file where it is cut); all else stays,
images not coded too.

The record codes, with the coder of §4.1:

1. `K = gamma(count)`, the number of images coded; it must be at most `⌊|skeleton| / 11⌋`.
2. For each, `d = gamma(count)`: its descriptor is at `from + d` of the skeleton, where `from` is
   0 for the first image and, for the others, the place of the image before (below). There must
   be a descriptor there (a byte `0x2C` and 9 more), its color table and its minimum code size
   byte within the skeleton; the image's **place** is after that byte. Its colors (§2) are read
   from the skeleton: a global color table runs to the end of the skeleton at most.
3. For each image in turn: its pixels (§4.2, §5), its codes (§6), the rest (§7).

The file is the skeleton with each image's chain written at its place (§7).

### 4.1 Coding

The record is a binary arithmetic code (carry-less, 32 bits), as aqr-lzo's (its `FORMAT.md`
§5.1):

```text
x1 = 0, x2 = 2^32 − 1;  the decoder reads 4 bytes into x (zeros past the end)
bit with probability p1 of a 1 (16 bits):
  xmid = x1 + (((x2 − x1) × p1) >> 16)
  1: x2 = xmid;  0: x1 = xmid + 1        (the decoder reads 1 iff x ≤ xmid)
  while x1 and x2 agree in their top byte:
    the encoder writes x2 >> 24; x1 <<= 8; x2 = x2 << 8 | 0xFF; the decoder shifts in a byte
end: the encoder writes (x1 >> 24) + 1
```

A decoder that reads more than 4 bytes past the end of the record fails.

Each **adaptive bit** has `p1` (initially 32768) and a count `c` (initially 0). After coding a
bit `b`: `p1 += floor((t − p1) × RATE[c] / 65536)` with `t = 65535` for a 1 and 0 for a 0,
`RATE[i] = floor(131072 / (2i + 3))`, then `p1` is clamped to 32 … 65504 and `c`, if below 30,
grows by 1. A **raw bit** has `p1 = 32768` and is not adapted; raw bits of a value go highest
first.

`gamma(t)` codes `v` as `x = v + 1`: `bitlen(x) − 1` ones then a zero, the `i`-th of them with
the adaptive bit `t[min(i, |t| − 1)]`; then the `bitlen(x) − 1` bits of `x` below its top one,
raw. Decoding fails at a 31st one.

The adaptive bits (fresh at the start of the record, kept from image to image):

| name | size | name | size |
|---|---|---|---|
| `count` | 32 | `pixels` | 32 |
| `whole` | 1 | `clear` | 16 |
| `end` | 1 | `longest` | 12 |
| `single` | 3 | `shorter` | 16 (gamma) |
| `first` | 1 | `pad` | 1 |
| `after` | 1 | `after_len` | 32 (gamma) |
| `whole_chain` | 1 | `size` | 4 |
| `size_v` | 16 (gamma) | | |

### 4.2 The count of pixels

An image of width `w` and height `h` has `N` pixels: adaptive bit `whole`, 1 for `N = w × h`;
else `N = gamma(pixels)`. The pixels of all images coded must be at most 2^28 in all.

The pixels are coded in **rows** of `max(w, 1)`: in the stream's order, but where the image is
interlaced, `w > 0` and `N ≥ w × h`, its first `h` rows in the order they are shown (§2).

## 5. The pixels

The pixels of all images are coded by one model, made before the first image for the `A`
pixels of the images as their descriptors tell (`A` the sum of `w × h`, at most 2^28) and the
screen of the skeleton (its width and height; 0 by 0 where the skeleton is shorter than 13
bytes, or where their product is above 2^26).

### 5.1 Arithmetic

```text
SQUASH = [1, 2, 4, 6, 10, 17, 27, 45, 74, 120, 194, 311, 488, 747, 1102, 1546, 2048, 2550,
          2994, 3349, 3608, 3785, 3902, 3976, 4022, 4051, 4069, 4079, 4086, 4090, 4092, 4094,
          4095]
squash(x), x signed: 4095 if x > 2047; 1 if x < −2047;
  else SQUASH[i] × (128 − f) + SQUASH[i + 1] × f + 64 >> 7, with i = (x >> 7) + 16, f = x & 127
stretch(p), p of 0 … 4095: the least x of −2047 … 2047 with squash(x) ≥ p, or 2047 if none
hash(x, y) = ((x ^ (y × 0x2F0B4F1D)) × 0x9E3779B1, rotated left by 13) × 0x85EBCA6B
  (u32, wrapping)
```

### 5.2 The model's state

- `B = clamp(bitlen(8 × A), 10, 22)`. 12 **tables** of `2^B` **slots** of 16 bits each, each a
  12-bit probability `p` above a 4-bit count `n`, all initially `p = 2048, n = 0`.
- Two mixers' **weights**, signed: 288 sets of 12, all initially 6554.
- The **APM**: `Q = clamp(bitlen(A), 8, 16)`; `2^Q` contexts of 33 values of 16 bits, value `j` of
  each initially `squash(128 × j − 2048) × 16`.
- The **canvas**: the screen's `width × height` values, initially 256 each.

### 5.3 An image

Given an image of `k` bits a pixel (its minimum code size), rows of width `w' = max(w, 1)`, at
`(left, top)`, and colors: `COLOR[v]` is the color of index `v` from the image's colors (red,
green, blue as signed values), black where they hold fewer than `v + 1` (index 256 included).

For each pixel `i` in turn, at `x = i mod w'`, `y = ⌊i / w'⌋`, of value `v`:

- **Neighbours**: `W = px[i − 1]` (`x ≥ 1`), `N = px[i − w']` (`y ≥ 1`), `NW = px[i − w' − 1]`
  (`x, y ≥ 1`), `NE = px[i − w' + 1]` (`y ≥ 1`, `x + 1 < w'`), `WW = px[i − 2]` (`x ≥ 2`),
  `NN = px[i − 2w']` (`y ≥ 2`), `NNE = px[i − 2w' + 1]` (`y ≥ 2`, `x + 1 < w'`); 256 where the
  condition does not hold. `P` is the canvas at `(left + x, top + y)`, or 256 where that is off the
  screen.
- **Colors**: with `cW = COLOR[W]` and so on, for each channel `c` of red, green, blue in turn:
  `mean = mean << 4 | ((2cW + 2cN + cNW + cNE) / 6 >> 4)`, `grad = grad << 4 |
  (clamp(cW + cN − cNW, 0, 255) >> 4)`, `up = up << 3 | ((cN + cNE) / 2 >> 5)`, all from 0.
- **Contexts**, with `p | q` the bitwise or and `<<` of `u32`:

  ```text
  h0 = 0                                     h6  = hash(hash(6, P), W | N << 9)
  h1 = hash(1, W)                            h7  = hash(hash(7, W | N << 9), NE | NNE << 9)
  h2 = hash(2, W | N << 9)                   h8  = hash(8, mean)
  h3 = hash(hash(3, W | N << 9), NW | NE << 9)   h9  = hash(hash(9, up), W)
  h4 = hash(hash(4, W | WW << 9), N)         h10 = hash(10, grad)
  h5 = hash(hash(5, N | NN << 9), NE | NW << 9)  h11 = hash(hash(11, up), N)
  ha = hash(12, W | N << 9)
  differ = (W ≠ N) + (NW ≠ W and NW ≠ N) + (NE ≠ W and NE ≠ N and NE ≠ NW)
  ```

- The `k` bits of `v`, from the highest, each at `depth` 0 … `k − 1`, with `node` (initially 1,
  then `node × 2 + bit`) and `sub` (likewise, but 1 again at each depth that is a multiple of 4):
  1. At a depth that is a multiple of 4, each table `j`'s **bucket** is
     `hash(hj, node) & (2^(B − 4) − 1)`, of the slots `16 × bucket` … `16 × bucket + 15`.
  2. Slot `j` is slot `16 × bucket + sub` of table `j`; `st_j = stretch(p_j)`.
  3. The mixers: set `s1 = node`, `s2 = 256 + min(depth, 7) × 4 + differ`;
     `d_m = clamp((Σ_j st_j × weight[s_m][j]) >> 16, −4095, 4095)` (signed, exact), `p_m = squash(d_m)`,
     and `pr = squash((d_1 + d_2) / 2)`.
  4. The APM: `sx = stretch(pr) + 2048`, `lo = sx >> 7`, `f = sx & 127`; context
     `a = hash(ha, node) & (2^Q − 1)`; `pa = APM[a][lo] × (128 − f) + APM[a][lo + 1] × f >> 11`.
  5. `pf = clamp((2pr + 2pa) >> 2, 1, 4095)`; the bit is coded with `p1 = pf × 16 + 8`.
  6. Updates, with `t = 65535` for a 1 and 0 for a 0:
     - `APM[a][lo] += (t − APM[a][lo]) × (128 − f) >> 12` and
       `APM[a][lo + 1] += (t − APM[a][lo + 1]) × f >> 12` (signed);
     - each `weight[s_m][j] = clamp(weight[s_m][j] + (st_j × e_m × 4 >> 14), −2^24, 2^24)` with
       `e_m = 4096 × bit − p_m`;
     - each slot: `p = clamp(p + ((t / 16 − p) × RATE[n] >> 16), 1, 4095)` (signed, `t / 16`
       being 4095 or 0, `RATE` of §4.1), then `n += 1` where `n < 15`.
- `v` is the low `k` bits of the final `node`.

After the image's pixels, each pixel `i` sets the canvas at `(left + i mod w', top + ⌊i / w'⌋)`,
where that is on the screen, to its value.

## 6. The codes

The codes of an image are coded given its `N` pixels `px` in the stream's order, against an
**encoder's dictionary**: the decoder's (§3) with, for each code defined, its **first** code,
the first code defined (since the last clear) of the same pixels. A code `(p, x)` defined is its
own first code unless one defined before it, since the last clear, has the first code of `p` as
its prefix's first code and `x` as its pixel; then that one's first code is its own. A root is its
own first code.

The walk, with `p = 0` (pixels coded), `prev` none, `last = 0`, `was = 0`, `started = false`:

```text
loop:
  ctx = !started + 2 × (next = 4096) + 4 × (next = last) + 8 × (p = N)
  bit clear[ctx]: 1 a clear code:
    last = next where prev is not none;  clear;  prev = none;  started = true
    fails at the 65th clear code in a row;  next loop
  started = true
  if p = N: bit end: 1 an end code;  the walk ends
  where prev is not none: define (prev, px[p])
  the longest match: from the root px[p], while p + L < N and a code's first code is the match's
    and its pixel px[p + L], that one, of length L + 1;  L the length, path[i] the first code
    of the match of length i + 1
  if L = 1: len = 1
  else if bit longest[was × 4 + lb(L)] = 1: len = L
  else if L = 2 or bit single[was] = 1: len = 1
  else: len = L − 1 − gamma(shorter), which must be at least 2
  bit first: 1 code = path[len − 1]
    0: code = width' raw bits, width' the dictionary's width now (after the definition above);
       it must be below next, neither the clear nor the end code, and of the pixels
       px[p .. p + len)
  was = 0 if len = L, 1 if len = 1, else 2
  a code;  p += len;  prev = code
```

with `lb(L)` = 0 for 1, 1 for 2, 2 for 3 … 7, 3 for 8 and more. A clear code clears both the
dictionary and the first codes.

## 7. Sub-blocks

`E` is the end bit of the codes written (§3). Then:

1. Where `E mod 8 ≠ 0`: bit `pad`, 1 for a pad of 0; else the pad as `8 − E mod 8` raw bits, which
   must not be 0.
2. Bit `after`, 1 for nothing after; else `n = 1 + gamma(after_len)` (at most 2^24) and `n` bytes
   of 8 raw bits.
3. Bit `whole_chain`, 1 for a chain with its terminator. Else it is cut: `s`, 8 raw bits, the
   size of the sub-block cut (0 where it is cut after a whole sub-block), and where `s > 0`, `r`,
   8 raw bits, the bytes of it there, which must be below `s`.
4. The data `X` is the codes written with their pad, then the bytes after. `T = |X| − r` (`r = 0`
   where the chain is whole or `s = 0`; `T` must not be negative) bytes go into whole
   sub-blocks: with `last = 255`, while `T > 0`: `u = min(last, T)`; bit
   `size[first + 2 × (u < last)]` (`first` 1 for the first sub-block), 1 for a sub-block of `u`
   bytes; else `1 + gamma(size_v)`, which must be at most 255 and `T`, and not `u`; `T` less it,
   `last` that size.

The chain is the sub-blocks (each its size, then its bytes of `X` in turn), then the terminator
(0) where it is whole; where it is cut, nothing more if `s = 0`, else `s` and the last `r`
bytes of `X`.

## 8. Formula module

The module is `gif-recons`, version 1. It rebuilds a GIF file from its skeleton and its record.

**Parameters**: none (empty).

**Inputs**: exactly two, in this order.

| slot | content |
|---|---|
| 0 | the skeleton |
| 1 | the record |

The output is the file of §4.

**Resources**: none.

The module fails on any parameters, any other count of inputs or resources, a record that fails
above, and an output whose length is not the application's declared output size.

## 9. Encoders and the encoder *(informative)*

What the encoders seen do (ImageMagick 7, ffmpeg 7, Pillow 11, macOS ImageIO through `sips`, and
the GIF files of a 10 GB corpus of the 2000s):

- A clear code first, the longest match at every step, a clear code where `next` reaches 4096
  (the dictionary full), the end code, a pad of zeros, sub-blocks of 255 bytes but the last.
  Their codes cost a few bytes a file.
- An encoder of icons in the corpus (Apache's) defines codes the decoder's dictionary already
  has, and writes either: §6's first codes are for such dictionaries.

The encoder (`recons::split`) decodes each image's stream (as far as it decodes, with at most
2^28 pixels for the file) and takes the images that give a pixel or more, of a minimum code size
of 2 to 8; the others stay in the skeleton. `formula::encode` takes a file from its header; the
skeleton goes in a stream of class `gif/skeleton`, the record in `gif/record`. aqr-compress finds
GIF files at the start of a file and in other data, up to their trailer.
