# aqr-lzo format

aqr-lzo splits an LZO1X stream into its decompressed **data** and a **record** of the parse its
compressor chose. From the data and the record the stream is rebuilt bit for bit. This document
describes the record, and the [formula module](#7-formula-module) that rebuilds streams from data
and records.

The record does not reproduce any compressor. It codes the compressor's matches given the data,
under contexts of what a match finder sees in the data. Any LZO1X stream that ends as the
compressors end it has a record. Those of LZO1X-1 and its variants (whose matches are nearly
always the nearest ones outside earlier matches) and of LZO1X-999 (the nearest ones of their
lengths) cost a few percent of the stream.

The document is normative and complete: an implementation of the module can be written from it
alone. Sections marked *informative* describe the reference encoder's choices; a decoder needs
none of them.

External reference: the LZO1X stream format as the Linux kernel documents it
(`Documentation/staging/lzo.rst`, "LZO stream format as understood by Linux's LZO
decompressor"). §3 restates what this document needs of it. aqr-lzo was written from that
description and from the output of liblzo2 and lzop used as black boxes, without LZO's code.

Contents:

1. [Conventions](#1-conventions)
2. [Overview](#2-overview)
3. [LZO1X streams](#3-lzo1x-streams)
4. [The match finder](#4-the-match-finder)
5. [The record](#5-the-record)
6. [lzop files](#6-lzop-files-informative) *(informative)*
7. [Formula module](#7-formula-module)
8. [Encoder choices](#8-encoder-choices-informative) *(informative)*

## 1. Conventions

- Integers are unsigned. `le16` is a little-endian 16-bit integer, `be32` a big-endian 32-bit
  one. Arithmetic is exact unless marked *wrapping* (modulo 2^32).
- `D` is the data and `N` its length. `D[a..b)` are the bytes from `a` to `b − 1`.
- `bitlen(x)` is the number of bits of `x` (0 for 0).
- `lcp(a, b)`, `a < b`: the length of the common prefix of `D[a..N)` and `D[b..N)`.
- **varint**: unsigned LEB128, least significant group first, in the shortest form.
- `W = 49151`, the farthest a match reaches back. `NONE` is a position that is none.

## 2. Overview

A stream is a sequence of literal runs and matches. The **parse** of a stream is:

- the matches, in order, each with `lits` (the literals before it), its length `n ≥ 2`, its
  distance `d ≥ 1` and its **form**, *short* or *long* (§3.3);
- `tail`, the literals after the last match;
- `first_short`, how the first run is written (§3.2).

The parse covers `D` exactly. With `D`, it determines the stream (§3.4), and the stream
determines it: an LZO1X stream's bytes are its parse. The record codes the parse given `D`.

## 3. LZO1X streams

### 3.1 The state

Before each instruction, the decoder's **state** is the number of literals just copied, up to
4: `state(r) = min(r, 4)` after a run of `r` literals (0 right after a match that copied none).
A match after `r` literals is decoded under `state(r)`.

### 3.2 Literal runs

- The first run of `r` literals (`r ≥ 1`) is written in one of two ways:
  - *first byte*: the byte `17 + r`, then the literals; for `1 ≤ r ≤ 238`;
  - *run instruction*: as below; for `r ≥ 4`.

  `first_short` says which. It is set if `r ≤ 3` and clear if `r > 238`. An empty `D` has no
  run, and `first_short` is clear.
- After a match, a run of `r` literals is written:
  - for `r = 0`: nothing;
  - for `1 ≤ r ≤ 3`: in the 2 low bits `S` of the match (below), then the literals;
  - for `r ≥ 4`: the run instruction `0000LLLL` with `r − 3` as a length of 4 bits (§3.3),
    then the literals.

### 3.3 Matches

A **length** `v ≥ 1` in a field of `b` bits (mask `M = 2^b − 1`) is written: if `v ≤ M`, `v` in
the field. Else 0 in the field, then `⌊(v − M − 1) / 255⌋` bytes 0, then the byte
`v − M − 255 × (that count)`, which is 1 to 255.

A match `(n, d)` after `r` literals is written in one of these forms. `S` is the run after it
(`1 ≤ r' ≤ 3`) or 0.

| form | condition | bytes |
|---|---|---|
| short (M1) | `1 ≤ state(r) ≤ 3`, `n = 2`, `d ≤ 1024` | `0000 DD SS`, `H`; with `d − 1 = 4H + DD` |
| short (M1) | `state(r) = 4`, `n = 3`, `2049 ≤ d ≤ 3072` | `0000 DD SS`, `H`; with `d − 2049 = 4H + DD` |
| short (M2) | `3 ≤ n ≤ 4`, `d ≤ 2048` (and not as M1) | `01 L DDD SS`, `H`; `L = n − 3`, `d − 1 = 8H + DDD` |
| short (M2) | `5 ≤ n ≤ 8`, `d ≤ 2048` | `1 LL DDD SS`, `H`; `LL = n − 5`, `d − 1 = 8H + DDD` |
| long (M3) | `n ≥ 3`, `d ≤ 16384` | `001` and `n − 2` as a length of 5 bits; `le16 ((d − 1) << 2 \| S)` |
| long (M4) | `n ≥ 3`, `16385 ≤ d ≤ W` | `0001 H` and `n − 2` as a length of 3 bits; `le16 ((d − 16384 − 16384H) << 2 \| S)` |

A match *fits* a form where the conditions of one of its rows hold; `short_fits` and
`long_fits` below. Where both fit, the parse says which is written. A match that fits neither
is not valid.

*Note.* Short forms are 2 bytes and M3 at least 3, so a compressor that minimises size writes
the short form where it fits. LZO1X-999 does not always.

### 3.4 The stream of a parse

1. The first run (§3.2), then each match and the run after it (§3.2, §3.3), in order.
2. The end: the bytes `11 00 00` (M4 of distance 16384).

The decoder copies `n` bytes from `d` back one at a time, so `d < n` repeats the last `d`
bytes. A stream ends at its end bytes.

### 3.5 Streams taken

A stream is split only where decoding it gives a parse whose stream (§3.4) is the stream itself.
Streams that end otherwise (another length or `S` in the end's M4) are not taken; neither are
LZO-RLE's (first byte 17 and a version byte), which fail as a match before any data.

## 4. The match finder

The record's contexts come from a match finder over `D`, in one of two **modes**: in mode 0 it
holds every position before the current one; in mode **skip**, those that do not lie inside a
match (a match at `p` of length `n` hides `p + 1 … p + n − 1`).

### 4.1 Chains

- **Chain 2**: `head2[65536]` by `k2(p) = D[p] | D[p+1] << 8`, for `p + 2 ≤ N`.
- **Chain 3**: `head3[65536]` by `h3(p) = (k3(p) × 0x9E3779B1, wrapping) >> 16`, where
  `k3(p) = D[p] | D[p+1] << 8 | D[p+2] << 16`, for `p + 3 ≤ N`.
- Each has a ring `prev[65536]`. Every entry starts as `NONE`.

Inserting `p`: if `p + 3 ≤ N`, `prev3[p mod 65536] = head3[h3(p)]` and `head3[h3(p)] = p`; then
if `p + 2 ≤ N`, `prev2[p mod 65536] = head2[k2(p)]` and `head2[k2(p)] = p`. Positions are
inserted in order: before the walk (§5) looks at `pos`, every position below `pos` that the mode
holds.

### 4.2 Queries at `pos`

- `reach(c)`: `c` if `c ≠ NONE` and `pos − c ≤ W`, else none.
- `near2 = reach(head2[k2(pos)])` if `pos + 2 ≤ N`, else none. `near3` likewise with `head3`,
  `h3` and `pos + 3 ≤ N`.
- The **candidates** of a chain: `c = near2` (or `near3`), then `reach(prev[c mod 65536])`, and
  so on, while `c` is not none. (Rings of 65536 hold every position within reach.)
- **pairs(pos)**, given `c2 = near2` (not none): the list `P` of `(length, position)`, first
  `(t, c2)` with `t = lcp(c2, pos)`. Then the first 32 candidates `c` of chain 3, stopping early
  where `t = N − pos`: where `c < c2` and `D[c + t] = D[pos + t]`, `l = lcp(c, pos)`, and if
  `l > t`, `(l, c)` is appended and `t = l`. The lengths increase along `P`, and so do the
  distances.
- **nearest(pos, n)**, `2 ≤ n ≤ N − pos`: the first of the first 1024 candidates of chain 2
  (`n = 2`) or chain 3 (`n ≥ 3`) with `lcp(c, pos) ≥ n`; none otherwise.

## 5. The record

A record is a byte `mode` (0, or 1 for skip; other values are an error), then the body.

### 5.1 Coding

The body is a binary arithmetic code (carry-less, 32 bits):

```text
x1 = 0, x2 = 2^32 − 1;  the decoder reads 4 bytes into x (zeros past the end)
bit with probability p1 of a 1 (16 bits):
  xmid = x1 + (((x2 − x1) × p1) >> 16)
  1: x2 = xmid;  0: x1 = xmid + 1        (the decoder reads 1 iff x ≤ xmid)
  while x1 and x2 agree in their top byte:
    the encoder writes x2 >> 24; x1 <<= 8; x2 = x2 << 8 | 0xFF; the decoder shifts in a byte
end: the encoder writes (x1 >> 24) + 1
```

A decoder that reads more than 4 bytes past the end of the body fails.

Each **adaptive bit** has `p1` (initially 32768) and a count `c` (initially 0). After coding a
bit `b`: `p1 += floor((t − p1) × RATE[c] / 65536)` with `t = 65535` for a 1 and 0 for a 0,
`RATE[i] = floor(131072 / (2i + 3))`, then `p1` is clamped to 32 … 65504 and `c`, if below 30,
grows by 1. A **raw bit** has `p1 = 32768` and is not adapted; raw bits of a value go highest
first.

- `gamma(t)` codes `v` as `x = v + 1`: `bitlen(x) − 1` ones then a zero, the `i`-th of them
  with the adaptive bit `t[min(i, |t| − 1)]`; then the `bitlen(x) − 1` bits of `x` below its top
  one, raw. Decoding fails at a 31st one.
- `tree5(t)` codes `v < 32` highest bit first, bit `i` with `t[m]`, `m = 1` then `2m + bit`.

All contexts are fresh at the start of a record:

| context | size | index |
|---|---|---|
| `start` | 8 × 12 × 12 × 4 | `((bucket(run) × 12 + lb(l2)) × 12 + lb(l3)) × 4 + db(e3)` |
| `first` | 1 | |
| `pair` | 8 × 9 | `min(k, 7) × 9 + min(m, 8)` |
| `longer` | 64 (gamma) | |
| `full` | 4 × 12 | `min(j, 3) × 12 + lb(top)` |
| `short` | 12 × 64 (gamma) | `lb(top)` |
| `far` | 5 × 12 | `state(run) × 12 + lb(n)` |
| `dist` | 64 (tree5) | |
| `form` | 2 × 8 | `[state(run) = 4 and n = 3] × 8 + min(n, 9) − 2` |

with `bucket(x) = min(bitlen(x), 7)`, `lb(l) = l` for `l < 8` else `min(5 + bitlen(l), 11)`, and
`db(e) = [e ≥ 64] + [e ≥ 1024] + [e ≥ 8192]`.

### 5.2 The walk

The body codes the parse in a walk over `D`, from `pos = 0` with `run = 0`, until `pos = N`:

```text
insert the positions below pos (as the mode holds them)
c2 = near2;  if none: a literal (pos += 1, run += 1), next
l2 = lcp(c2, pos);  l3, e3 = lcp(near3, pos), pos − near3 if near3, else 0, 0
bit start[...]: 0 a literal (pos += 1, run += 1), next
1: a match after run literals:
  if it is the first match: the first run's form (below)
  P = pairs(pos), of m entries
  j = 0;  while j < m and bit pair[k = j, m] = 1: j += 1
  if j = m:
    n = P[m−1].length + 1 + gamma(longer);  fails if n > N − pos
    near = nearest(pos, n)
  else:
    i = m − 1 − j;  (top, c) = P[i];  lo = P[i−1].length + 1 if i > 0, else 2
    bit full[j, top]: 1: n = top
      0: n = top − 1 − gamma(short[top]);  fails unless n ≥ lo
    near = c
  if near is none, or bit far[run, n] = 1:
    b = tree5(dist);  fails unless 1 ≤ b ≤ 16
    d = 2^(b−1) + (b − 1 raw bits)
  else d = pos − near
  fails unless d ≤ min(pos, W), n ≤ N − pos and lcp(pos − d, pos) ≥ n
  the form: if both short_fits and long_fits (§3.3): bit form[...] (1 long);
            else the one that fits; fails if none does
  the match (run, n, d, form);  in mode skip, pos + 1 … pos + n − 1 are never inserted
  pos += n, run = 0
at the end: tail = run; if no match came, the first run's form (below)
```

**The first run's form**, coded once its length `r` (`run` then) is known: `first_short` is
clear for `r = 0` and `r > 238`, set for `1 ≤ r ≤ 3`, and else the adaptive bit `first`.

## 6. lzop files *(informative)*

lzop writes a header and then blocks, each an LZO1X stream of a piece of the data (256 KiB but
for the last), or the piece itself where the stream would not be smaller. The structure, as
libarchive reads it:

```text
89 4C 5A 4F 00 0D 0A 1A 0A
be16 version, be16 library version, be16 version needed (version ≥ 0x0940)
u8 method (1, 2: LZO1X-1 and LZO1X-1(15); 3: LZO1X-999), u8 level (version ≥ 0x0940)
be32 flags, be32 filter (flag 0x800), be32 mode, be32 mtime, be32 mtime high (≥ 0x0940)
u8 name length, name, be32 header checksum
be32 length, data, be32 checksum            (flag 0x40, the extra field)
blocks: be32 data length (0 ends the file), be32 stream length (= the data length: stored)
        be32 checksum of the data           (flags 0x001 Adler-32, 0x100 CRC-32)
        be32 checksum of the stream         (flags 0x002, 0x200; not for a stored block)
        the stream or the data
```

## 7. Formula module

The module is `lzo-recons`, version 1. It rebuilds one or more streams, each from its data and
its record, and its output is the streams one after another. A container cuts the output by
the streams' lengths (a Split instruction of the formula), and adds what lies around them.

**Parameters**: none (empty).

**Inputs**: exactly three, in this order.

| slot | content |
|---|---|
| 0 | the data of every stream, one after another |
| 1 | the records of every stream, one after another |
| 2 | the index: for each stream, the length of its data and the length of its record (varints); or empty, for a single stream of all the data and the whole record |

The index must use up the data and the records exactly. Each stream is the stream (§3.4) of the
parse that its record codes over its data (§5). Streams are rebuilt independently of each other.

**Resources**: none.

The module fails on any parameters, any other count of inputs or resources, an index that
does not cut the inputs exactly, a record that fails above, or an output whose length is not
the application's declared output size.

## 8. Encoder choices *(informative)*

- `recons::split` decodes a stream, checks that its parse gives it back (§3.5), and codes the
  parse in the mode whose record of the first 64 KiB or so is the smaller (skip suits LZO1X-1,
  whose hash table holds no position inside a match; mode 0 suits LZO1X-999).
- Where a match is longer than every pair, the walk codes its length beyond the longest; where
  its distance is not the nearest, the distance itself.
- `formula::encode` takes an lzop file at the start of its input. Each compressed block whose
  stream splits (up to its end, to the data length its block header declares) becomes a stream
  of the module. The header, the block headers and checksums, the streams that do not split and
  what follows the file go in a stream of class `lzo`; the data of the split and stored blocks,
  in the file's order, in one of class `raw`, which the formula may recompress in turn. The
  records go in `lzo/record`, the index in `lzo/index`.
