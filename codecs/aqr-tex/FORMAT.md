# The modules of aqr-tex

This document specifies the modules of aqr-formula entry formulas that aqr-tex makes. Each
rebuilds the blocks of a texture of a group of block formats from its coded form:

- `bcn-recons` (§2 to §10): BC1, BC2, BC3, BC4 or BC5 (DXT1 to DXT5, ATI1 and ATI2), each
  block's endpoints, as those of a block before or as new, and its indices, as levels on the line
  between its endpoints, under contexts of where the pixels around (decoded so far, and those of
  the next smaller mipmap) predict them;
- `bc7-recons` (§11): BC7, each block as a block around it or before, or its fields (all but
  its indices as a block's around it or before, or new), its indices as `bcn-recons`'s.

The document is meant to be complete: an implementation of the modules can be written from it
alone (with the partition tables of BC7, which are its format's). Sections marked *(encoder)*
describe the choices this implementation makes when it builds formulas; the modules do not
depend on them.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [Blocks](#3-blocks)
4. [Coding](#4-coding)
5. [The walk](#5-the-walk)
6. [Planes](#6-planes)
7. [Endpoints](#7-endpoints)
8. [Indices](#8-indices)
9. [BC2's alpha](#9-bc2s-alpha)
10. [Encoding](#10-encoding-encoder)
11. [`bc7-recons`](#11-bc7-recons)

## 1. Conventions

- Integers are of any size, unless said otherwise; `/` of non-negative integers rounds down,
  `>>` is an arithmetic shift (it rounds down for negative integers too).
  `clamp(v, a, b)` is `min(max(v, a), b)`. **bitlen**(v) is the number of bits of v ≥ 0 (0 for
  0).
- **varint**: unsigned LEB128, least significant group first, at most 10 bytes. A writer always
  uses the shortest form.
- **MED**(a, b, c): the median of a, b and a + b − c (LOCO-I's median edge detector).
- **zz**(r) = 2r for r ≥ 0, −2r − 1 for r < 0; **unzz** is its inverse.
- Multi-byte fields of blocks are little-endian.

## 2. The module

- **Module**: name `bcn-recons`, version 1.
- **Params**, the *layout*:

  ```text
  u8      format   1: BC1, 2: BC2, 3: BC3, 4: BC4, 5: BC5 (8: BC7, of §11)
  varint  W        the width of the largest mipmap, in pixels, 1 to 65536
  varint  H        its height, 1 to 65536
  varint  M        the mipmaps of a layer, 1 to bitlen(max(W, H))
  varint  L        the layers, 1 to 65536
  ```

  Nothing else may follow, and the varints must be of their shortest form.
- **Resources**: none.
- **Inputs**: two, the streams of §5, each a code of §4: the first stream, as it is; and the
  other streams, as a packed byte sequence (the spec's "Packed byte sequences": the number of
  items and the end of each, u64 little-endian, then the items), or empty where there are
  none.
- **Output**: the blocks of the layout (§3), whose length must be the declared one. It must be
  at most 8192 times the length of the input, plus 2^24; otherwise, or where any inconsistency
  arises (§7, §4), it is an error.

## 3. Blocks

A *block* stands for 4×4 pixels; blocks are of 8 bytes (BC1, BC4) or 16 (BC2, BC3, BC5). Mipmap
m (0 the largest) of a layer is of `w_m = max(1, W >> m)` × `h_m = max(1, H >> m)` pixels and
`bw_m = ceil(w_m / 4)` × `bh_m = ceil(h_m / 4)` blocks. The output is, for each layer in turn,
its mipmaps from m = 0, each its rows of blocks from the top, each row from the left.

A block is made of *parts*, each with its own contexts (§5):

| format | bytes 0–7 | bytes 8–15 |
|---|---|---|
| BC1 | colour, three-colour mode allowed | |
| BC2 | alpha (§9) | colour, four-colour mode always |
| BC3 | value (alpha) | colour, four-colour mode always |
| BC4 | value | |
| BC5 | value (red) | value (green) |

A **colour** part is the endpoints `c0` (bytes 0–1) and `c1` (bytes 2–3), RGB565 (red in bits
15–11, green 10–5, blue 4–0), and the 2-bit index of each pixel p = 4y + x of the block in bits
2p, 2p + 1 of bytes 4–7. Its *key* is `c0 + 65536 · c1`. An endpoint's colour is its channels
expanded to 8 bits: `r·8 + r/4`, `g·4 + g/16`, `b·8 + b/4`. Its four colours, P0 to P3, are the
colours of c0 and c1 and, in four-colour mode (always in BC2 and BC3; in BC1 where c0 > c1),
channel by channel `(2·P0 + P1 + 1) / 3` and `(P0 + 2·P1 + 1) / 3`; else `(P0 + P1 + 1) / 2`
and black (0, 0, 0).

A **value** part is the endpoints `a0` (byte 0) and `a1` (byte 1), and the 3-bit index of each
pixel p in bits 3p to 3p + 2 of bytes 2–7. Its key is `a0 + 256 · a1`. Its eight values V0 to V7
are a0, a1 and, in eight-value mode (a0 > a1), `V_k = (a0·(8 − k) + a1·(k − 1) + 3) / 7` for k =
2 … 7; else `V_k = (a0·(6 − k) + a1·(k − 1) + 2) / 5` for k = 2 … 5, V6 = 0 and V7 = 255.

The **level** of an index is its place on the line from the first endpoint to the second:

| part, mode | level of index 0, 1, 2, … | steps |
|---|---|---|
| colour, four-colour | 0, 3, 1, 2 | 3 |
| colour, three-colour | 0, 2, 1, 3 (3: black) | 2 |
| value, eight-value | 0, 7, 1, 2, 3, 4, 5, 6 | 7 |
| value, six-value | 0, 5, 1, 2, 3, 4, 6, 7 (6: 0, 7: 255) | 5 |

## 4. Coding

Each stream is a binary arithmetic code (carry-less, 32 bits):

```text
x1 = 0, x2 = 2^32 − 1;  the decoder reads 4 bytes into x (zeros past the end)
bit with probability p1 of a 1 (16 bits):
  xmid = x1 + (((x2 − x1) × p1) >> 16)
  1: x2 = xmid;  0: x1 = xmid + 1        (the decoder reads 1 iff x ≤ xmid)
  while x1 and x2 agree in their top byte:
    the encoder writes x2 >> 24; x1 <<= 8; x2 = x2 << 8 | 0xFF; the decoder shifts in a byte
end: the encoder writes (x1 >> 24) + 1
```

A decoder that has read more than 4 bytes past the end of a stream when the last block it codes
is decoded fails.

Each **adaptive bit** has `p1` (initially 32768) and a count `c` (initially 0). After coding a
bit `b`: `p1 += (t − p1) × RATE[c] >> 16` with `t = 65535` for a 1 and 0 for a 0,
`RATE[i] = 131072 / (2i + 3)`, then `p1` is clamped to 32 … 65504 and `c`, if below 30, grows by
1. A **raw bit** has `p1 = 32768` and is not adapted; raw bits of a value go highest first.

- `tree_n(t)` codes `v < 2^n` highest bit first, bit `i` with `t[k]`, `k = 1` then
  `2k + bit` (t has 2^n adaptive bits; `t[0]` is not used).
- `token(t, u)` codes `v < 2^15`: `n = bitlen(v)` by `tree_4(t)`; for n ≥ 2, the bit of v below
  its top one by the adaptive bit `u[n]`, then the n − 2 bits below it, raw. (n = 0 is v = 0,
  n = 1 is v = 1.) t and u are 16 adaptive bits each.

## 5. The walk

For each layer in turn, its mipmaps are decoded from the smallest (m = M − 1) to the largest.
A mipmap of `B = bw_m · bh_m` blocks is coded in `n = clamp(B / 16384, 1, 16)` *stripes*:
stripe k (0 ≤ k < n) is its rows of blocks from `k · bh_m / n` to `(k + 1) · bh_m / n` (at least
one row each, as no row is of more than 16384 blocks). Of each stripe, the parts are decoded in
the order of §3 (all of its blocks for the first part, then all of them for the second); of each
part, its blocks in the order of the output. Decoding a block's part is its endpoints (§7), then
its indices (§8), or its alpha (§9). A stripe is decoded as a surface of its own rows: its
blocks to the left, above and above to the left (§7), and the pixels of its planes (§6), are of
the stripe alone.

The *contexts* of the texture are fresh at the start, and are kept from stripe 0 of each mipmap
to stripe 0 of the next: each part of §3 has its own (its endpoints' and its indices' of §7 and
§8, or §9's), and its own **cache** of keys (§7). Stripe 0 is coded with them. Each stripe k ≥ 1
is coded with a copy of them as they are before stripe 0 of its mipmap, and what the copy
becomes is dropped: so the stripes of a mipmap depend on the mipmaps before only, and may be
decoded at once.

The streams are: the first, which codes every stripe 0 in turn (the first input); then one for
each stripe k ≥ 1, in the order of their mipmaps, by k (the second input). Their number must be
exactly that.

## 6. Planes

As the blocks of a part of a stripe are decoded, the pixels they stand for are set in the
*plane* of the part, of `4·bw_m` × `4·r` pixels (r: the stripe's rows of blocks), each of 3
channels (colour: the pixel's colour), or 1 (value: its value; alpha: its 4-bit alpha). A pixel
is set when its block's part is decoded, and is read only after. The plane of a part of a
mipmap is those of its stripes, one above another.

- **Causal prediction** of pixel (x, y) of a stripe's plane, from those of it set before: if
  x > 0 and y > 0, channel by channel `MED(A, B, C)` of the pixels A = (x − 1, y),
  B = (x, y − 1) and C = (x − 1, y − 1); else A if x > 0, B if y > 0, and none at (0, 0).
- **Coarse prediction** of pixel (x, y) of a stripe of mipmap m < M − 1, the pixel (x, y) of
  the mipmap (y counted from the top of the mipmap, not of the stripe), from the plane P of the
  same part of mipmap m + 1 of the layer (of `w'` × `h'` pixels, its sides whole blocks): with
  `near(v, n) = (c, o)`, `c = min(v / 2, n − 1)`, `o = max(c − 1, 0)` for v even and
  `o = min(c + 1, n − 1)` for v odd, and `(cx, ox) = near(x, w')`, `(cy, oy) = near(y, h')`,
  channel by channel

  ```text
  (9·P(cx, cy) + 3·P(ox, cy) + 3·P(cx, oy) + P(ox, oy) + 8) >> 4
  ```

  Mipmap M − 1 has none.

## 7. Endpoints

The *choice* of a block is where its endpoints come from: 0 (the block to its left), 1 (the
block above), 2 (the cache) or 3 (new); `lc` and `uc` are the choices of the blocks to its left
and above, 4 where there is none. With `k = lc · 5 + uc`, let `left` be the key of the block to
the left, if any, and `up` that of the block above, if any and other than `left`:

1. if `left`: the adaptive bit `left[k · 2 + (1 if up else 0)]` (50 bits); 1: the key is
   `left`, choice 0;
2. if `up`: the adaptive bit `up[k]` (25); 1: the key is `up`, choice 1;
3. the adaptive bit `hit[lc]` (5); 1: r by `token(rank, rank_next)`, and the key is the one of
   rank r in the cache, choice 2. It is an error if r ≥ 4096 or there is no key of rank r;
4. else the key is new, choice 3, by its endpoints' residuals (below).

After a block's key is known, it is *used*: the cache of the part is the keys used before, by
the order of their last use, the most recent of rank 0 (a key used again moves to rank 0).

**New colour endpoints.** Of the endpoints of the blocks to the left (L), above (U) and above
to the left (UL), where they are, take each component `e·3 + j` in the order (e, j) = (0, 1),
(0, 0), (0, 2), (1, 1), (1, 0), (1, 2): the green, red and blue of c0, then of c1 (e: the
endpoint; j: the channel, of `n_j` = 5, 6, 5 bits). Of a block N, its value is `N_c` for e = 0
and `N_c − N_j` (the difference from c0's channel) for e = 1. Then:

```text
pred = MED(L, U, UL) if all three are there, else L if there, else U if there,
       else 2^(n_j − 1) for e = 0 and 0 for e = 1;  then, for e = 1, pred += the decoded c0 channel j
act  = min(|L − U|, 6) if both are there, else 7
g    = 0 for green, else clamp(G_e + 3, 0, 6) + 1, G_e the residual of green of this endpoint, / 2 rounded toward zero
v    = token(res[ctx], res_next[ctx]),  ctx = ((e·3 + j) · 8 + act) · 8 + g    (384 pairs of 16 bits)
```

It is an error if `v ≥ 2^n_j`. The residual is `unzz(v)`, and the component is `(pred + unzz(v))
mod 2^n_j`.

**New value endpoints.** Likewise, of a0 (e = 0) then a1 (e = 1), a block's value being `a0`
for e = 0 and `a1 − a0` for e = 1, of 8 bits:

```text
pred = MED(L, U, UL), else L, else U, else 128 for e = 0 and 0 for e = 1;  for e = 1, pred += the decoded a0
act  = min(bitlen(|L − U|), 6) if both are there, else 7
rc   = 0 for e = 0, min(bitlen(v_0), 15) for e = 1, v_0 the v of a0
v    = token(res[ctx], res_next[ctx]),  ctx = ((e · 8 + act) · 16 + rc)    (256 pairs)
```

It is an error if `v ≥ 256`; the endpoint is `(pred + unzz(v)) mod 256`.

## 8. Indices

The indices of a block's part, pixel p = 0 … 15 at (x, y) = (4·bx + p mod 4, 4·by + p / 4) of
the plane, each set in the plane as it is decoded (its colour, or value, of §3).

**Colours.** Let `D = P1 − P0` (by channel) and `l2 = |D|²`.

- If l2 = 0: index p is `tree_2(flat[(a · 5 + b) · 4 …])` (25 trees), a = 1 + the index of
  pixel p − 1 where p mod 4 > 0 (else 0), b = 1 + that of pixel p − 4 where p ≥ 4 (else 0).
- Else, with `contrast` = 1 (l2 ≤ 100), 2 (≤ 1000) or 3, `four` = 1 in four-colour mode (else
  0), `steps` of §3, and `m = steps · 2^19 / l2`:

  ```text
  bucket(Q) = clamp(((dot · m + 2^16) >> 17) + 2, 0, 14),  dot = Σ (Q_c − P0_c) · D_c over channels c
  q1 = bucket(the coarse prediction of the pixel), or 15 where there is none
  q2 = bucket(the causal prediction of the pixel), or 15 where there is none
  level = tree_2(idx[(((contrast · 2 + four) · 256) + q1 · 16 + q2) · 4 …])     (2048 trees)
  ```

  and the index is the one of that level (§3).

**Values.** Let `s = a1 − a0`.

- If s = 0: index p is `tree_3(flat[(a · 9 + b) · 8 …])` (81 trees), a and b as for colours.
- Else, with `contrast` = 1 (|s| ≤ 8), 2 (≤ 32) or 3, `eight` = 1 in eight-value mode, `steps`
  of §3, and `m = steps · 2^18 / |s|`:

  ```text
  bucket(Q) = clamp(((dot · m + 2^16) >> 17) + 1, 0, 18),  dot = (Q − a0) · sign(s)
  q1, q2 as for colours, 19 where there is none
  level = tree_3(idx[(((contrast · 2 + eight) · 400) + q1 · 20 + q2) · 8 …])    (3200 trees)
  ```

## 9. BC2's alpha

The 4-bit alpha of each pixel p of the block, in bits 4p to 4p + 3 of bytes 0–7, as for
indices:

```text
q1 = the coarse prediction of the pixel, or 16 where there is none
q2 = its causal prediction, or 16 where there is none
alpha = tree_4(a[(q1 · 17 + q2) · 16 …])    (289 trees)
```

## 10. Encoding *(encoder)*

- A DDS file is taken where its header (`DDS `, a size of 124) is of a FourCC of BC1 to BC5
  (`DXT1`–`DXT5`, `ATI1`, `ATI2`, `BC4U`, `BC4S`, `BC5U`, `BC5S`), or of a DX10 header of a DXGI
  format of them (70–84) and a 2D texture, and is not a volume texture or a cube map of fewer
  than six faces. Its mipmaps are its mipmap count where the header's flags say there is one
  (at least 1), else 1; its layers are 6 for a cube map, times the DX10 array size. All of its
  blocks must be in the file.
- The formula is the header (`dds/header`), the application, and whatever follows the blocks
  (`dds/trailer`), as they are. The first stream is apart from the others (`bcn/coded` and
  `bcn/stripes`), so that the streams of textures alike, which start alike, are as alike in the
  archive.
- A DDS file of uncompressed pixels of 8, 24 or 32 bits (no FourCC) has each of its surfaces
  applied to aqr-raw's `raster-recons` (its rows unpadded, a byte a channel), and surfaces it
  does not code kept as they are (`dds/pixels`).
- The choice of a block is the first of 1–3 of §7 that holds; the residuals of new endpoints
  are those in [−2^(n − 1), 2^(n − 1)).

## 11. `bc7-recons`

- **Module**: name `bc7-recons`, version 1.
- **Params**: the layout of §2, of format 8 (BC7).
- **Resources**, **inputs** and **output**: as §2's.

The walk is §5's, of one part (the block), its contexts and cache those of §11.3 to §11.6;
the planes §6's, of 4 channels (R, G, B, A: the pixels of §11.1).

### 11.1 Blocks

A block is 16 bytes, its bits numbered from bit 0 of byte 0 up. Its *mode* m is the number of
0 bits below its lowest 1 bit, or 8 (*reserved*) where byte 0 is 0; the m + 1 bits up to that
1 bit are followed by the fields of the mode:

| m | NS | PB | RB | ISB | CB | AB | p-bits | IB | IB2 |
|---|---|---|---|---|---|---|---|---|---|
| 0 | 3 | 4 | 0 | 0 | 4 | 0 | each endpoint | 3 | 0 |
| 1 | 2 | 6 | 0 | 0 | 6 | 0 | each subset | 3 | 0 |
| 2 | 3 | 6 | 0 | 0 | 5 | 0 | none | 2 | 0 |
| 3 | 2 | 6 | 0 | 0 | 7 | 0 | each endpoint | 2 | 0 |
| 4 | 1 | 0 | 2 | 1 | 5 | 6 | none | 2 | 3 |
| 5 | 1 | 0 | 2 | 0 | 7 | 8 | none | 2 | 2 |
| 6 | 1 | 0 | 0 | 0 | 7 | 7 | each endpoint | 4 | 0 |
| 7 | 2 | 6 | 0 | 0 | 5 | 5 | each endpoint | 2 | 0 |

In order: the partition (PB bits), the rotation (RB), the index selection (ISB); the endpoints,
channel by channel (R, G, B, then A where AB > 0), each channel of every endpoint j = 0 … 2·NS
− 1 in turn (CB bits, AB of A; endpoint 2s + e is endpoint e of subset s); the p-bits, of each
endpoint in turn, or of each subset (both of its endpoints'); the indices of the pixels p = 0 …
15 (p = 4y + x of the block), IB bits each, one bit fewer of an *anchor*; the second indices,
IB2 bits each, one bit fewer of pixel 0. Every 128 bits of a mode are a block of it.

The subset of each pixel, and the anchors (pixel 0 of subset 0, and one pixel of each other
subset), are those of the partition in BC7's tables of partitions of two subsets (modes 1, 3, 7)
and of three (modes 0 and 2; mode 0 of the first 16).

Its pixels, which the planes hold: an endpoint's channel of n bits, with its p-bit where the
mode has them (2v + p, n + 1 bits), is expanded to 8 bits as `v·2^(8 − n) + v·2^(8 − n) / 2^n`
(its top bits repeated below); A is 255 of a mode of no AB. Of an index i of b bits, its weight
w is entry i of (0, 21, 43, 64), (0, 9, 18, 27, 37, 46, 55, 64) or (0, 4, 9, 13, 17, 21, 26, 30,
34, 38, 43, 47, 51, 55, 60, 64) for b = 2, 3, 4; a channel between endpoints a and z of subset s
is `(a·(64 − w) + z·w + 32) >> 6`. All four channels are by the index of the pixel, but of
modes 4 and 5: R, G and B by its index and A by its second (mode 4 of index selection 1: R, G, B
by its second, A by its index). Then rotation r > 0 swaps channel r − 1 (R, G, B) with A. A
reserved block's pixels are (0, 0, 0, 0).

### 11.2 The choices

Of the block at (bx, by) of the stripe, L is the block to its left and U the block above (of
the stripe; none past its sides), and lc, uc their *choices* (as below; 6 where there is none).
The block's *head* is all of its bits but its indices (of mode m < 8, its low 128 − I(m) bits,
I = 45, 46, 29, 30, 78, 62, 63, 30; of mode 8, all of them).

1. If L: the adaptive bit `copy[lc·7 + uc]` (98 bits); 1: the block is L, choice 0.
2. If U and U is not L (bytes): `copy[49 + lc·7 + uc]`; 1: the block is U, choice 1.
3. If L: `head[lc]` (21 bits); 1: the head is L's, choice 2.
4. Else, if U and its head is not L's (where there is an L): `head[7 + uc]`; 1: the head is U's,
   choice 3.
5. Else `head[14 + lc]`; 1: r by `token(rank, rank_next)`, and C the block of rank r in the cache
   (an error if r ≥ 4096 or there is none): `whole[lc]` (7 bits); 1: the block is C, else its
   head is C's; choice 4.
6. Else the head is new (§11.3), choice 5.

A head taken (choices 2 to 4) of mode 8 is the block; else its indices are coded (§11.5), the
fields of the head as they are. After the block, it is *used*: the cache is §7's of keys, each
use of a key carrying a block, the block of a key's rank that of its last use. Its key is a hash
of its head h (128 bits): `x = (h mod 2^64 XOR rotl64(h >> 64, 29)) · 0x9e3779b97f4a7c15 mod
2^64`, the key `(x >> 32) XOR (x mod 2^32)`.

*(encoder)* The choice is the first of 1 to 5 that holds; a block of the cache, only where the
block of its key's last use has the block's head (keys can collide).

### 11.3 New heads

- The mode: `tree_4(mode[(ml·10 + mu)·16 …])` (100 trees), ml and mu the modes of L and U (9
  where there is none); a value above 8 is 8. Of mode 8, the bytes 1 to 15 of the block, raw
  (byte 0 is 0); then its pixels are set, and nothing else is coded.
- The partition, of NS > 1. A block *fits* where it is not of mode 8, its NS is the mode's and
  its partition below 2^PB. If L fits: `same[k]`, k = 1 where L is of the mode, else 0; 1: L's
  partition. Else (or where L does not fit), if U fits and its partition is not L's (where L
  fits): `same[2 + k]`, of U; 1: U's partition. Else `tree_PB(part[t·64 …])`, t = 0 of mode 0,
  1 of NS = 2, 2 of NS = 3.
- The rotation, of RB > 0: `tree_2(rot[((m − 4)·5 + r)·4 …])`, r L's rotation where L is of the
  mode, else 4. The index selection, of mode 4: `isb[i]`, i L's where L is of mode 4, else 2.
- The endpoints and p-bits (§11.4), then the indices (§11.5).

### 11.4 Endpoints

Pixels here are *as stored*: rotation r > 0 swaps channel r − 1 with A of the planes' pixels.
The *groups* of channels are (R, G, B) and (A) of modes 4 and 5, (R, G, B, A) of the others.

For each subset s, of first pixel a (in the order of p), predicted endpoints P0 and P1 and the
*source* of the prediction:

- of mipmap m < M − 1, source 0: the *ends* of the coarse predictions (§6) of the subset's
  pixels, in the order of p, by that of a;
- else, where there are L or U, source 1: the ends of the pixels of the stripe's plane that
  touch the subset: for each of its pixels p in turn, (4bx − 1, 4by + p/4) where L and p mod 4 =
  0, then (4bx + p, 4by − 1) where U and p < 4; where none do, all of the column to the left (top
  to bottom) and of the row above (left to right). They are by the pixel near a, (ax, ay) in the
  block: the left one of row ay where L, and U is none or ax ≤ ay; else the upper one of column
  ax;
- else, source 2: 128 in every channel of both.

The *ends* of pixels Q_1 … Q_n by a pixel A, group by group: `mean_k = (Σ_i Q_i,k) / n` of each
channel k of the group; F the first of the pixels of the most `Σ_k (Q_k − mean_k)²`; of a pixel
Q, `t(Q) = Σ_k (Q_k − mean_k)·(F_k − mean_k)`; lo the first pixel of the least t, hi the first of
the most. Where `|t(A) − t(lo)| ≤ |t(hi) − t(A)|`, P0 is lo and P1 hi in the group's channels,
else P0 hi and P1 lo.

Then of each endpoint e = 0, 1 of the subset, its channels in the order G, R, B, A (slots 0 to
3; A skipped where AB = 0), of n = CB bits (AB of A), with pb = 1 where the mode has p-bits:

```text
q      = (P_e,c · (2^(n + pb) − 1) + 127) / 255
pred   = q >> pb, plus rG (the residual of G of this endpoint) of R and B
before = class(rG) of R and B; class(r0_c) of G and A of e = 1 (r0_c the residual of channel c
         of endpoint 0); 0 of G and A of e = 0
ctx    = (((source · 2 + prec) · 4 + slot) · 2 + e) · 4 + before    (prec = 1 where CB ≥ 6)
v      = token(res[ctx·16 …], res_next[ctx·16 …])                  (96 pairs of 16 bits)
```

with `class(r)` 0 (r = 0), 1 (|r| = 1), 2 (|r| ≤ 3) or 3. The residual r is the value of
[−2^(n − 1), 2^(n − 1)) congruent to `unzz(v)` modulo 2^n, the channel `(pred + r) mod 2^n`; and
the predicted p-bit of the endpoint is q mod 2 of its G.

Then the p-bits: of each endpoint j in turn, `pbit[pp_j]`; or of each subset s, `pbit[3 +
pp_2s]`, both of its endpoints' (pp_j the predicted p-bit of endpoint j).

*(encoder)* The residuals are those of [−2^(n − 1), 2^(n − 1)).

### 11.5 Indices

The endpoints E_j expanded to 8 bits (§11.1), not rotated. The *sets* of indices of the mode,
each (b bits, kind, channels):

| mode | first | second |
|---|---|---|
| 0, 1 | (3, 1, RGBA) | |
| 2, 3, 7 | (2, 0, RGBA) | |
| 6 | (4, 2, RGBA) | |
| 4, index selection 0 | (2, 3, RGB) | (3, 6, A) |
| 4, index selection 1 | (2, 5, A) | (3, 4, RGB) |
| 5 | (2, 3, RGB) | (2, 5, A) |

Of each set and subset s, a *line*: `D = E_2s+1 − E_2s` in the set's channels (0 in the others),
`l2 = |D|²`. Where l2 > 0: `contrast` = 0 (l2 ≤ 100), 1 (≤ 1000) or 2; `steps = 2^b − 1`; `res`
= 4, 2, 1 of b = 2, 3, 4; `nb = steps·res + 3`; `m = steps·res·2^16 / l2`; and

```text
bucket(Q) = clamp(((Σ_c (Q_c − E_2s,c) · D_c · m + 2^15) >> 16) + 1, 0, nb − 1)
```

The pixels p = 0 … 15 in turn, at (x, y) of the plane: of mipmap m < M − 1, Q1 its coarse
prediction and Q2 its causal one (§6); else Q1 the pixel above (y > 0) and Q2 the one to the
left (x > 0), of the stripe's plane; as stored. Then of each set in turn (first, second), its
index: where p *leads* (an anchor, of the first set; p = 0, of the second), its b − 1 bits below
a top bit of 0 by the tree from node 2 (node 2, then 4 or 5, …); else by `tree_b`; of the trees

- where l2 > 0: `idx[base_k + ((contrast·(nb + 1) + a)·(nb + 1) + c)·2^b …]`, a and c the
  buckets of Q1 and Q2, nb where there is none;
- else: `flat[fbase_k + (left·(2^b + 1) + above)·2^b …]`, left 1 + the index of the set of pixel
  p − 1 where p mod 4 > 0 (else 0), above that of pixel p − 4 where p ≥ 4;

with base_k the sum of `3·(nb + 1)²·2^b` and fbase_k of `(2^b + 1)²·2^b` over the kinds before k
(b and nb of each kind's). Then pixel p is set in the plane (§11.1).

### 11.6 Encoding *(encoder)*

- DDS files of a DX10 header of DXGI formats 97 to 99 are BC7 (§10); KTX files of OpenGL's
  internal formats 0x8E8C and 0x8E8D, KTX 2 of Vulkan's formats 145 and 146, PVR 3 of pixel
  format 15. The blocks of a KTX, KTX 2 or PVR file (by its mipmap, each its layers) are those of
  a layout taken in the layout's order: the application's output is cut into its surfaces
  (`Formula::split`), each where the file has it, the bytes between them as they are
  (`tex/container`).
