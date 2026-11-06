# aqr-prores format

aqr-prores codes the ProRes frames of a file (QuickTime or MXF) in **chunks**: a chunk is a span
of the file from a frame's start to a later frame's end, and its frames are coded with a context
model of their quantized coefficients, the frame before as context, while the bytes between them
(audio, the container's headers) go apart as they are. The coefficients are coded in stripes
of rows, each a stream of its own, so that they decode at once. From the coded chunk and
those bytes the module `prores-recons` rebuilds the span bit for bit. This document describes
ProRes frames as aqr-prores reads and writes them (§2), the module (§3), the chunk and its
streams (§4), a frame's headers (§5), the planes of coefficients (§6) and their model (§7).

The document is normative and complete: an implementation of the module can be written from it
alone. Sections marked *informative* describe the reference encoder's choices; a decoder needs
none of them.

External reference: SMPTE RDD 36 (Apple ProRes bitstream syntax and decoding process), for the
meaning of the fields. What the module writes is specified here in full.

Contents:

1. [Conventions](#1-conventions)
2. [ProRes frames](#2-prores-frames)
3. [The module](#3-the-module)
4. [The chunk](#4-the-chunk)
5. [A frame's headers](#5-a-frames-headers)
6. [Planes](#6-planes)
7. [The model](#7-the-model)
8. [The encoder](#8-the-encoder-informative) *(informative)*

## 1. Conventions

- Integers are unsigned unless signed is said. `be16` and `be32` are big-endian integers,
  `le32` little-endian. `>>` of a signed integer is arithmetic.
- `bitlen(x)` is the number of bits of `|x|` (0 for 0). `⌈a / b⌉` rounds up.
- `S[a..b)` are the bytes of `S` from `a` to `b − 1`.
- A coefficient is a signed 16-bit integer; values written to one wrap to 16 bits (two's
  complement).
- Bits of the ProRes bitstream and of the chunk's raw bits (§4) are MSB first: bit `i` is bit
  `7 − i mod 8` of byte `⌊i / 8⌋`, and a value of `n` bits is written highest bit first.

## 2. ProRes frames

### 2.1 Layout

A **frame** is, from its start:

```text
be32    size          bytes of the frame, from this field on
4       'icpf'
H bytes frame header  (its first be16 is H)
P       pictures      1, or 2 (fields) where the header's interlace mode is not 0
T bytes trailer       the rest, up to size
```

The **frame header** (offsets from its start): `be16 H` at 0 (at least 20), `be16 width` at 8 and
`be16 height` at 10 (neither 0), at 12 a byte whose top two bits are the **chroma format** `cf`
(2 for 4:2:2, 3 for 4:4:4; nothing else is read) and bits 2–3 the **interlace mode**, and at 17 a
byte whose low four bits are the **alpha type** (0 for none). The rest is kept as it is.

A **picture**:

```text
1       hs << 3 | r   header size hs, at least 8; r kept
be32    size          bytes of the picture, from its first byte
be16    n             number of slices
1       lw << 4 | r'  slices' log2 width lw (at most 3); r' kept
hs − 8  more          kept
be16    [n]           each slice's bytes
        the slices, one after another
```

A picture is `mbw = ⌈width / 16⌉` macroblocks wide. Along a row, its slices are `widths`: from
`x = 0` while `x < mbw`, a slice of `w` macroblocks with `w` the largest of `2^lw, 2^(lw−1), …, 1`
such that `x + w ≤ mbw`, then `x += w`. `n` is a whole number `rows` of rows of them.

A **slice** of `m` macroblocks:

```text
1       sh << 3 | r   header size sh, at least 6; r kept
1       q             quantizer index, kept
be16    ys            bytes of Y
be16    us            bytes of Cb
be16    vs            bytes of Cr, where sh ≥ 8; else the slice's rest less the alpha
sh − 6 or sh − 8 more kept
ys, us, vs bytes      Y, Cb, Cr
the rest              alpha (empty but where the alpha type is not 0), kept
```

Y has `4m` blocks; Cb and Cr have `2m` blocks for `cf = 2` and `4m` for `cf = 3`.

### 2.2 A component's coefficients

A component of `B` blocks (a power of two, `B = 2^b`) holds `64 B` coefficients `c[k][i]`,
block `k`, scan position `i` (0 the DC). Its bytes are empty (all coefficients 0), or:

**DC**, a codeword (§2.3) of each block in order: the first in codebook `0xB8`, giving
`d0 = (v >> 1) ^ −(v & 1)` (signed); `c[0][0] = d0`. Then `code = 5`, `sign = 0`, and for each
later block `k`: `v` in codebook `DC[min(code, 6)]`; `code = v`; `sign = 0` if `v = 0`, else
`sign ^= −(v & 1)` (0 or −1); `c[k][0] = c[k−1][0] + ((((v + 1) >> 1) ^ sign) − sign)`.

**AC**, as runs and levels over the positions `p = i·B + k` (scan position `i` from 1, block
`k`), from `run = 4`, `level = 2`, `p = B − 1`, while the component has bits left that are not
all zero: `run` in codebook `RUN[min(run, 15)]`; `p += run + 1` (below `64 B`); `level` in
codebook `LEV[min(level, 9)]`, plus 1; a sign bit (1 negative); `c[p mod B][p >> b] = ±level`.

The codewords end within the component's last byte; the bits after them are zero, and so may be
whole bytes after it (a **tail** of `z` zero bytes).

```text
DC  = 04 28 28 4D 4D 70 70
RUN = 06 06 05 05 04 29 29 29 29 28 28 28 28 28 28 4C
LEV = 04 0A 05 06 04 28 28 28 28 4C
```

### 2.3 Codewords

Codebook `cb`: `s = cb & 3`, Rice order `r = cb >> 5`, exp-Golomb order `e = (cb >> 2) & 7`.
A value `v < (s + 1) << r` is `v >> r` zeros, a one, and the `r` low bits of `v`. Any other is
`s + 1` zeros then the exp-Golomb code of order `e` of `v − ((s + 1) << r)`: with
`y = v − ((s + 1) << r) + 2^e` and `n = bitlen(y)`, `n − 1 − e` zeros then the `n` bits of `y`.

### 2.4 Writing a frame

A frame is written from its frame header, its pictures' headers (`size` aside), its slices'
headers (sizes aside), their coefficients, tails and alpha, and its trailer: each component
coded as §2.2 writes it (DC codewords, then for each nonzero coefficient in order of `p`, its run,
level and sign), padded with zero bits to a byte, then its tail; `ys`, `us` (and `vs` where
`sh ≥ 8`) are the components' bytes, the slices' sizes the slices' bytes, a picture's `size` its
bytes, and the frame's `size` its bytes. An empty component (§5) has no bytes, not even its
padding.

## 3. The module

- **Module**: name `prores-recons`, version 2.
- **Params**: `11 + k` varints (unsigned LEB128, shortest form), what decoding holds (§4.4):

  ```text
  blocks         most blocks of a frame (all its pictures' components), at most 2^24
  frame          most bytes of a frame, at most 2^32 − 1
  side           most of what the first stream tells, as §4.4 counts it
  stripe_blocks  most blocks of a stripe of a picture (§6), at most blocks
  k              the number of stripes, 1 to 64
  result[k]      bytes each stripe writes (§4.3)
  items          the frames stream 0 tells (kept or modelled)
  frames         of them, the modelled frames
  pictures       the modelled frames' pictures
  slices         the pictures' slices
  widths         the pictures' slices along a row, added up
  bytes          the bytes stream 0 gives: of the frames kept, of the frame headers and picture
                 headers it codes (those not the same as before), of the slices' headers and
                 alpha, of the trailers not of zeros
  ```

  Decoding stream 0 must give exactly these counts; else it fails.

- **Resources**: none.
- **Inputs**: exactly two, the chunk `C` (§4; class `prores/coded`) and the bytes between its
  frames `G` (class `prores/rest`, of which it is a slice).
- **Output**: the span of §4, which must be of the `len` the application declares.
- **Tasks**: the stripes (§4.3) may be decoded as tasks, at most 16 at once (the module's task
  slots); the output does not depend on it.

## 4. The chunk

`C` is `1 + k` **streams**: first, for each stream in order, `le32 A` and `le32 R`, its bytes of
arithmetic code and of raw bits; then the streams one after another, each its `A` bytes of
arithmetic code and its `R` bytes of raw bits. Nothing follows. Stream 0 tells the frames
(§4.2, §5); stream `1 + s` the coefficients of stripe `s` of every picture (§4.3).

### 4.1 Coding

The arithmetic code is binary and carry-less (32 bits), as aqr-lzo's (its `FORMAT.md` §5.1):

```text
x1 = 0, x2 = 2^32 − 1;  the decoder reads 4 bytes into x (zeros past the end)
bit with probability p1 of a 1 (16 bits):
  xmid = x1 + (((x2 − x1) × p1) >> 16)
  1: x2 = xmid;  0: x1 = xmid + 1        (the decoder reads 1 iff x ≤ xmid)
  while x1 and x2 agree in their top byte:
    the encoder writes x2 >> 24; x1 <<= 8; x2 = x2 << 8 | 0xFF; the decoder shifts in a byte
end: the encoder writes (x1 >> 24) + 1
```

Each **adaptive bit** has `p1` (initially 32768) and a count `c` (initially 0). After coding a
bit `b`: `p1 += floor((t − p1) × RATE[c] / 65536)` with `t = 65535` for a 1 and 0 for a 0,
`RATE[i] = floor(131072 / (2i + 3))`, then `p1` is clamped to 32 … 65504 and `c`, if below 30,
grows by 1. Each stream has adaptive bits of its own, fresh at its start and kept from frame to
frame.

A value of `n` **raw** bits is read from the stream's raw bits, highest bit first. Below,
`raw(n)` is such a value and `bytes(n)` is `n` bytes of `raw(8)` each.

A stream fails where its arithmetic code is read more than 4 bytes past its end, or its raw bits
past theirs.

### 4.2 Stream 0

1. `N = raw(32)`, the number of frames.
2. For each frame: `g = raw(32)`, the bytes of `G` before it; then the adaptive bit `modelled`:
   if 1, a frame's headers (§5); if 0, `n = raw(32)` and `bytes(n)`, a frame kept as it is (or
   any bytes), at most `frame` of them.
3. `g = raw(32)`, the bytes of `G` after the last frame.

### 4.3 Stripes

A picture of `rows` slice rows has `k` **stripes**: stripe `s` is its slice rows
`⌊rows·s / k⌋` up to (not with) `⌊rows·(s + 1) / k⌋`, and so may be empty.

Stream `1 + s`, for each modelled frame in order and each of its pictures `j`, codes the planes
(§6) of stripe `s` of the picture, Y, Cb and Cr, each by §7, with the same plane of stripe `s`
of picture `j` of the frame before as context where there is one of the same size in blocks.
The frame before is the modelled frame just before; there is none after a frame kept as it is
(nor at the chunk's start).

Stripe `s` **writes**, for each modelled frame and picture, the `le32` byte lengths of its slices
(in the stripe's rows, in order) and then the slices (§2.4: header, components, alpha; their
coefficients from the planes, the rest from §5). What it writes must be `result[s]` bytes.

### 4.4 The output

The output `O` is, for each frame of stream 0: the next `g` bytes of `G`; then a frame kept as
it is, or the frame written (§2.4) from its headers (§5) and, for each picture, the slices that
the stripes write, stripe by stripe (each slice's length, which must fit in 16 bits, into the
picture's table); each frame at most `frame` bytes. Then the last `g` bytes of
`G`. All of `G` is used and all that the stripes write, and `O` is `len` bytes; else decoding
fails.

What stream 0 tells is counted, and must stay within `side`: 64 for each frame, with a frame kept
as it is its bytes, and for a modelled one its frame header's and trailer's bytes, 64 for each
picture, and for each slice 256 and its header's and alpha's bytes. A frame's pictures' blocks
must stay within `blocks` (as each picture's slice count is read) and each of their stripes'
blocks within `stripe_blocks`.

## 5. A frame's headers

The adaptive bits of stream 0's frames: `same_header`, `same_pic`, `same_byte0`, `same_q`,
`tail` and `trailer_zero`, one each. Kept from frame to frame: the last frame header `FH` and,
for picture `j` (0 or 1), the last picture header `PH[j]` (its `size` zeroed), all initially
empty.

1. `same_header`: if 0, `n = raw(16)` and `FH = bytes(n)`. `FH` is the frame header (§2.1);
   `H = n` must be its first `be16`, and its width, height and chroma format must be valid.
2. For each picture `j`:
   1. `same_pic`: if 0, `n = raw(8)` and `PH[j] = bytes(n)`. It is the picture's header, `size`
      aside; its `hs` must be `n` (at least 8), its `lw` at most 3, its `n` slices a whole
      number of rows (at most 4096 rows; `mbw` at most 8192).
   2. For each slice (with `b0 = q = 0` at the picture's start): `same_byte0`: if 0,
      `b0 = raw(8)`; `same_q`: if 0, `q = raw(8)`. Its header is `b0`, `q`, the sizes (from the
      data, §2.4) and `raw(8)` for each of its other bytes; `sh = b0 >> 3` must be at least 6.
      Where the alpha type is not 0, `n = raw(16)` and the alpha is `bytes(n)`. Then for Y, Cb
      and Cr: `tail`: if 1, `e = raw(1)` and `z = raw(16)`: an empty component if `e`, and a tail
      of `z` zero bytes; if 0, neither.
3. `n = raw(32)`, the trailer's bytes; if `n > 0`, `trailer_zero`: if 1, `n` zeros, else
   `bytes(n)`.

## 6. Planes

A component of a stripe of a picture is a **plane** of blocks, `bw` wide and `bh = 2r` high for
the stripe's `r` slice rows: for Y (and Cb, Cr for `cf = 3`) `bw = 2 mbw` and a macroblock is 2×2
blocks; for Cb and Cr of `cf = 2`, `bw = mbw` and a macroblock is 1×2 blocks. Block `k` of a
slice whose left edge is macroblock `x0` in the stripe's slice row `R` (from 0) is at

- 2×2: column `2 x0 + 2 (k >> 2) + (k & 1)`, row `2R + ((k >> 1) & 1)`;
- 1×2: column `x0 + (k >> 1)`, row `2R + (k & 1)`.

Planes are coded in raster order of their blocks, each block's coefficients in scan order.

## 7. The model

Component class `cc` is 0 for Y and 1 for Cb and Cr. The plane of the frame before is `Q` where
there is one (§4.3), and then `hp = 1`; else `hp = 0`. For block `i` at column `x`, row `y`, `L` is
the block to its left, `A` above, `AL` above left and `AR` above right, where in the plane.
Per plane, each block's nonzero count `nz` and DC residual size `rs` are kept (§7.1, §7.2).

Tables (index from 0):

```text
SCAN  = 0 1 8 9 2 3 10 11 16 17 24 25 18 19 26 27 4 5 12 20 13 6 7 14 21 28 29 22 15 23 30
        31 32 33 40 48 41 34 35 42 49 56 57 50 43 36 37 44 51 58 59 52 45 38 39 46 53 60 61
        54 47 55 62 63                                (scan position -> raster position)
KBAND[i]  = i − 1 for 1 ≤ i ≤ 15, else 15 + ⌊(i − 16) / 4⌋        (27 bands)
MBAND[i]  = [0 1 2 3 3 4 4 4 5 5 5 5 6 6 6 6][⌊(i − 1) / 4⌋]
REM[r]    = r − 1 for r ≤ 4; 4 for 5–6; 5 for 7–8; 6 for 9–12; 7 for 13–16; 8 for 17–24;
            9 for 25–32; 10 above
NZB[n]    = n for n ≤ 7; 8 for 8–11; 9 for 12–15; 10 for 16–23; 11 for 24–31; 12 for 32–39;
            13 for 40–47; 14 for 48–55; 15 above
half(e)   = e for e < 4; else with t = bitlen(e) − 1, 2t + (bit t − 1 of e)
```

`FN[i]` are the scan positions of the raster neighbours above (raster `SCAN[i] − 8`) and to the
left (`SCAN[i] − 1`, in the same row) of position `i`, each where it exists and comes before `i`
in the scan; a missing one counts as 0 below.

The adaptive bits, as arrays (`NB = 16`, `PV = 4`, `NS = 15`):

| name | size | name | size |
|---|---|---|---|
| `zero` | 2·27·11·16·2·4 | `size` | 2·27·16·2·4·15 |
| `mant` | 2·17·2·8·3·3 | `sign` | 2·27·9 |
| `nz` | 2·16·9·64 | `dc` | 2·2·16·18 |
| `dc_sign` | 2·2·3 | | |

`unary(t, base, max)` decodes `v`: from `v = 0`, while `v + 1 < max` and the adaptive bit
`t[base + v]` is 1, `v += 1`.

### 7.1 DC

The prediction `P` is `Q`'s DC of the block where there is `Q`; else, with `l`, `a`, `al` the DC
of `L`, `A`, `AL`: `max(min(max(l, a), l + a − al), min(l, a))` where all three exist, `l` or `a`
where only that one exists, 0 at the first block. `rb` is `⌈(rs(L) + rs(A)) / 2⌉` where both
exist, the one that exists, or 0; then `rs = unary(dc, ((2cc + hp)·16 + min(rb, 15))·18, 17)`.
The residual is 0 for `rs = 0`; else its sign is the adaptive bit `dc_sign[(2cc + hp)·3]` (1
negative) and its magnitude `2^(rs−1) | raw(rs − 1)`. The DC is `P` plus the residual.

### 7.2 The nonzero count

`avg` is `⌈(nz(L) + nz(A)) / 2⌉` where both exist, the one that exists, or 0. `t` is
`1 + ⌊NZB[the nonzero AC coefficients of Q's block] / 2⌋` where there is `Q`, else 0. With
`base = ((16cc + NZB[avg])·9 + t)·64`, six bits are decoded from `node = 1`: each the adaptive
bit `nz[base + node]`, then `node = 2·node + bit`; the count is `nz = node − 64`.

### 7.3 The AC coefficients

Where `nz > 0`, scan positions `i = 1, 2, …, 63` are visited, with `rem = nz`, until `rem`
reaches 0 (after the coefficient that makes it 0); the coefficients not decoded are 0.
`mag[i]` is the magnitude decoded at position `i` of this block (0 before it is).

For the block, `E[i]`: where there is `Q`, `2·|Q[i]|` with `wf = 0`; else
`2·(wa·|A[i]| + wl·|L[i]| + wal·|AL[i]| + war·|AR[i]|)` with `wf = 3`, the weights by which
neighbours exist: `(wa, wl)` = (2, 2) both, (4, 0) `A` alone, (0, 4) `L` alone, (0, 0) neither;
`(wal, war)` = (1, 1) both, (2, 0) `AL` alone, (0, 2) `AR` alone; and where neither `AL` nor
`AR` exists, `wa` and `wl` are each made `w + ⌊w / 2⌋`.

At position `i`, with `tq = Q[i]` (0 without `Q`), `f = mag[FN[i][0]] + mag[FN[i][1]]`,
`nb = min(half(E[i] + wf·f), 15)`, `pv = min(bitlen(f), 3)`, `kb = KBAND[i]`:

1. The adaptive bit `zero[(((27cc + kb)·11 + REM[rem])·16 + nb)·8 + 4hp + pv]`: 0 for a zero
   coefficient (go to the next position).
2. The size `s = 1 + unary(size, (((27cc + kb)·16 + nb)·8 + 4hp + pv)·15, 16)`.
3. The magnitude, from `m = 1`: for `j = 0` and, where `s ≥ 3`, `j = 1`, a bit
   `mant[((((17cc + s)·2 + j)·8 + MBAND[i])·3 + pre)·3 + tm]` with `pre = 0` for `j = 0` and
   `1 + (m & 1)` for `j = 1`, `tm = 1 + (bit s − 2 − j of |tq|)` where `bitlen(tq) = s`, else 0;
   `m = 2m + bit`. Where `s > 3`, `m = m << (s − 3) | raw(s − 3)`.
4. The sign (1 negative): `raw(1)` where `tq = 0`; else the adaptive bit
   `sign[(27cc + kb)·9 + 3ts + tcm]` with `ts` 1 for `tq < 0` and 2 for `tq > 0`, `tcm` 1 where
   `bitlen(tq) = s` and 2 otherwise.
5. `mag[i] = m`, the coefficient is `±m`, and `rem −= 1`.

## 8. The encoder *(informative)*

`formula::encode` finds the frames of a file (a `be32` size and `icpf` whose frame parses as §2:
the frame header, pictures that fit the frame, slices that fit the pictures, components whose
codewords end within them with zero padding, coefficients within 16 bits; a frame found is
skipped whole) and cuts them into chunks of about 64 MiB of frames, a chunk ending also where
more than 4 MiB lie between two frames. A chunk has a stripe for each 4 MiB of its frames,
rounded up (at most 64): as each stripe's model learns anew, their cost stays a small part
whatever the size, and a large file's chunks decode on 16 threads. Each chunk is coded on a
thread of its own, its stripes side by side. A frame that parses is modelled: it is written again bit for bit, as its
codewords, signs and positions have one coding each, its padding is zero and recorded, and its
sizes are derived. Any other is kept as it is. The parameters are what the chunk takes, exactly.

The formula's root is the file: the bytes outside the chunks' spans (slices of the stream
`prores/rest`) and an application of `prores-recons` for each span, whose inputs are its coded
chunk (a slice of `prores/coded`, of kind compressed) and the bytes between its frames (the
slice of `prores/rest` they are in, in order). It is made where the chunks come to less than
the frames' bytes; aqr-compress weighs it against the file.
