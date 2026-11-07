# aqr-lha format

aqr-lha takes LHA archives apart. It splits each stream of the methods `-lh4-` to `-lh7-`
into the **data** it decodes to and a **record** of how its compressor wrote it: the parse,
the blocks' sizes and their headers. From the data and the record the stream is rebuilt bit for
bit. This document describes the streams as far as the module needs them, the record, and the
[formula module](#7-formula-module); the archive is described as aqr-lha reads it.

The record does not reproduce any compressor. It codes the parse given the data, under contexts
of what two match finders see in the data, and each block's header against the one a compressor
of LHA's lineage writes from the block's tokens. Any stream of the method's format has a
record. The compressors seen (LHa for UNIX, LHA 2.x for MS-DOS and its ports, LhA for AmigaOS,
UNLHA32's front ends and others: §8) write the predicted headers in every block, and their
matches are mostly the longest within reach, so their records cost about 0.4–2% of the streams.

The document is normative and complete: an implementation of the module can be written from it
alone. Sections marked *informative* describe the archive and the reference encoder's choices; a
decoder needs none of them.

External references: none is needed. The streams' format (§3) is the one of Haruhiko Okumura's
ar002, which LHA's `-lh5-` took up, as LHA's decoders read it; the headers' predictions (§4) are
the Huffman coder of that lineage, as their output shows it. aqr-lha was written from the format
as described here and from the output of the compressors, used as black boxes (the test archives
of [lhasa](https://github.com/fragglet/lhasa) and archives of Aminet), without any LHA code.

Contents:

1. [Conventions](#1-conventions)
2. [The archive](#2-the-archive-informative) *(informative)*
3. [Streams](#3-streams)
4. [Predicted headers](#4-predicted-headers)
5. [The match finders](#5-the-match-finders)
6. [The record](#6-the-record)
7. [Formula module](#7-formula-module)
8. [Encoder choices](#8-encoder-choices-informative) *(informative)*

## 1. Conventions

- Integers are unsigned. `le16`, `le32`, `le64` are little-endian integers. Arithmetic is exact
  unless marked *wrapping* (modulo 2^64).
- `D` is a stream's data and `N` its length. `D[a..b)` are the bytes from `a` to `b − 1`.
- `bitlen(x)` is the number of bits of `x` (0 for 0).
- `lcp(a, b)`, `a < b`: the length of the common prefix of `D[a..N)` and `D[b..N)`.
- `[c]` is 1 where `c` holds, else 0.
- **varint**: unsigned LEB128, least significant group first, in the shortest form.
- `NONE` is a position that is none.

## 2. The archive *(informative)*

An archive is a sequence of entries, each a header and then its file's packed data, ended by a
byte 0 or by the end of the data. Every header has at offsets 2 to 6 its method id (`-` two
letters or digits, one more, `-`: `-lh5-`), at 7 the packed size (`le32`), at 11 the original
size (`le32`), at 20 its level (0 to 3). By level:

- **0**: byte 0 is the header's size less 2, byte 1 the sum of its bytes from 2 (modulo 256), at
  21 the length `n` of the name, at 22 the name, then the file's CRC-16 (`le16`, ARC's:
  reflected polynomial 0xA001 from 0), and what else the size counts. The data follows.
- **1**: as level 0, the size counting at least an OS id after the CRC-16 and the `le16` size of
  the first extended header at its end. Extended headers follow, each a byte of type, its data
  and the `le16` size of the next (0 ends them), its size counting all three. The packed size
  counts them: the data is the packed size less their sizes.
- **2**: bytes 0-1 are the header's whole size (`le16`), the extended headers within it from
  offset 26 (the first's size at 24), the file's CRC-16 at 21.
- **3**: bytes 0-1 are 4, the header's whole size is the `le32` at 24, the extended headers from
  32 (the first's size at 28) with sizes of 4 bytes.

An extended header of type 1 holds the file's name; of type 0x42, the packed and original sizes
(`le64` each) of a file of 4 GiB or more, in place of the base header's.

aqr-lha's formula (`formula.rs`) keeps the headers and whatever it does not take as they are.
Stored files (`-lh0-`, `-lz4-`, `-pm0-` of packed size the original size) are their contents;
the data of `-lh4-` to `-lh7-` within the budget is an output of the module (§7), where its
record is shorter than the data; other methods are kept. The archive is taken at the start of a
file only: its first bytes are no signature to look for in other data.

## 3. Streams

### 3.1 Methods

| method | `W`, the window | `NP`, the distances' symbols | `PBIT` |
|---|---|---|---|
| `-lh4-` (record number 4) | 4096 | 14 | 4 |
| `-lh5-` (5) | 8192 | 14 | 4 |
| `-lh6-` (6) | 32768 | 16 | 5 |
| `-lh7-` (7) | 65536 | 17 | 5 |

`NC = 510` (256 literals and lengths 3 to 256), `NT = 19`, `CBIT = 9`, `TBIT = 5`.

### 3.2 Bits and codes

Bits are written highest first into bytes, the first bit the top bit of the first byte. A value
of `k` bits is written highest bit first.

A **code** is given by lengths `len[s]` of its symbols (0 to 16; 0 for a symbol without a code).
It is canonical: going through the lengths 1 to 16, and within a length the symbols in order,
each gets the next code (`code = 0`; for each length `l`, its symbols get `code, code + 1, …`;
then `code = (code + count[l]) << 1`). A code is **over-full** where the codes of a length run
past `2^l`; it may be incomplete. A code may also be a **single** symbol `s`, of no bits: every
symbol read is `s`.

### 3.3 Tokens

A stream is the code of a sequence of **tokens**: literals (a byte) and matches `(n, d)` of
length `3 ≤ n ≤ 256` and distance `1 ≤ d ≤ W`. A match at `pos` copies `n` bytes from `d` back
one at a time (`d < n` repeats the last `d` bytes): `d ≤ pos` and `pos + n ≤ N`. The data is the
tokens' bytes, `N` of them.

A token is written as a symbol of the code `C`: a literal `b` as `b`, a match as `256 + n − 3`;
then for a match its distance: `j = bitlen(d − 1)` as a symbol of the code `P` (`j < NP`), then
for `j ≥ 2` the `j − 1` bits of `d − 1` below its top one.

### 3.4 Blocks

The tokens are cut into **blocks**, each a header and then its tokens. The **header**:

1. `size` (16 bits): the count of tokens of the block, 1 to 65535.
2. The table `T` (`read_pt(NT, TBIT, special 3)`): the code of `C`'s lengths.
3. The table `C`: `n` (`CBIT` bits). For `n = 0`: a symbol `s` (`CBIT` bits), `s < NC`, and `C` is
   single `s`. Else **items**, each a symbol of `T` and its extra bits, until they tell `n`
   lengths or more: symbol 0 is one length 0; 1 is `3 + v` lengths 0, `v` of 4 bits; 2 is `20 +
   v` lengths 0, `v` of `CBIT` bits; `3 ≤ t ≤ 18` is the length `t − 2`. The lengths told are
   `C`'s, the rest 0; they must be at most `NC`, and the last item is the one reaching `n`.
4. The table `P` (`read_pt(NP, PBIT, none)`).

`read_pt(nn, nbit, special)`: `n` (`nbit` bits). For `n = 0`: a symbol `s` (`nbit` bits), and
the code is single `s`. Else, `1 ≤ n ≤ nn`, lengths until there are `n` or more: a length is 3
bits `c`; for `c = 7`, as many more as the bits 1 that follow, then a bit 0 (`c ≤ 16`). Where the
count of lengths reaches `special` right after a length, 2 bits `k` follow and `k` lengths 0.
The lengths read are the code's (at most `nn`), the rest 0.

None of the codes may be over-full. A table's lengths and its count are as written: the same
lengths may be written otherwise (a count past the last length not 0, zeros as items of another
kind), and the record keeps which (§6.4).

### 3.5 The stream

The blocks follow one another until the tokens make `N` bytes; the last token ends the data
exactly, and the blocks' sizes count the tokens exactly. Then bits to the end of the byte (the
**padding**, `pad_bits` of them, 0 to 7, of any value `pad`), then any bytes (the **tail**). A
stream decodes to its data; a decoder fails on a code it cannot read, a token out of the data
(a distance past `pos` or `W`, a length past `N`), or bits read past the stream's end.

## 4. Predicted headers

The header predicted for a block of tokens is the one a compressor of LHA's lineage writes from
the tokens' frequencies: `fc[s]` of `C`'s symbols, `fp[j]` of `P`'s (§3.3).

### 4.1 Lengths

`lengths(f)` of `n` symbols, frequencies `f`:

```text
heap = the symbols s with f[s] > 0, in order, as heap[1..]; size = their count
size < 2: the code is single: the one symbol, or 0 for none
down(i): k = heap[i]; while 2i ≤ size: j = 2i; if j < size and f[heap[j]] > f[heap[j+1]]: j += 1
         if f[k] ≤ f[heap[j]]: stop;  heap[i] = heap[j]; i = j
         heap[i] = k
for i from size / 2 down to 1: down(i)
order = [];  avail = n (inner nodes are n, n + 1, …, with frequencies f[k] of their own)
repeat: i = heap[1]; if i < n: append i to order
        heap[1] = heap[size]; size −= 1; down(1)
        j = heap[1]; if j < n: append j to order
        k = avail; avail += 1; f[k] = f[i] + f[j]; heap[1] = k; down(1)
        children of k: i and j
        until size ≤ 1
cnt[l] = the count of leaves at depth l below the root k (depths past 16 counted at 16)
while sum(cnt[l] × 2^(16 − l)) ≠ 2^16:
    cnt[16] −= 1; for the first l from 15 down to 1 with cnt[l] > 0: cnt[l] −= 1, cnt[l+1] += 2
for l from 16 down to 1: the next cnt[l] symbols of order get the length l
```

### 4.2 Tables

- `pt(lengths, special)`: for a single code `s`, `n = 0` and `s`. Else `n` is one past the last
  length not 0, the lengths to `n` written; with `special` (for `T`) and `n ≥ 3`, the 2 bits are
  the count of zeros at `3, 4, 5` from 3 on (lengths past `n` are 0), and those zeros are not
  written as lengths (the count may thus reach past `n`).
- `C`'s items: `n` is one past the last length not 0 (`n = 0` and the symbol for a single code);
  going through the lengths to `n`, a length `l > 0` is the item `l + 2`; a run of `r` zeros is
  `r` items 0 for `r ≤ 2`, the item 1 of `r − 3` for `r ≤ 18`, the items 0 and then 1 of 15 for
  `r = 19`, the item 2 of `r − 20` beyond.

### 4.3 The header

- `C` from `lengths(fc)`. Where it is single `s`: `T` is single 0 (`n = 0`, symbol 0) and `C`
  single `s`. Else `C`'s items (§4.2), and `T = pt(lengths(ft), special)` with `ft[t]` the count
  of items of symbol `t`.
- `P = pt(lengths(fp), none)`.
- `size` is the count of tokens.

## 5. The match finders

The record's contexts come from two match finders over the stream's data, positions within
**reach** `R` back (the largest distance of the stream's matches, given in the record).

### 5.1 The chains

- **Keys**: `KEYS = [3, 5, 8, 16]`. The key of `k` bytes at `p` (`p + k ≤ N`): from `h = 0`, for
  each byte `b` of `D[p..p+k)`, `h = (h ^ b) × 0x100000001B3` (wrapping), and the key is `(h ×
  0x9E3779B97F4A7C15) >> 48` (wrapping).
- Each key has a chain: `head[65536]` and `prev[p]`, all `NONE` at first. Inserting `p`: for each
  key with `p + k ≤ N`, `prev[p] = head[key]` and `head[key] = p`. Positions are inserted in
  order. A chain is walked from `head` by `prev`; it ends at `NONE` or at a position more than
  `R` before the one queried.
- **pairs(q)**, given `cap`, with the positions below `q` inserted: for each key `k` in order,
  while `q + k ≤ N` and `k ≤ cap`: `best = k − 1`; for the first `DEPTH = 32` candidates `c` of
  its chain from `head[key(q)]`: where `D[c + best] = D[q + best]`, `l = min(lcp(c, q), cap)`;
  if `l > best`, `(q − c, l)` is *found* and `best = l`; stop the chain at `best = cap`. Stop the
  keys once a chain reached `cap`. Then the found `(distance, length)` are sorted by distance,
  then length; going through them, each longer than all before it (and than 2) is a **pair**
  `(length, distance)`. The pairs go by increasing distance and length: each is the nearest
  found of its length. The **longest** is the last, or `(0, 0)` without pairs.
- **occurrences(q, n)**: walking the chain of 3 bytes from `head[key(q)]`, the first `LIMIT =
  4096` candidates `c` (those at `q` or after it skipped but counted), those with `lcp(c, q) ≥
  n` in order: nearest first. The **rank** of a distance `d` is its index among them (the count
  of them where it is not among them).

### 5.2 The tree

A binary search tree of the positions `p` within reach, ordered by `D[p..p+256)` as strings,
the first byte choosing one of 256 roots (Okumura's LZSS match finder). Each node has a parent
and a left and right child, each `NONE` or a node; a root has only a right child.

- **insert(r)**: `r` gets no children. From the root of `D[r]`, going right, compare: at a node
  `q`, `i` from 1 while `i < 256`: past the data (`r + i = N`) the comparison is *less*; else it
  compares `D[r + i]` with `D[q + i]`, stopping where they differ; `i` is then the length
  matched. Where `i` is more than the best so far, it is the best, at `q`; at `i = 256` the node
  of `q` is replaced by `r` (its parent's child, its children `r`'s) and the insertion ends.
  Else it goes left where less, right where more (or equal, at the root), and where that child is
  `NONE`, `r` becomes it. The **tree's match** at `r` is `(best, r − q)` where the best is at
  least 3, else none.
- **delete(p)** (a node in the tree): `q` is its left child where it has no right one, its right
  child where it has no left one; else its left child, and where that one has a right child,
  the rightmost node below it, which is first taken out (its parent's right child becomes its
  left child) and given `p`'s left child; then `q` gets `p`'s right child. `q` (or `NONE`) takes
  `p`'s place as its parent's child, on the same side.
- Positions are inserted in order, each `r` after deleting `r − R − 1` (where `r > R`).

## 6. The record

### 6.1 Coding

The record of a stream is a byte, the method's number (4 to 7, §3.1), then a binary arithmetic
code, as aqr-lzo's (`aqr-lzo/FORMAT.md` §5.1): carry-less, 32 bits, adaptive bits of 16-bit
probabilities learnt at `RATE[c]`, raw bits of probability 1/2, and `gamma(t)` coding `v` as `x
= v + 1` (`bitlen(x) − 1` ones then a zero with the adaptive bits of `t`, the last one for the
rest, then the bits of `x` below its top one, raw). Its end is aqr-lzo's.

The contexts, all fresh at the start:

| context | size | index |
|---|---|---|
| `is_match` | 2 × 12 × 7 × 3 | `((prev × 12 + lb(L)) × 7 + far) × 3 + lazy` |
| `tree` | 3 × 2 | `rel × 2 + prev` |
| `pair` | 3 × 4 × 4 × 2 | `((min(j, 2) × 4 + dl) × 4 + dd) × 2 + [i = 0]` |
| `full` | 12 | `lb(top)` |
| `shorter` | 12 × 32 (gamma) | from `32 lb(top)` |
| `escape` | 12 × 16 (gamma) | from `16 lb(L)` |
| `rank` | 12 × 24 (gamma) | from `24 lb(n)` |
| `cut` | 2 | `[a cut was learnt]` (§6.3) |
| `predicted` | 1 | |
| `misc` | 96: three gammas | `[0..32)` the reach, header bits, far distances; `[32..64)` the tail; `[64..96)` the count of tokens |

with `lb(l) = l` for `l < 8` else `min(5 + bitlen(l), 11)`; `db(d) = [d ≥ 16] + [d ≥ 256] + [d ≥
1024] + [d ≥ 4096] + [d ≥ 16384]`; `prev` 0 after a literal (and at the start), 1 after a
match.

The code is: `R = gamma(misc[0..32])`, `R ≤ W`; `K = gamma(misc[64..96])`, the count of the
stream's tokens, `K ≤ N` (a token covers a byte at least); the walk (§6.2), which must make
exactly `K` tokens; the blocks (§6.3); the end (§6.4). The count lets a decoder reserve the tokens
it holds until the blocks are written.

### 6.2 The walk

The record codes the tokens in a walk over `D` from `pos = 0` until `pos = N`:

```text
insert the positions below pos into the chains;  cap = min(N − pos, 256)
P = pairs(pos)   (those found for pos + 1 at the step before, where it was pos + 1)
L, Ld = the longest pair's length and distance;  far = 6 without pairs, else db(Ld)
lazy: if L ≥ 3 and pos + 1 < N: insert pos; nx = the longest length of pairs(pos + 1) with
      cap' = min(N − pos − 1, 256); lazy = [nx > L] + [nx > L + 1]; else lazy = 0

bit is_match: 0 a literal D[pos] (pos += 1, prev = 0), next
the tree: insert the positions up to pos (§5.2); (bl, bd) its match at pos, if any:
  rel = 0 where (bl, bd) = (L, Ld), 1 where bl = L, else 2
  bit tree[...]: 1 the match (bl, bd): pos += bl, prev = 1, next
m = |P|;  j = 0; while j < m: i = m − 1 − j; bit pair[...] (below): 0 stops, 1: j += 1
j < m: (top, d) = P[m − 1 − j]; n = length(top); the match (n, d)
j = m (an escape): n = 3 + gamma(escape[16 lb(L)..]), n ≤ cap; r = gamma(rank[24 lb(n)..])
       d = the occurrence of rank r of occurrences(pos, n) if there is one, else
       1 + gamma(misc[0..32])
fails unless the match fits: 1 ≤ d ≤ min(pos, R), 3 ≤ n ≤ cap, lcp(pos − d, pos) ≥ n
pos += n, prev = 1
```

`length(top)`: bit `full[lb(top)]`: 1 `top`; 0 `top − 1 − gamma(shorter[32 lb(top)..])`, failing
below 0. For the pair bits, `dl, dd = 3, 3` for `i = 0`, else `dl = min(P[i].length −
P[i−1].length − 1, 2)` and `dd = min(bitlen(P[i].distance) − bitlen(P[i−1].distance), 3)`.

### 6.3 The blocks

The blocks follow, until their sizes count all the tokens. For each, with `T` the tokens not in
a block yet:

1. Its size: the bit `cut[known]`: 1 the size predicted, 0 the size as 16 raw bits, which must
   be 1 to `|T|`.
2. Its header: the bit `predicted`: 1 the header predicted for its tokens (§4); 0 `b =
   gamma(misc[0..32])`, `b ≤ 65536`, then `b` raw bits, the header as written (§3.4), which must
   be read from them exactly, of the block's size.

**The cut**. A compressor buffering its tokens ends a block after a multiple of 8 tokens once
their bytes reach a threshold, when another token follows: a literal 1 byte, a match 3, and a
flag byte at the start of each 8. `bytes(k)` of the first `k` tokens of `T` is `k + 2 × (their
matches) + ⌈k / 8⌉`. The cut keeps `lo` (initially 0), `hi`, `known` and `broken` (both
initially false):

- The size predicted: `min(|T|, 65535)` where `known` is false or `broken` true; else the first
  `k` of `8, 16, …` below that with `bytes(k) ≥ lo`, or `min(|T|, 65535)` where none.
- After a block of `s` tokens with `s < |T|`: where `s` is not a multiple of 8, `broken`; else
  `lo = max(lo, bytes(s − 8) + 1)`, `hi = min(hi, bytes(s))`, `known` true, and `broken` where
  `lo > hi`.

### 6.4 The end

`pad_bits` is what the blocks written leave to the end of their last byte (§3.5); the padding is
that many raw bits. The tail: `t = gamma(misc[32..64])`, `t ≤ 2^24`, then `t` bytes of 8 raw
bits each.

The stream is the blocks (each header and its tokens, §3), the padding and the tail. A decoder
fails on a header that is no valid header of its block (its codes over-full or unable to write
its tokens, its tables' counts past their alphabets), on any failure above, or on an arithmetic
code read more than 4 bytes past its end.

## 7. Formula module

The module is `lha-recons`, version 2 (version 1 had no count of tokens in the record). It rebuilds one or more streams, each from its data and
its record, and its output is the streams one after another. A container cuts the output by
the streams' lengths (a Split instruction of the formula), and adds what lies around them.

**Parameters**: none (empty).

**Inputs**: exactly three, in this order.

| slot | content |
|---|---|
| 0 | the data of every stream, one after another |
| 1 | the records of every stream (§6), one after another |
| 2 | the index: for each stream, the length of its data and the length of its record (varints); or empty, for a single stream of all the data and the whole record |

The index must use up the data and the records exactly. Streams are rebuilt independently of
each other.

**Resources**: none.

The module fails on any parameters, any other count of inputs or resources, an index that does
not cut the inputs exactly, a record that fails above, or an output whose length is not the
application's declared output size.

## 8. Encoder choices *(informative)*

- `recons::split` decodes a stream, checks that its blocks and tokens encode to it again, and
  codes them (§6), the reach the largest distance of its matches. `formula` splits the streams of
  an archive side by side, those decoding within the budget by their headers' sizes, and keeps a
  stream whose record is not shorter than it.
- Every block of every compressor seen has the predicted header (§4): LHa for UNIX 1.14i, LHA
  2.13 and 2.55e for MS-DOS and its ports (OS/2, OS-9/68k, Human68K, MacLHA, LHarc for Atari),
  LhA for AmigaOS, MorphOS's LHA, ExpLZH, LHMelt, Tascal LHA, LH/2. Their blocks are cut by the
  rule of §6.3 (thresholds seen: 16360 bytes and others).
- Their matches: LhA for AmigaOS and LHa for UNIX take the longest match, nearly always the
  nearest of its length, and lazily (a literal where the next position has a longer match), as
  is told by `is_match`'s contexts. LHA 2.x takes the longest, at times not the nearest of its
  length (3% of matches, a near one), which the tree's match and the ranks cover in part.
- LHARK's `-lh7-` is another format under the same id, and is not decoded.
