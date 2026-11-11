# aqr-lzx format

aqr-lzx splits an LZX stream into its decompressed **data** and a **record** of everything else
its bits say: how it is cut into blocks, its Huffman trees and how the pretree writes them, its
matches, its padding. From the data and the record the stream is rebuilt bit for bit. This
document describes the streams taken, the record, and the [formula module](#5-formula-module)
`lzx-recons` that rebuilds streams from data and records.

No encoder is reproduced. The record codes the parse given the data, under contexts of what a
match finder sees in the data; the trees are coded against Huffman codes of the block's
frequencies. Any stream that reads whole (§3.6) has a record.

The document is normative and complete: an implementation of the module can be written from it
alone. Sections marked *informative* describe the reference encoder's choices; a decoder needs
none of them.

External reference: LZX as Microsoft's Open Specifications describe it (MS-PATCH, "LZX DELTA
Compression and Decompression", of which plain LZX is the part without a reference). §3 restates
what this document needs of it. aqr-lzx was written from that description, without any LZX
implementation's code.

Contents:

1. [Conventions](#1-conventions)
2. [Overview](#2-overview)
3. [LZX streams](#3-lzx-streams)
4. [The record](#4-the-record)
5. [Formula module](#5-formula-module)
6. [Encoder choices](#6-encoder-choices-informative) *(informative)*

## 1. Conventions

- Integers are unsigned unless said otherwise. `le32` is a little-endian 32-bit integer.
  Arithmetic is exact unless marked *wrapping* (modulo 2^32).
- `D` is the data of a stream (or chunk) as LZX codes it, `N` its length; `D[a..b)` are the
  bytes from `a` to `b − 1`.
- `lcp(a, b, cap)`, `a ≤ b`: the length of the common prefix of `D[a..N)` and `D[b..N)`, at most
  `cap` (and at most `N − b`).
- **varint**: unsigned LEB128, least significant group first (the encoder writes the shortest
  form).
- `lb(n)`, the bucket of a length: 0 for 0 and 1, 1 for 2, 2 for 3, 3 for 4–5, 4 for 6–8, 5 for
  9–16, 6 for 17–32, 7 above.
- `NONE` is a position that is none.

## 2. Overview

LZX comes in two **variants** here:

- **WIM**: the chunks of a WIM file's resource. The resource's data is cut into chunks of
  32768 bytes (the last one shorter), each compressed on its own with a window of 32 KiB and e8
  translation of size 12000000; a chunk as long as its data is **stored** (the data as it is).
  A *stream* of the WIM variant is a sequence of chunks, one after another.
- **CAB**: a stream as CAB folders and CHM sections hold it, with a window of `2^w` bytes,
  `15 ≤ w ≤ 21`: a header telling the e8 translation's size (or none), then blocks over frames
  of 32768 bytes of data, the bitstream aligned to a word at the end of each frame. A *stream*
  of the CAB variant is one such stream.

The **parse** of an LZX stream (or chunk) is everything its bits say besides the data (§3.5).
The data and the parse determine the stream, and the stream determines both. The record codes
the parse given the data.

The data the module is given is the data as the file holds it, e8 translation undone (§3.4); the
module translates it again before it walks it.

## 3. LZX streams

### 3.1 Bits

The bitstream is a sequence of 16-bit little-endian words; bits are taken from the top bit of
each word down. A value of `k` bits is written highest bit first. **Aligning** to a word writes
`16 − r` bits of padding, where `r` (1 to 15) bits of the current word are written; it writes
nothing where `r = 0`.

### 3.2 Constants of the variant

- The window `W = 2^15` (WIM) or `2^w` (CAB). A match reaches at most `W − 3` back.
- Position slots `S`: `2w` for `w ≤ 19`, 42 for `w = 20`, 50 for `w = 21` (30 for WIM).
- `EXTRA[s]`, the extra bits of slot `s`: 0 for `s < 4`, `⌊(s − 2) / 2⌋` for `4 ≤ s < 36`, and
  17 above. `BASE[0] = 0`, `BASE[s] = BASE[s − 1] + 2^EXTRA[s − 1]`.
- The **slot** of a formatted offset `f ≥ 3` is the greatest `s` with `BASE[s] ≤ f`.
- The main tree has `M = 256 + 8S` symbols, the length tree 249, the aligned tree 8, the
  pretree 20.

### 3.3 Huffman codes

A tree is given by its code lengths (0: no code). Lengths are at most 16 (the pretree's at most
15, the aligned tree's at most 7). The code is canonical: codes are assigned in order of length,
then symbol, each length's first code being `(first code of the length before + count of that
length) << 1`, starting from 0 at length 1. Lengths that oversubscribe the code (more codes of a
length than there is room for) are not valid; a code that does not fill its space is valid.
Writing a symbol of length 0 is not valid.

### 3.4 e8 translation

The translation of a piece `P` of data starting at position `s` of its stream, with size `z`
(a signed 32-bit integer), is done in place where `|P| > 10`:

```text
i = 0
while i < |P| − 10:
  if P[i] = E8:
    at = s + i;  v = P[i+1..i+5) as a signed le32
    translating (the file's data into LZX's):
      if −at ≤ v < z:  v = (v + at if v < z − at, else v − z)
    undoing (LZX's data into the file's):
      if −at ≤ v < z:  v = (v − at if v ≥ 0, else v + z)
    P[i+1..i+5) = v;  i += 5
  else i += 1
```

Each maps the values in `[−at, z)` one to one onto themselves and leaves others alone, and the
positions it looks at are the same either way, so each undoes the other.

- WIM: each LZX chunk's data is one piece, `s = 0`, `z = 12000000`. A stored chunk is not
  translated.
- CAB: where the header tells a size `z`, each frame `[32768k, min(32768(k + 1), N))` of the
  stream's first 2^30 bytes is a piece, `s = 32768k`. Without a header size, nothing is
  translated.

### 3.5 The parse

A parse is:

- (CAB) `e8`: the header's size, or none;
- the blocks, in order, each with:
  - `type`: 1 verbatim, 2 aligned, 3 uncompressed;
  - `size ≥ 1`; and (WIM) `default`, whether it is written as the bit telling 32768;
  - verbatim and aligned blocks: (aligned) the aligned tree's 8 lengths; three **tree codes**,
    of the main tree's first 256 lengths, of its other `M − 256`, and of the length tree's 249;
    the block's tokens;
  - uncompressed blocks: `align`, the bits padding to a word; `reps`, the three repeat offsets
    written (each a 32-bit value); `pad`, the byte after an odd size, or none;
- the **tokens**: each a literal, or a match `(len, off, rep)` with `2 ≤ len ≤ 257`, `off ≥ 1`
  and `rep` one of 0, 1, 2 (the repeat slot it is coded as) or none (a new offset);
- (CAB) the frames' padding: a value for each frame but the last whose end a token reaches;
- `tail`, the bits padding the last word; `trailing`, the bytes after it.

A **tree code** is the pretree's 20 lengths and the pretree symbols, each with its extra:

| symbol | extra | writes at `i` (`L` the lengths before the block) |
|---|---|---|
| `x ≤ 16` | | one length, `(L[i] + 17 − x) mod 17` |
| 17 | 4 bits `e` | `4 + e` zeros |
| 18 | 5 bits `e` | `20 + e` zeros |
| 19 | 1 bit `e`, then a symbol `y ≤ 16` | `4 + e` lengths, each `(L[i] + 17 − y) mod 17` |

A tree code must write exactly the lengths of its part. The lengths before the first block of a
stream (or WIM chunk) are all zero; each verbatim or aligned block's become those before the
next; uncompressed blocks leave them as they are.

The **repeat offsets** `R` are `[1, 1, 1]` as a stream (or WIM chunk) starts. A match of `rep = k`
swaps `R[0]` and `R[k]` and has `off = R[0]`. A new offset sets `R = [off, R[0], R[1]]`. An
uncompressed block sets `R` to its `reps`.

### 3.6 The stream of a parse

The stream of data `D` (as LZX codes it, e8 translation done) and a parse, with `at = 0`:

1. (CAB) one bit: whether `e8` is given; if so, its 16 high bits, then its 16 low bits.
   (WIM: `e8` must be none, and there is no frame padding.)
2. Each block:
   - 3 bits `type`.
   - The size. WIM: one bit `default`; where set, `size` must be 32768; else 16 bits `size`
     (below 65536). CAB: `default` must be clear and `size < 2^24`; 16 bits `size >> 8`, then 8
     bits `size & 255`.
   - `end = min(at + size, N)` (the last block may say more than is left).
   - Verbatim or aligned: (aligned) 8 × 3 bits, the aligned tree's lengths. Then each of the
     three tree codes: the pretree's 20 lengths, 4 bits each, then its symbols, each written by
     the pretree's code, followed by its extra (19's `y` by the pretree's code too). The lengths
     are updated (§3.5). Then each token of the block:
     - a literal: the main tree's code of `D[at]`; `at += 1`.
     - a match: `h = min(len − 2, 7)`; the slot is `rep` where given (and `R[0]` after the swap
       must be `off`), else that of `f = off + 2`; `R` is updated. It must hold that
       `1 ≤ off ≤ min(at, W − 3)`, `at + len ≤ end`, and `D[at − off + j] = D[at + j]` for
       `j < len` (copied one byte at a time). Written: the main tree's code of
       `256 + 8 × slot + h`; for `h = 7`, the length tree's code of `len − 9`; for a new offset,
       with `e = EXTRA[slot]` and `x = f − BASE[slot]`: in an aligned block with `e ≥ 3`, `e − 3`
       bits of `x >> 3` then the aligned tree's code of `x & 7`; else `e` bits of `x`.
       `at += len`.
     - (CAB) after the token, if `at ≥ frame_end` and `frame_end < N`: align to a word with the
       next frame padding (which must fit the bits; 0 where there are none), and
       `frame_end = min(frame_end + 32768, N)`. `frame_end` starts at `min(32768, N)`.

     The block must end at `end`.
   - Uncompressed: `16 − r` bits of `align`, `r` the bits of the current word written (16 bits
     where it is aligned already); `align` must fit them. Then the bytes: `le32` of each of
     `reps`, `D[at..end)` (which must be `size` bytes, or reach `N`), and, for an odd `size`,
     `pad` where given. The bitstream goes on from the next byte, at a word's start.
     `at = end`; (CAB) `frame_end` steps by 32768 (at most to `N`) while `frame_end ≤ at` and
     `frame_end < N`.
3. Every byte of `D`, every token and every frame padding must be used. If the last word is
   partly written, its other `k` bits are the low `k` bits of `tail`. Then `trailing`.

A stream (or WIM chunk) is taken where reading it gives a parse and data whose stream is the
stream itself. Reading follows the steps above: a block's data ends at `end`, an uncompressed
block's `pad` is none only where the stream ends there, `tail` is what is left of the last word
read, `trailing` the bytes after it.

## 4. The record

A stream's record is a binary arithmetic code of its parse given its data, its contexts going on
from WIM chunk to WIM chunk within the stream.

### 4.1 Coding

The record is a binary arithmetic code (carry-less, 32 bits):

```text
x1 = 0, x2 = 2^32 − 1;  the decoder reads 4 bytes into x (zeros past the end)
bit with probability p1 of a 1 (16 bits):
  xmid = x1 + (((x2 − x1) × p1) >> 16)
  1: x2 = xmid;  0: x1 = xmid + 1        (the decoder reads 1 iff x ≤ xmid)
  while x1 and x2 agree in their top byte:
    the encoder writes x2 >> 24; x1 <<= 8; x2 = x2 << 8 | 0xFF; the decoder shifts in a byte
end: the encoder writes (x1 >> 24) + 1
```

A decoder that has read more than 4 bytes past the end of the record, after a WIM chunk or at
the end of a CAB stream, fails.

Each **adaptive bit** has `p1` (initially 32768) and a count `c` (initially 0). After coding a
bit: `p1 += floor((t − p1) × RATE[c] / 65536)` with `t = 65535` for a 1 and 0 for a 0,
`RATE[i] = floor(131072 / (2i + 3))`, then `p1` is clamped to 32 … 65504 and `c`, if below 30,
grows by 1. A **raw bit** has `p1 = 32768` and is not adapted; the raw bits of a value go
highest first.

- `gamma(t)`, `t` a table of 32 adaptive bits, codes `v` as `x = v + 1`, `b = bitlen(x) − 1`:
  `b` ones then a zero, the `i`-th of them (from 0) with `t[min(i, 15)]`; decoding fails at a
  31st one. Then the `b` bits of `x` below its top one, highest first: the first with
  `t[16 + min(b, 15)]`, the others with `t[31]`.
- `tree5(t)`, `t` a table of 32 adaptive bits, codes `v < 32` highest bit first, each bit with
  `t[m]`, `m` starting at 1 and becoming `2m + bit`.

All contexts are fresh at the start of a stream's record:

| context | size | index |
|---|---|---|
| `stored` | 1 | |
| `kind` | 4 × 2 | `prev × 2 + j` |
| `deflt` | 4 | `k` |
| `whole` | 4 | `k` |
| `is_match` | 8 × 8 × 4 | `(lb(longest) × 8 + lb(rl0)) × 4 + min(lb(rl1), 3)` |
| `is_rep` | as `is_match` | as `is_match` |
| `rep0` | 8 × 8 | `lb(longest) × 8 + lb(rl0)` |
| `rep1` | 8 × 8 | `lb(rl1) × 8 + lb(rl2)` |
| `len_eq` | 4 × 8 | `q = kind × 8 + lb(pred)` |
| `len_short` | 4 × 8 | `q` |
| `len_by` | 2 × 4 × 8 (gamma) | `q` (shorter), `32 + q` (longer) |
| `escape` | 8 | `lb(len)` |
| `rank` | 8 × 4 (gamma) | `lb(len) × 4 + min(lb(longest), 3)` |
| `tree_eq` | 17 × 3 | `pred × 3 + tree` |
| `tree_val` | 17 × 3 (tree5) | `pred × 3 + tree` |
| `pre_eq` | 16 | `pred` |
| `pre_val` | 16 (tree5) | `pred` |
| `run` | 8 × 8 × 2 | `u` (below) |
| `run18` | 8 × 8 × 2 | `u` |
| `run_kind` | 8 × 8 × 2 | `u` |
| `run_full` | 2 | `sym − 17` |
| `run_n` | 1 (tree5) | |
| `same5` | 1 | |
| `z0` … `z6` | 1 each | |
| `trail` | 1 (gamma) | |

### 4.2 The match finder

The contexts of tokens come from a match finder over `D`, with a **reach** `F`: 32768 (WIM) or
`W − 3` (CAB). It is reset at the start of each WIM chunk and of each CAB stream.

- **Chain 2**: heads `head2[65536]` by `k2(p) = D[p] | D[p+1] << 8`, for `p + 2 ≤ N`.
- **Chain 3**: heads `head3[65536]` by `h3(p) = (k3(p) × 0x9E3779B1, wrapping) >> 16`, where
  `k3(p) = D[p] | D[p+1] << 8 | D[p+2] << 16`, for `p + 3 ≤ N`.
- Each has a ring of links `prev[W]`. Every entry starts as `NONE`.

Inserting `p`: if `p + 3 ≤ N`, `prev3[p mod W] = head3[h3(p)]` and `head3[h3(p)] = p`; then if
`p + 2 ≤ N`, `prev2[p mod W] = head2[k2(p)]` and `head2[k2(p)] = p`. Before the walk looks at
`pos` (§4.4), every position below `pos` is inserted, in order.

At `pos`, a candidate `c` is *near* if `c ≠ NONE` and `pos − c ≤ F`. The **candidates** of a
chain start at its head for `pos` and go on by `prev[c mod W]` while they are near.

- **longest(pos, cap)**, `cap = min(end − pos, 257)`: 0 if `cap < 2`. Else the greatest of:
  `lcp(c, pos, cap)` for `c = head2[k2(pos)]` if `pos + 2 ≤ N`, `c` is near and that is at least 2;
  and, if `cap ≥ 3`, `lcp(c, pos, cap)` for each of the first 32 candidates `c` of chain 3 (from
  `head3[h3(pos)]`); 0 if none. (It may be 1; 0 and 1 are alike everywhere they are used.)
- **offsets(pos, n)**: the chain is chain 3 (from `head3[h3(pos)]`) if `n ≥ 3` and
  `pos + 3 ≤ N`, else chain 2 (from `head2[k2(pos)]`) if `pos + 2 ≤ N`, else none. Of its first
  4096 candidates `c`, in order, those with `lcp(c, pos, n) ≥ n` give the offsets `pos − c`
  (nearest first).

### 4.3 A chunk or stream

A record starts with three **capacities**, `T`, `P` and `B`, three gammas under one table of 32
adaptive bits of their own (fresh at the start, and not `trail`): the most tokens of any block of
the stream, the most frames' padding of any block (§4.4), and the most bytes of data any block
covers (its size, or what is left of the stream where that is less). `T` and `B` are at most the
length `N` of the stream's data and `P` at most `N / 32768 + 2`; a block of more tokens than
`T`, more padding than `P` or covering more than `B` bytes fails. They let a decoder reserve
the tokens and padding of a block, and the data it holds of a CAB stream, once (module version 3;
version 2 had `T` and `P` only, version 1 none). The encoder writes the greatest of its blocks'.

The record of a WIM stream is, for each chunk of its data (32768 bytes each, the last shorter),
the chunk's part below; the record of a CAB stream is one part, over its whole data. Within a
part, `piece` is the chunk's (or stream's) data as the module is given it:

```text
(WIM) bit stored: 1 → the chunk is stored: its output is piece as it is; the part ends
(CAB) bit z5: 1 → e8 = 16 raw bits (high) then 16 raw bits (low); 0 → none
D = piece, e8-translated (§3.4);  N = |D|;  reset the match finder;  R = [1, 1, 1]
lengths L all zero;  prev = 0;  last = 32768;  at = 0;  (CAB) frame_end = min(32768, N)
while at < N: a block
  type: bit kind[prev × 2 + 0]: 0 → k = 0;  1 → k = 1 + bit kind[prev × 2 + 1]
        type = k + 1;  prev = k + 1
  size, left = N − at:
    (WIM) bit deflt[k]: 1 → default, size = 32768
    else bit whole[k]: 1 → size = left
    else (CAB) bit deflt[k]: 1 → size = last
    else (WIM) 16 raw bits;  (CAB) 8 raw bits (high) then 16 raw bits (low);  fails if 0
    last = size;  end = min(at + size, N)
  uncompressed:
    bit z0: 1 → align = 16 raw bits;  0 → align = 0
    bit z1: 1 → reps = R;  0 → each of the three: 16 raw bits (high) then 16 raw bits (low)
    (CAB) fails if a rep is 0 or above W − 3
    R = reps
    if size is odd: bit z2: 0 → pad none;  1 → bit z3: 1 → pad = 8 raw bits;  0 → pad = 0
    at = end;  (CAB) frame_end steps as in §3.6
  verbatim or aligned:
    the tokens of D[at..end) (§4.4)
    the trees (§4.5)
    at = end
tail: bit z4: 1 → 16 raw bits;  0 → 0
trailing: n = gamma(trail), fails if n > 32768;  then n bytes, 8 raw bits each
the output: the stream (§3.6) of D and the parse;  fails where it does not hold
```

### 4.4 Tokens

```text
pos = at
while pos < end:
  insert the positions below pos
  cap = min(end − pos, 257);  longest = longest(pos, cap)
  rl_k = lcp(pos − R[k], pos, cap) if R[k] ≤ pos, else 0      (k = 0, 1, 2)
  bit is_match[...]: 0 → a literal; pos += 1; the frame (below); next
  rep = none
  if some rl_k ≥ 2 and bit is_rep[...] = 1:
    bit rep0[...]: 1 → rep = 0;  0 → bit rep1[...]: 1 → rep = 1;  0 → rep = 2
  (kind, pred) = (rep, rl_rep) for a repeat, (3, longest) for a new offset
  len, q = kind × 8 + lb(pred):
    if pred ≥ 2 and bit len_eq[q] = 1: len = pred
    else if pred > 2 and (kind < 3 or bit len_short[q] = 1):
      len = pred − 1 − gamma(len_by[q]);  fails if below 0
    else: len = max(pred, 1) + 1 + gamma(len_by[32 + q])
    fails unless 2 ≤ len ≤ cap
  off:
    a repeat k: swap R[0] and R[k];  off = R[0]
    new: bit escape[lb(len)]: 1 → off = 15 raw bits (WIM), w raw bits (CAB)
                              0 → r = gamma(rank[...]);  off = the r-th (from 0) of
                                  offsets(pos, len);  fails if there is none
         fails unless 1 ≤ off ≤ min(pos, F);  R = [off, R[0], R[1]]
  fails unless 1 ≤ off ≤ pos and lcp(pos − off, pos, len) ≥ len
  the token (len, off, rep);  pos += len;  the frame (below)

the frame (CAB): if pos ≥ frame_end and frame_end < N:
  bit z6: 1 → the frame's padding = 16 raw bits;  0 → 0
  frame_end = min(frame_end + 32768, N)
```

### 4.5 Trees

`huffman(freq, limit)` gives the code lengths of a Huffman code of the frequencies `freq`, at
most `limit`:

1. The symbols of nonzero frequency, in order, are the leaves. None: every length is 0. One:
   its length is 1, the others' 0.
2. Two queues: the leaves sorted by (frequency, symbol), and the merged nodes in the order they
   are made. Each of `n − 1` steps takes two nodes, one after the other, each the head of the
   leaf queue where its frequency is at most that of the head of the merged queue (or that is
   empty), else the head of the merged queue; and appends a node of their summed frequency to the
   merged queue. A leaf's length is its depth below the last node.
3. If a length is above `limit`: every length is cut to `limit`; then, while
   `Σ 2^(limit − length) > 2^limit`, the first leaf in order of (length descending, frequency,
   symbol) whose length is below `limit` is lengthened by 1.

After a block's tokens, with `p` running over `D[at..end)`, the frequencies are counted: a
literal counts `fm[D[p]]`; a match counts `fm[256 + 8 × slot + h]` (slot as in §3.6), for
`h = 7` `fl[len − 9]`, and for a new offset with `EXTRA[slot] ≥ 3`,
`fa[(off + 2 − BASE[slot]) & 7]`.

A **length** against a prediction `pred`, of tree `tree` (0 main, 1 length, 2 aligned), at most
`max`: bit `tree_eq[pred × 3 + tree]`: 1 → `pred`; 0 → `x = tree5(tree_val[pred × 3 + tree])`,
which fails if `x > max` or `x = pred`.

```text
(aligned) P = huffman(fa, 7);  the aligned tree's lengths: length(P[i], 2, 7), i = 0 … 7
P = huffman(fm[0..M), 16);  T[i] = length(P[i], 0, 16), i = 0 … M − 1
Q = huffman(fl, 16);        U[i] = length(Q[i], 1, 16), i = 0 … 248
the tree codes of T[0..256) over L[0..256), T[256..M) over L[256..M), U over the length tree's L
L = T and U
```

The **tree code** of target lengths `T` over lengths before `B` (both of the part):

```text
i = 0
while i < |T|:
  zeros = the count of 0 from T[i];  same = the count of T[i] from T[i]
  u = (lb(zeros) × 8 + lb(same)) × 2 + [T[i] = 0]
  if same ≥ 4 and bit run[u] = 1: a run
    if zeros ≥ 4 and bit run_kind[u] = 1: zeros:
      sym = 17 if zeros < 20, else (bit run18[u]: 1 → 18, 0 → 17)
      (lo, hi) = (4, 19) for 17, (20, 51) for 18;  full = min(zeros, hi)
      bit run_full[sym − 17]: 1 → n = full;  0 → n = lo + tree5(run_n)
    else: sym = 19;  full = min(same, 5)
      n = 4 if full < 5, else 4 + bit same5
      (lo, hi) = (4, 5)
    fails unless lo ≤ n ≤ max(full, lo) and i + n ≤ |T|
    the symbol sym, extra n − lo;  for 19, y = (B[i] + 17 − T[i]) mod 17
    i += n
  else: the symbol (B[i] + 17 − T[i]) mod 17;  i += 1
the pretree's lengths: P = huffman(g, 15), g counting each symbol and each 19's y
  for s = 0 … 19: bit pre_eq[P[s]]: 1 → P[s];  0 → tree5(pre_val[P[s]]), fails if above 15
```

(A run of zeros, `zeros ≥ 4`, has `same = zeros`; `run_kind` is coded wherever it can be either.)

## 5. Formula module

The module is `lzx-recons`, version 3 (version 2 had two capacities in the record and was a whole
module; version 1 had none, §4.3). It is a **part module** (spec, "Module execution"): its inputs
are read and its output written in order through windows, so that it holds the window and the
block being coded of a CAB stream, never the stream. It rebuilds one or more streams, each from
its data and its record, between bytes it is given as they are: its output is a part of a
container as it is.

**Parameters**: none for streams of the WIM variant; one byte `w`, `15 ≤ w ≤ 21`, for streams of
the CAB variant with a window of `2^w`. Anything else fails.

**Inputs**: exactly four, in this order.

| slot | content |
|---|---|
| 0 | the data of every stream (as the file holds it, e8 undone), one after another |
| 1 | the records of every stream, one after another |
| 2 | the bytes kept: before each stream, then after the last |
| 3 | the index: for each stream, three varints: the bytes kept before it, the length of its data, the length of its record |

The index cuts inputs 2, 0 and 1 in that order, from their starts; inputs 0 and 1 must be used
up exactly, and what is left of input 2 is the bytes kept after the last stream.

The output is, for each stream in order, the bytes kept before it, then the stream its data and
record rebuild (§4); then the bytes kept after the last. Streams are rebuilt independently of
each other (each record's contexts fresh). The output must be the application's declared size.

**Resources**: none.

The module fails on other parameters, another count of inputs or resources, an index that does
not cut the inputs exactly, a record that fails (§4), or an output of another length.

**Windows**: 16 pages each of the data, records, bytes kept and the output; one page of the index.

**Data of a CAB stream** *(normative for what the module reads)*: a block's tokens are walked over
the block's data and the 8 bytes after it, so the module reads the data frame by frame (32,768
bytes, the last fewer), e8 translated as each is read, as far as that needs, keeps the window
before the block (`2^w − 3` bytes at least) and drops what is before it. It holds
at most `2 · 2^w + B + 8 + 32768` bytes of the stream (or all of it where it is shorter): it slides
the data on by a multiple of `2^w` (at most the bytes before the block's start less `2^w − 3`)
when the next block's data and the 8 bytes after it would not fit. The match finder's positions
move with it, those left of the new start dropped (they are out of reach of every position to
come); the result is the same as when the data is held whole. A block covering more than `B`
bytes, or past what the data holds, fails.

**Memory** *(informative)*: the module lays its memory out as a stack (`aqr_api::stack`), and
the plan of its memory is exact (`aqr-lzx`'s `plan::decode`): the pieces of the readers of the
index (256 bytes), the records and the bytes kept (16 KiB each), the match finder (four bytes
each of the two heads of 65,536 and the two rings of the window), made once for all the streams;
for each stream its contexts, the code lengths (twice), the tokens (8 bytes each, `T`) and the
padding (2 bytes each, `P`) of a block, the data held (above; WIM's: a chunk twice, as it is read
and as it is e8 translated), the writer's output (32 KiB, put out whenever 16 KiB of it are
written), and 128 KiB (`SCRATCH`) for the work of a block's trees (taken once and given back, so
that the plan does not depend on the trees). A block is written as soon as its record is read.

## 6. Encoder choices *(informative)*

- `recons::split` reads each chunk of a WIM resource; a chunk as long as its data is stored. It
  fails where a chunk does not read whole or the chunks are not 32768 bytes of data but the
  last. `recons::split_stream` reads a CAB stream. Both code the parse read, so the record
  gives it back exactly; what the record leaves to the walk (the trees' symbols where no run is
  taken, the runs' values) is checked against it.
- A new offset is coded by its rank among those `offsets` gives for its length, and escaped
  where it is not among them (beyond 4096 candidates, or on another chain).
- The record's bytes go mostly to whether a token is a match and to its length; offsets, coded
  by rank, cost little. install.wim of a Windows Server 2008 R2 DVD: of 171.0 MB of records,
  91.1 MB kinds, 72.7 MB lengths, 6.3 MB trees, 0.8 MB offsets, 0.1 MB the rest.
- aqr-wim (WIM files) and aqr-mszip (CAB and CHM files) make the applications: see their
  `FORMAT.md`s.
