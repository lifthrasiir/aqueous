# The modules of aqr-tex

This document specifies the modules of aqr-formula entry formulas that aqr-tex makes. Each
rebuilds the blocks of a texture of a group of block formats from its coded form:

- `bcn-recons` (§2 to §10): BC1, BC2, BC3, BC4 or BC5 (DXT1 to DXT5, ATI1 and ATI2), each
  block's endpoints, as those of a block before or as new, and its indices, as levels on the line
  between its endpoints, under contexts of where the pixels around (decoded so far, and those of
  the next smaller mipmap) predict them;
- `bc7-recons` (§11): BC7, each block as a block around it or before, or its fields (all but
  its indices as a block's around it or before, or new), its indices as `bcn-recons`'s;
- `bc6h-recons` (§12): BC6H, as `bc7-recons`;
- `etc-recons` (§13): ETC1, ETC2 and EAC, each part of a block (RGB, EAC) as `bc7-recons`'s
  blocks, the fields of its mode by the pixels predicted, its indices by a ladder of its levels;
- `pvrtc-recons` (§14): PVRTC1, a surface's colours (as `bc7-recons`'s blocks, new ones by the
  words around) then its modulation (by the images between which it blends);
- `astc-recons` (§15): ASTC of 2D footprints, as `bc7-recons`, of its fields as ASTC reads them
  and the codes of its integer sequences as choices among those of their values.

The document is meant to be complete: an implementation of the modules can be written from it
alone (with the partition tables of BC7, the bit layouts of BC6H and what ASTC reads, which are
their formats').
Sections marked *(encoder)* describe the choices this implementation makes when it builds
formulas; the modules do not depend on them.

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
12. [`bc6h-recons`](#12-bc6h-recons)
13. [`etc-recons`](#13-etc-recons)
14. [`pvrtc-recons`](#14-pvrtc-recons)
15. [`astc-recons`](#15-astc-recons)

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
  u8      format   1: BC1, 2: BC2, 3: BC3, 4: BC4, 5: BC5 (8: BC7, of §11; 6, 7: BC6H, of §12;
                   16 to 23: ETC and EAC, of §13; 32, 33: PVRTC1, of §14; 48: ASTC, of §15,
                   then its footprint)
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

## 12. `bc6h-recons`

- **Module**: name `bc6h-recons`, version 1.
- **Params**: the layout of §2, of format 6 (BC6H of unsigned half floats) or 7 (signed).
- **Resources**, **inputs** and **output**: as §2's.

The walk is §5's, of one part (the block); the planes §6's, of 3 channels (R, G, B) of 16 bits:
the values of §12.1. Its choices, cache and coding are §11's but where told otherwise.

### 12.1 Blocks

A block is 16 bytes, its bits numbered as BC7's. Its *mode* m is that of its header: of its low
2 bits 00, mode 0; 01, mode 1; else of its low 5 bits, 00010, 00110, 01010, 01110, 10010, 10110,
11010, 11110, 00011, 00111, 01011, 01111 for modes 2 to 13, and 10011, 10111, 11011, 11111 are
*reserved* (mode 14). After the header, the fields of the mode: those of the endpoints w, x
(subset 0), y, z (subset 1) and of the partition, in the order of BC6H's tables of their bits
(some, of modes 12 and 13, in reverse); then the indices of the pixels p = 0 … 15, of 3 bits
(modes 0 to 9, of two subsets) or 4 (of one), one bit fewer of an anchor (the partitions, of 5
bits, are BC7's first 32 of two subsets). Every 128 bits of a mode are a block of it.

Of each mode, W the bits of w's channels and d_c those of the others' (of R, G, B):

| m | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 | 13 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| W | 10 | 7 | 11 | 11 | 11 | 9 | 8 | 8 | 8 | 6 | 10 | 11 | 12 | 16 |
| d | 5 5 5 | 6 6 6 | 5 4 4 | 4 5 4 | 4 4 5 | 5 5 5 | 6 5 5 | 5 6 5 | 5 5 6 | 6 6 6 | 10 10 10 | 9 9 9 | 8 8 8 | 4 4 4 |

Modes 9 and 10 are not *transformed*: their endpoints are as stored; of the others, x, y and z
are differences from w. An endpoint's channel as an *integer*: w's is its field, sign-extended
of signed formats; another's, its field, sign-extended where the format is signed or the mode
transformed, then of a transformed mode `(v + w) mod 2^W`, sign-extended of signed formats.
*Unquantized*, of W bits, an integer v is, of unsigned formats: v where W ≥ 15; 0 for 0; 0xFFFF
for 2^W − 1; else `(v·2^16 + 2^15) >> W`; of signed formats: v where W = 16; else of a = |v|, 0
for 0, 0x7FFF where a ≥ 2^(W − 1) − 1, else `(a·2^15 + 2^14) >> (W − 1)`, of the sign of v.

Its pixels, which the planes hold: of each channel, `(a·(64 − w) + z·w + 32) >> 6` of the
endpoints a, z of the pixel's subset unquantized, w the weight of its index (§11.1), plus 0x8000
of signed formats. A reserved block's pixels are 0 (0x8000 of signed formats).

### 12.2 The choices

As §11.2, the head of a block of mode m < 14 its low 82 bits (of two subsets) or 65 (of one),
of a reserved block all of it.

### 12.3 New heads

- The mode: `tree_4(mode[(ml·16 + mu)·16 …])` (256 trees), ml and mu the modes of L and U (15
  where there is none); a value above 14 is 14. Of mode 14: the header's 5 bits, raw (a value
  that is not a reserved header is 10011), then the 3 bits above them, then bytes 1 to 15, raw;
  then its pixels are set, and nothing else is coded.
- The partition, of two subsets: as §11.3, a block fitting where it is not reserved and of two
  subsets; `same` of 4 bits, else `tree_5(part)`.
- The endpoints (§12.4), then the indices (§12.5).

### 12.4 Endpoints

For each subset s, predicted endpoints P0 and P1 and their source, as §11.4 of the plane's
values, of one group of channels (R, G, B); of source 2, 0x8000 in every channel. Then of each
endpoint e = 0, 1 of the subset (j = 2s + e), its channels in the order G, R, B (slots 0 to 2):

```text
q      = quantize(P_e,c − bias, W)                      (bias 0x8000 of signed formats, else 0)
n      = W of j = 0, d_c else
pred   = q of j = 0 or of a mode not transformed, else q − I0_c (the integer of w's channel c);
         plus rG (the residual of G of this endpoint) of R and B
before = class(rG) of R and B; 0 of G of j = 0; class(r0_G) of G else (r0 the residuals of w)
ctx    = (((source · 3 + prec) · 4 + j) · 3 + slot) · 8 + before   (prec 0, 1, 2 of W ≤ 8, ≤ 11, more)
v      = wtoken(res[ctx·32 …], res_next[ctx·32 …])                (864 pairs of 32 bits)
```

with `class(r)` the bit length of |r|, up to 7, and `wtoken(t, u)` coding v < 2^31 as `token`
does but its bit length by `tree_5(t)` (u of 32 bits). The residual r is the value of
[−2^(n − 1), 2^(n − 1)) congruent to `unzz(v)` modulo 2^n, the field `(pred + r) mod 2^n`.

`quantize(u, W)` of unsigned formats: `clamp(u, 0, 2^W − 1)` where W ≥ 15, else
`clamp(u, 0, 0xFFFF)·2^W >> 16`; of signed formats: u where W = 16, else of the sign of u and of
magnitude `min(min(|u|, 0x7FFF)·2^(W − 1) >> 15, 2^(W − 1) − 1)`.

### 12.5 Indices

The endpoints E_j unquantized, plus 0x8000 of signed formats; b = 3 (two subsets) or 4 (one).
Of each subset, a line: `D = E_2s+1 − E_2s`, `l2 = |D|²`; where l2 > 0, `contrast` = 0 (l2 ≤
10^4), 1 (≤ 10^6) or 2, `scale` = 14 (b = 3) or 15 (b = 4), `nb = scale + 3`, and

```text
bucket(Q) = clamp(floor((Σ_c (Q_c − E_2s,c) · D_c · scale + floor(l2 / 2)) / l2) + 1, 0, nb − 1)
```

Then the pixels p = 0 … 15 in turn, Q1 and Q2 as §11.5, each's index as §11.5's of one set
(where p is an anchor, by the tree from node 2), of the trees `idx[base + ((contrast·(nb + 1) +
a)·(nb + 1) + c)·2^b …]` (base 0 of b = 3, 3·18²·8 of b = 4) or, where l2 = 0, `flat[fbase +
(left·(2^b + 1) + above)·2^b …]` (fbase 0 of b = 3, 9·9·8 of b = 4). Then pixel p is set in the
plane: `(E_2s,c·(64 − w) + E_2s+1,c·w + 32) >> 6` of each channel.

### 12.6 Encoding *(encoder)*

- DDS files of a DX10 header of DXGI formats 94 and 95 are BC6H unsigned, of 96 signed; KTX files
  of OpenGL's internal formats 0x8E8F (unsigned) and 0x8E8E (signed), KTX 2 of Vulkan's 143 and
  144, PVR 3 of pixel format 14 (signed by its channel type), as §11.6.

## 13. `etc-recons`

- **Module**: name `etc-recons`, version 1.
- **Params**: the layout of §2, of format 16 (ETC1), 17 (ETC2's RGB), 18 (ETC2's RGBA), 19 (ETC2's
  RGB of punch-through alpha), 20 to 23 (EAC: R11, R11 signed, RG11, RG11 signed).
- **Resources**, **inputs** and **output**: as §2's.

The walk is §5's, of the parts of §13.1, each its own contexts and cache (§11.2's), in the order
of the table; the planes §6's: of an RGB part 4 channels (R, G, B, A) of 8 bits, of an EAC part 1
channel of 16 bits (its values, §13.1).

### 13.1 Blocks

| format | bytes 0–7 | bytes 8–15 |
|---|---|---|
| ETC1, ETC2, ETC2 A1 | RGB | |
| ETC2 RGBA | EAC of alpha | RGB (of ETC2) |
| R11 | EAC of R11 | |
| RG11 | EAC of R11 (red) | EAC of R11 (green) |

A part is a 64-bit word, its bytes most significant first (bit 63 the top bit of its byte 0).
Of the pixels p = 4y + x of a block, the indices go column by column: pixel p's is the k-th, k =
4x + y.

**RGB.** Of a word w, its *mode*: of ETC1, *individual* where bit 33 is 0, else *differential*;
of ETC2, individual where bit 33 is 0; of ETC2 and ETC2 A1 (whose bit 33 is its *opacity*), else
by the 5-bit values R (bits 63–59), G (55–51), B (47–43) and the 3-bit two's-complement Rd
(58–56), Gd (50–48), Bd (42–40): *T* where R + Rd is outside 0–31, else *H* where G + Gd is,
else *planar* where B + Bd is, else differential. The fields of each mode, each its segments of
bits from its most significant:

| mode | fields |
|---|---|
| individual | R1 63–60, R2 59–56, G1 55–52, G2 51–48, B1 47–44, B2 43–40, cw1 39–37, cw2 36–34, flip 32 |
| differential | R 63–59, Rd 58–56, G 55–51, Gd 50–48, B 47–43, Bd 42–40, cw1 39–37, cw2 36–34, flip 32 |
| T | R1 (60–59, 57–56), G1 55–52, B1 51–48, R2 47–44, G2 43–40, B2 39–36, dist (35–34, 32) |
| H | R1 62–59, G1 (58–56, 52), B1 (51, 49–47), R2 46–43, G2 42–39, B2 38–35, da 34, db 32 |
| planar | RO 62–57, GO (56, 54–49), BO (48, 44–43, 41–39), RH (38–34, 32), GH 31–25, BH 24–19, RV 18–13, GV 12–6, BV 5–0 |

Bit 33, and of every mode but planar the indices (bits 0–31:
the k-th pixel's index of 2 bits, its top bit at 16 + k and its low bit at k), are apart; the bits
of no field of the mode are its bits *left over* (T: 63–61, 58; H: 63, 55–53, 50; planar: 63,
55, 47–45, 42; none of the others).

Its pixels: of individual and differential blocks, the subblock of pixel p is by x (x ≥ 2) where
flip is 0, by y (y ≥ 2) where it is 1; base colours of 4 bits (individual: R1, G1, B1 and R2, G2,
B2) times 17, of 5 bits (differential: R, G, B, and `(R + Rd) mod 32` and the like) expanded as
`v·8 + v/4`; of codeword cw, modifiers a, b = (2, 8), (5, 17), (9, 29), (13, 42), (18, 60), (24,
80), (33, 106), (47, 183); index 0 the base plus a, 1 plus b, 2 minus a, 3 minus b, of every
channel, clamped to 0–255; but of ETC2 A1 of opacity 0, index 0 the base and index 2 transparent.
Of T and H blocks, c1 = (R1, G1, B1) and c2 = (R2, G2, B2) times 17, d the distance of DIST =
(3, 6, 11, 16, 23, 32, 41, 64): of T, by dist, its paint colours c1, c2 + d, c2, c2 − d; of H, by
`da·4 + db·2 + (c1 ≥ c2)` (as `R·256 + G·16 + B` of their 4 bits), c1 + d, c1 − d, c2 + d, c2 −
d (each channel clamped); index i its paint colour i, index 2 transparent of ETC2 A1 of opacity
0. Of planar blocks, O, H and V of 6, 7 and 6 bits expanded (6: `v·4 + v/16`; 7: `v·2 + v/64`),
each channel `clamp((x·(H − O) + y·(V − O) + 4·O + 2) >> 2, 0, 255)`. Alpha is 255; a
transparent pixel is (0, 0, 0, 0).

**EAC.** Of a word: base (bits 63–56), multiplier m (55–52), table t (51–48), and the k-th
pixel's index at bits 47 − 3k to 45 − 3k. Of modifier M of row t of EAC's table of 16 rows by
index, the value of an index: of ETC2's alpha, `clamp(base + M·m, 0, 255)`; of R11,
`clamp(base·8 + 4 + M·m·8, 0, 2047)` (M where m = 0); of R11 signed, of b the base as a signed
byte (−127 for −128), `clamp(b·8 + M·m·8, −1023, 1023) + 1024` (M where m = 0). The planes hold
the values of the pixels' indices.

### 13.2 Copies

Of each part, §11.2's choices (L and U of the part; the cache of 8-byte blocks), the block its
bytes. The head of an RGB block: of planar mode, all 64 bits; else 2^64 + its bits 63–32. Of an
EAC block, its bits 63–48. A head taken is of the block's fields; of a planar block it is the
block, else its indices are coded (§13.4, §13.6).

### 13.3 New RGB blocks

The predicted pixels of the block, A, and the source: of mipmap m < M − 1, the coarse
predictions (§6) of the block's pixels, source 0; else, where there are L or U, source 1, and of
pixels `of`, those of the stripe's plane that touch them (§11.4: of each of them in turn, the
pixel to the left where x = 0, then above where y = 0; else all of the column to the left and the
row above); else source 2, no pixels. And A(x, y): the coarse prediction of pixel (x, y), or the
left pixel of row y where L, and U is none or x ≤ y, else the upper one of column x.

1. The mode: of ETC1, `mode[ml·6 + mu]` (1: differential); else `tree_3(mode[(ml·6 + mu)·8 …])`
   (a value above 4 is 4); ml and mu the modes of L and U (5 where there is none).
2. Bit 33: of ETC2 A1, `opaque[min(mode, 1)·3 + o]`, o L's bit 33 (2 where there is none); else
   1 but of individual blocks.
3. Of individual and differential blocks: the flip, `flip[(fl·3 + fu)·3 + s]` (fl and fu L's
   and U's flips where they are individual or differential, else 2; s of source 0: 1 where the
   coarse predictions split top and bottom into two of less spread than left and right — of
   each half the sum over its pixels of `|S(p) − (Σ S)/8|`, S the sum of a pixel's channels — 0
   where not; 2 of the others). Then of each subblock k, the mean P_k of its predicted pixels
   (`(Σ + n/2) / n` of each channel; 128 of none) and the class of their *spread* (the range of
   their sums of channels: 0 up to 24, 1 up to 96, 2 up to 300, 3; 0 of none).
   - individual: the colours (below) of fields (R1, G1, B1) of kind 0 by P_0, (R2, G2, B2) of kind
     5 by P_1, of 4 bits;
   - differential: (R, G, B) of kind 1 by P_0, of 5 bits; then of each channel c in the order G,
     R, B (slots 0 to 2), the difference `pred = clamp(q − base_c, −4, 3)` (q = `(P_1,c·31 + 127)
     / 255`, base_c the channel just decoded), its field `(pred + v) mod 8` of v by
     `tree_3(delta[(slot·4 + (pred + 4)/2)·8 …])`;
   - the codewords cw1 and cw2 in turn: `tree_3(cw[((mode·4 + spread_k)·9 + before)·8 …])`,
     before 8 of cw1, cw1 of cw2.
4. Of T and H blocks: (R1, G1, B1) of kind 2 (T) or 3 (H) by the mean of all the predicted pixels,
   of 4 bits; (R2, G2, B2) of kind 7 (T) or 8 (H) by c1 (times 17); then T's dist, H's da and db
   in turn, `tree_n(dist[(t·4 + j)·8 …])` (n the field's bits; t 0 of T and 1 of H, j the field's
   place).
5. Of planar blocks: O, H, V in turn, of kind 4, of 6, 7 and 6 bits, by O = A(0, 0) (128 of none),
   H = O + (A(3, 0) − O)·4/3 and V = O + (A(0, 3) − O)·4/3 (of each channel; division toward zero;
   O where there is none).
6. The bits left over, from the most significant: the j-th (from 0) by
   `over[(min(max(mode − 2, 0), 2)·6 + min(j, 5))·2 + bit 63]` (bit 63 as decoded so far).
7. Of planar blocks, the pixels; of the others, the indices (§13.4).

The *colours* of fields (F_R, F_G, F_B) of kind k, of n_c bits, by prediction P: of each channel
in the order G, R, B (slots 0 to 2), `pred = (clamp(P_c, 0, 255)·(2^n − 1) + 127) / 255`, plus rG
(the residual of G) of R and B; `ctx = ((k·3 + source)·3 + slot)·4 + before`, before 0 of G and
class(rG) of R and B (§11.4's class); `v = token(res[ctx·16 …], res_next[ctx·16 …])` (360 pairs);
the residual r the value of [−2^(n − 1), 2^(n − 1)) congruent to unzz(v), the field `(pred + r)
mod 2^n`.

### 13.4 RGB indices

Of each pixel p in turn, at (x, y) of the plane, Q1 and Q2 as §11.5 (of mipmap m < M − 1 the
coarse and causal predictions, else the pixels above and to the left), of a *kind* 1 of ETC2 A1
of opacity 0, else 0:

- of individual and differential blocks, of p's subblock k: the *ladder* of its levels l = 0 to 3,
  of indices 3, 2, 0, 1 (opaque, as §13.1), the sums of their channels; of a prediction of A ≥
  128, its *rung* (below) of the sum of its channels, else (or where there is none) 8; the level
  by `tree_2(idx[(((kind·3 + c)·9 + a)·9 + b)·4 …])` (c = min(cw_k / 3, 2), a and b of Q1 and Q2),
  and the index that of the level;
- of T and H blocks: of each prediction, the paint colour nearest it (the first of the least
  Σ (Q_c − paint_c)² over R, G, B), 4 where its A < 128, 5 where there is none; the index by
  `tree_2(paint[(((t·2 + kind)·6 + a)·6 + b)·4 …])`.

The *rung* of x on a ladder of values v_0 ≤ … ≤ v_n−1: the number of its thresholds at or below
x, of v_i (x ≥ v_i) and of the midpoints (2x ≥ v_i + v_i+1). The pixel is set in the plane as it
decodes.

### 13.5 New EAC blocks

The predicted values (as §13.3, one channel) and their mean m (1024, or 128 of alpha, of none),
and their range where there are any. The base: `pred` = m of alpha, `(m − 4) >> 3` of R11, `((m −
1024) >> 3) mod 256` of R11 signed; `v = token(res[source·16 …], res_next[source·16 …])`, the base
`(pred + r) mod 256`, r of [−128, 128) congruent to unzz(v). The table by `tree_4(table[t·16 …])`,
t L's table (16 where there is none). The multiplier by `tree_4(mult[s·16 …])`, s (8 where there
are no values) the range by the table's span u = (EAC[t][7] − EAC[t][3]) times 1 (alpha) or 8
(R11): `min((range + u/2) / u, 7)`.

### 13.6 EAC indices

The ladder of the block's levels l = 0 to 7, of indices 3, 2, 1, 0, 4, 5, 6, 7, their values.
Where its first and last are equal (*flat*), of each pixel p in turn, its index by
`tree_3(flat[(left·9 + above)·8 …])` (§11.5's left and above, of this block's indices); else by
the level `tree_3(idx[((c·17 + a)·17 + b)·8 …])`, c = 0 (span of the ladder up to 8), 1 (up to
64) or 2, a and b the rungs of Q1 and Q2 (16 where there is none). The pixel is set in the plane.

### 13.7 Encoding *(encoder)*

- KTX files of OpenGL's internal formats 0x8D64 (ETC1), 0x9274–0x9279 (ETC2) and 0x9270–0x9273
  (EAC), KTX 2 of Vulkan's 147–156, PVR 3 of pixel formats 6 (ETC1) and 22–26 (signed EAC by its
  channel type), as §11.6; Unity's textures of formats 34 (ETC1), 41–47 (EAC and ETC2) by aqr-bundle.

## 14. `pvrtc-recons`

- **Module**: name `pvrtc-recons`, version 1.
- **Params**: the layout of §2, of format 32 (PVRTC1 of 4 bits a pixel: words of 4×4 pixels) or
  33 (of 2 bits: 8×4), its sides powers of two. A surface is of at least 2 × 2 words.
- **Resources**, **inputs** and **output**: as §2's.

The walk is §5's, but every surface is one stripe (its words depend on those around them); of
one part, the word, its contexts and cache those of §14.2 to §14.4; the planes §6's, of 4
channels (R, G, B, A). A surface's words are taken row by row (word (X, Y) of a surface of w × h
words), each at the offset of its *reflected Morton order* in the output: the bits of X and Y
interleaved from the lowest, Y's the lower of each pair, as far as the bits of min(w, h) − 1 go,
then the rest of X's (or Y's) above them.

### 14.1 Words

A word is 8 bytes, little-endian. Bit 32 is the modulation flag M; colour A is bits 33–47, colour
B bits 48–63, each of a top bit 1 (opaque: A of R 5, G 5, B 4 bits; B of 5, 5, 5) or 0
(translucent: alpha 3 bits, then A of R 4, G 4, B 3; B of 4, 4, 4), its channels from the top. As
ARGB 4555: alpha `a·2` (15 opaque); a channel of 3 bits `c·4 + c/2`, of 4 `c·2 + c/8`, of 5 as it
is.

The images A and B at pixel (x, y), of footprint (fw, 4): of `XL = floor((x − fw/2) / fw)`, `YL =
floor((y − 2) / 4)`, `xr = x − fw/2 − fw·XL`, `yr = y − 2 − 4·YL`, and the words at (XL, YL), (XL
+ 1, YL), (XL, YL + 1), (XL + 1, YL + 1) (wrapping at the sides), each channel `C = c00·(fw −
xr)·(4 − yr) + c10·xr·(4 − yr) + c01·(fw − xr)·yr + c11·xr·yr` of the colour's in each word, as 8
bits: of 4 bits a pixel, alpha `C + C/16` and the others `C/2 + C/64`; of 2, alpha `C/2 + C/32`
and the others `C/4 + C/128`. A pixel of weight w is `(A·(8 − w) + B·w) / 8` of each channel;
alpha 0 where it is punch-through.

Of 4 bits a pixel, pixel p = 4y + x's 2-bit value at bits 2p, 2p + 1: its weight, of M = 0, 0,
3, 5, 8; of M = 1, 0, 4, 4 (punch-through), 8. Of 2 bits, pixel p = 8y + x: of M = 0, bit p, of
weight 0 or 8. Of M = 1, bit 0 is the *mode* (0: of the four neighbours; 1: of the two of a way,
vertical where bit 20 is 1); the pixels of x + y even are *stored*, the k-th (k = (8y + x) / 2)
at bits 2k, 2k + 1, but pixel (0, 0) at bit 1 alone and, of mode 1, (4, 2) at bit 21 alone (a bit
alone repeated as two), of weights 0, 3, 5, 8 (of none punch-through); a pixel not stored, the
mean of the weights of its neighbours (`(w1 + w2 + w3 + w4 + 2) / 4`, or of a way `(w1 + w2 + 1) /
2`), each in the word it is in (wrapping at the sides): stored there, or of weight 0 or 8 by its
bit of a word of M = 0.

### 14.2 The first pass: colours

The words in turn: §11.2's choices, the head of a word its bits 32–63; a word taken whole is
done; a head taken is its colours; else new colours (§14.3). Its modulation is 0 until the second
pass.

### 14.3 New colours

L, U and UL the words to the left, above and above to the left (of the surface).

1. M: `flag[lm·3 + um]`, lm and um those of L and U (2 where there is none).
2. Of colour A then B (k = 0, 1): the opacity `opaque[(k·3 + o)·2 + (k = 1 ? A's opacity : 0)]`,
   o L's opacity of the colour (2 where there is none); then its channels in the order G, R, B, A
   (slots 0 to 3; A skipped where opaque), of n bits:

   ```text
   N      = the channel of L's, U's and UL's colour k (ARGB 4555), of `full` 4 bits (A) or 5
   pred   = MED of them where all three are; else L's, else U's; else 2^(full − 1)
   act    = min(|L − U|, 3) where both are, else 3
   p      = pred >> (full − n), plus rG (the residual of G) of R and B
   before = 0 of G, class(rG) (§11.4) of the others
   ctx    = (((k·2 + opaque)·4 + slot)·4 + act)·4 + before
   v      = token(res[ctx·16 …], res_next[ctx·16 …])                 (256 pairs)
   ```

   the residual r the value of [−2^(n − 1), 2^(n − 1)) congruent to unzz(v), the channel `(p +
   r) mod 2^n`.

### 14.4 The second pass: modulation

The words in turn: of a word not taken whole, its modulation; then its pixels set in the plane
(of 2 bits a pixel, those not stored by the mean of the weights of their neighbours of the mode
within the word, `(Σ + n/2) / n` of the n there are); and it is used (the cache's key of its
head, its bytes). After the last, of 2 bits a pixel, every pixel of the plane
set as §14.1 tells.

Of 2 bits of M = 1, the mode `bits2[0]`, and of mode 1 the way `bits2[1]`. Then each pixel p in
turn (but those not stored), at (x, y), of its value's bits n (1 or 2) and *kind*: of 4 bits a
pixel, M; of 2, of M = 0, 2; else 3. Of the images A and B there, `l2 = Σ (B_c − A_c)²` over
ARGB; Q1 and Q2 as §11.5 (the coarse and causal predictions, or the pixels above and to the left),
each of bucket `clamp(floor((Σ (Q_c − A_c)·(B_c − A_c)·8 + floor(l2 / 2)) / l2) + 1, 0, 10)`, or
11 where l2 = 0 or there is none; contrast 0 (l2 ≤ 300), 1 (≤ 3000) or 2. The value by
`tree_n(modu[(((kind·3 + contrast)·12 + a)·12 + c)·4 …])`; the pixel set in the plane (§14.1, of
2 bits a pixel of its weight alone).

### 14.5 Encoding *(encoder)*

- PVR 3 files of pixel formats 0 to 3, KTX of OpenGL's internal formats 0x8C00–0x8C03, KTX 2 of
  Vulkan's 1000054000, 1000054001, 1000054004 and 1000054005, as §11.6; Unity's textures of
  formats 30 to 33 by aqr-bundle.
- The pixels of 2 bits a pixel of M = 1 are PowerVR's decoders' (and texture2ddecoder's): their
  stored values of weights 0, 3, 5, 8, none punch-through, which the Khronos description has as
  those of 4 bits of M = 1.

## 15. `astc-recons`

- **Module**: name `astc-recons`, version 1.
- **Params**: the layout of §2, of format 48 and its footprint (fw, fh: a u8 each) one of ASTC's
  2D footprints (4×4 to 12×12).
- **Resources**, **inputs** and **output**: as §2's.

The walk is §5's, of one part (the block), its contexts and cache those of §15.2 to §15.4; the
planes §6's, of 4 channels (R, G, B, A) of 8 bits, of blocks of fw × fh pixels.

### 15.1 Blocks

A block is 16 bytes, its bits numbered from bit 0 of byte 0 up, read as ASTC's (the Khronos Data
Format Specification 1.3, §23) are: a *void-extent* block where its bits 0–8 are 0x1FC and bits
10 and 11 are 1; a *normal* block where its block mode is of a 2D grid not reserved, of at most 64
weights in 24 to 96 bits, its grid within the footprint, of no dual planes of 4 partitions, its
colour values at most 18 and of at least `ceil(13·n / 5)` bits; else an *error* block. Of a
normal block, its fields: the block mode (bits 0–10: the grid wx × wy, its weights' range, dual
planes), the partitions (bits 11–12, plus 1), the partition index (the *seed*, bits 13–22), the
endpoint modes (of one partition bits 13–16; else bits 23–24 the *selector*, 0 of one mode for
all in bits 25–28, else of each partition its class bit and 2 mode bits, bits 25–28 and the bits
just below the weights), the component selector of dual planes (2 bits below those), the colour
values (from bit 17, or 29, in the largest of ASTC's ranges, 0 to 5 up to 0 to 255,
whose integer sequence fits the bits up to the selector or the extra bits), the bits between the
colour values' end and what lies above it (*unused*), and the weights (from bit 127 down, the bit
order reversed). Of the integer sequences (ASTC's), each group of 5 trits' 8 bits, or of 3 quints'
7 bits, is the *code* of its values: those present of a group cut short (the others 0). The codes
of a group of values are those that decode to them (13 tuples of trits and 3 of quints have two,
and a group cut short may have several), ascending.

Its pixels, which the planes hold: of normal blocks, of each partition its endpoints by its mode
as ASTC decodes them of its colour values unquantized (but of the HDR modes 2, 3, 7, 11, 14 and 15
an approximation: of 2, 3 and 11 the values as they are; of 7, e1 = (v0, v1, v2) and e0 = e1 −
v3/2 of each channel; of 14 and 15 as 11, alpha v6, v7), each texel's weight by ASTC's infill of
the grid's weights unquantized (of dual planes, the second plane's of the selected component), its
partition by ASTC's function of the seed, and of each channel `(C0·(64 − i) + C1·i + 32) / 64 >> 8`
of the endpoints' channels times 257; of a void-extent block the top byte of each of its 16-bit
channels; of an error block (255, 0, 255, 255).

### 15.2 The choices

As §11.2, the head of a normal block its bits below its weights plus 2^127, of any other all 128
of its bits.

### 15.3 New blocks

Its kind by `tree_2(kind[(kl·4 + ku)·4 …])`, kl and ku the kinds of L and U (void-extent 0, error
1, normal 2; 3 where there is none); a value of 3 is normal.

- An error block: its 16 bytes, raw.
- A void-extent block: bit 9 by `void[256]`; the extents (bits 12–63) all 1 by `void[257]`, else
  their 52 bits raw from the top; then its channels k = 0 to 3 (R, G, B, A, bits 64 + 16k), each
  `(pred + unzz(v)) mod 2^16` of v by `wtoken(void[(d·4 + k)·32 …], void_next[(d·4 + k)·32 …])`
  (d bit 9; the residual of [−2^15, 2^15)), pred the channel of L where L is void-extent, else of
  bit 9 0 the mean of the pixels predicted (rounded: of the coarse predictions of all its texels,
  of mipmap m < M − 1; else of the column to its left and the row above in the plane; 0x80 of
  none) times 257, of bit 9 1 0x3C00. Bits 0–8 are 0x1FC, 10 and 11 are 1.
- A normal block (§15.4): its fields but its weights, then its weights (§15.5).

### 15.4 New normal blocks

1. The block mode: where L is normal, `same[0]` (1: L's); else where U is normal and its mode is
   not L's (where L is normal), `same[1]` (1: U's); else `tree_11(mode)`.
2. The partitions: `tree_2(np[l·4 …])` plus 1, l L's partitions minus 1 where L is normal, else 4.
3. Of several partitions: the seed, `same[2]` (1: L's) where L is normal of as many partitions,
   else `tree_10(seed)`; the selector `tree_2(cem[1280 …])`.
4. The endpoint modes: of one partition or the selector 0, one for all by
   `tree_4(cem[(np − 1)·256 + c·16 …])` (c L's first mode where L is normal, else 16); else of each
   partition p in turn, its class bit `cem[1284 + (sel − 1)·4 + min(p, 3)]` (1: the class sel, 0: sel
   − 1) and mode `tree_2(cem[1300 + (class·4 + min(p, 3))·4 …])`.
5. Of dual planes, the component `tree_2(ccs)`.
6. The colour values (their range and number by the fields so far, §15.1), each by its predicted
   value: of the first of L and U that is normal of the same partitions, modes and range, its
   value unquantized (source 0); else of the modes 0, 4, 6, 8, 10 and 12 by the pixels predicted
   of the partition (source 1): of mipmap m < M − 1 the coarse predictions of its texels, else the
   column to the left and the row above; lo the first of the least sum of R, G, B, hi the first of
   the most; of mode 0 (Llo, Lhi), 4 (Llo, Lhi, lo_A, hi_A), 8 (lo_R, hi_R, lo_G, hi_G, lo_B, hi_B),
   12 (those, lo_A, hi_A), 6 (hi_R, hi_G, hi_B, s), 10 (hi_R, hi_G, hi_B, s, lo_A, hi_A), L the sum
   divided by 3 and s = `clamp(256·sum(lo) / max(sum(hi), 1), 0, 255)`; else 128 (source 2). The
   values of a range in the order of what they unquantize to (then of their own) are its *ranks*;
   of the rank pr of the first nearest the prediction:

   ```text
   ctx  = ((source·8 + min(place, 7))·5 + min(cr/4, 4))       (place: of the partition's values)
   v    = token(colour[ctx·16 …], colour_next[ctx·16 …])
   rank = (pr + unzz(v)) mod L                                 (L the range's levels)
   ```

   Then the codes of its groups (below).
7. The unused bits from the lowest, the k-th by `unused[min(k, 7)]`.

The *codes* of a sequence's groups in turn: where its values have several codes (§15.1), the
index of its code among them by `tree_n(choice[((t·6 + k)·2 + w)·8 …])` (n the bits of their
number less one, at most that less one; t 1 of trits, 0 of quints; k the values of the group; w 1
of weights, 0 of colour values).

*(encoder)* The residual is of ranks of [−L/2, L/2): `d = (rank − pr) mod L`, less L where d ≥
(L + 1)/2.

### 15.5 Weights

The endpoints of each partition (§15.1) decoded. Of each weight i of the grid's values (grid point
g = i / planes at (gx, gy), plane i mod planes): its texel (tx, ty) = `((gx·(fw − 1) + (wx − 1)/2)
/ (wx − 1), …)` (0 of wx = 1, and likewise of y); e0, e1 the endpoints of its partition there;
the channels all but the selected component of plane 0, it alone of plane 1 (all of no dual
planes). Of mipmap m < M − 1, wp = `clamp(floor((Σ (Q_c − e0_c)·(e1_c − e0_c)·64 + floor(l2/2)) /
l2), 0, 64)` of Q the coarse prediction at the texel, where l2 = Σ (e1_c − e0_c)² > 0; and m the MED
of the weights (unquantized) to the left, above and above to the left in the grid (of the same
plane), else the one there is of the left and upper ones. The prediction and its agreement a: wp
and min(|wp − m| / 12, 2); wp and 2 of no m; m and 3 of no wp; 32 and 3 of neither. The weight
by its rank as the colour values', `ctx = (wr·4 + a)·2 + plane` (wr the range's index) of
`weight` and `weight_next`; then the codes of its groups.

### 15.6 Encoding *(encoder)*

- `.astc` files (astcenc's) of 2D footprints, KTX files of OpenGL's internal formats 0x93B0–0x93BD
  and 0x93D0–0x93DD, KTX 2 of Vulkan's 157–184 and 1000066000–1000066013, PVR 3 of pixel formats
  27–40, as §11.6; Unity's textures of formats 48–59 and 66–71 by aqr-bundle.
