# The `bcn-recons` module

This document specifies the module `bcn-recons` of aqr-formula entry formulas. The module
rebuilds the blocks of a texture compressed in BC1, BC2, BC3, BC4 or BC5 (DXT1 to DXT5, ATI1 and
ATI2) from one coded stream: each block's endpoints, as those of a block before or as new, and
its indices, as levels on the line between its endpoints, under contexts of where the pixels
around (decoded so far, and those of the next smaller mipmap) predict them. The document is
meant to be complete: an implementation of the module can be written from it alone. Sections
marked *(encoder)* describe the choices this implementation makes when it builds formulas; the
module does not depend on them.

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
  u8      format   1: BC1, 2: BC2, 3: BC3, 4: BC4, 5: BC5
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
