# aqr-xpress format

aqr-xpress splits an XPRESS Huffman stream into its decompressed **data** and a **record** of
everything else its bits say: how it is cut into blocks, its Huffman code lengths, its matches,
whether it ends with the end symbol. From the data and the record the stream is rebuilt bit for
bit. This document describes the streams taken, the record, and the [formula module](#5-formula-module)
`xpress-recons` that rebuilds streams from data and records.

No encoder is reproduced. The record codes the parse given the data, under contexts of what a
match finder sees in the data; the code lengths are coded against a Huffman code of the block's
frequencies. Any stream that reads whole (§3.4) has a record.

The document is normative and complete: an implementation of the module can be written from it
alone. Sections marked *informative* describe what was seen of Windows' encoders; a decoder
needs none of it.

External reference: the LZ77+Huffman variant of Microsoft's Xpress Compression Algorithm
([MS-XCA], §2.1 and §2.2). §3 restates what this document needs of it. aqr-xpress was written
from that description, without any implementation's code.

Contents:

1. [Conventions](#1-conventions)
2. [Overview](#2-overview)
3. [XPRESS Huffman streams](#3-xpress-huffman-streams)
4. [The record](#4-the-record)
5. [Formula module](#5-formula-module)
6. [Windows' encoders](#6-windows-encoders-informative) *(informative)*

## 1. Conventions

- Integers are unsigned unless said otherwise. Words and dwords are little-endian.
- `D` is the data of a chunk, `N` its length; `D[a..b)` are the bytes from `a` to `b − 1`.
- `lcp(a, b, cap)`, `a < b`: the length of the common prefix of `D[a..N)` and `D[b..N)`, at most
  `cap` (and at most `N − b`).
- **varint**: unsigned LEB128, least significant group first (the encoder writes the shortest
  form).

## 2. Overview

A **stream** is **chunks**, each compressed on its own and holding a fixed number of bytes of
data but the last (the chunk size): WOF's (Windows' compressed files: 4, 8 or 16 KiB of data), a
WIM's resource, or a whole buffer (`RtlCompressBuffer`, ESE, hibernation files: one chunk). A
chunk is **stored** (it is as long as its data and is the data) or **compressed** (§3). The data
of a stream is its chunks' data one after another.

The record of a stream tells the chunk size and, for each chunk, whether it is stored and, if
not, its parse: the tokens (§4.4) and the code lengths (§4.5). A container (WOF in aqr-disk, WIM)
cuts its resources into chunks; the module (§5) rebuilds the chunks one after another, between
bytes the container keeps.

## 3. XPRESS Huffman streams

### 3.1 Blocks

A compressed chunk is blocks one after another. A block is a table of 256 bytes, then the bits of
its tokens. The table holds 512 code lengths of 4 bits, the length of symbol `2i` in the low
nibble of byte `i` and that of `2i + 1` in the high one. Lengths are at most 15 (0: no code).
The code is canonical: codes are assigned in order of length, then symbol, each length's first
code being `(first code of the length before + count of that length) << 1`, starting from 0 at
length 1. The lengths must fill the code exactly (`Σ 2^(15 − length) = 2^15`); anything else is
not read (§3.4).

A block holds tokens up to **the first that brings the data of the block to 65,536 bytes or
more** (so the last token may pass it); a block also ends at the end of the chunk. A block is
never empty. Blocks carry no sizes: this rule cuts them.

### 3.2 Tokens

Symbols 0 to 255 are literals. Symbol `256 + 16 × b + l` (`b < 32`, `l < 16`) is a match whose
offset has `b` extra bits: the offset is `2^b + x`, `x` the next `b` bits of the stream, so it
is between `2^b` and `2^(b+1) − 1`; only `b ≤ 15` is taken here (offsets up to 65,535). Its
length is `l + 3` for `l < 15`; for `l = 15` a byte `e` follows in the stream (below), and the
length is `e + 18` if `e < 255`; if `e = 255` a word `w` follows, and the length is `w + 3`; if
`w = 0` a dword `v` follows instead, and the length is `v + 3`. The writer uses the shortest
form that holds the length (`w ≥ 270`; the dword from length 65,539). A match copies `length`
bytes from `offset` back, one by one (it may overlap itself); it reaches back into the chunk
only. Symbol 256 is thus a match of 3 bytes at offset 1; it is the **end symbol** only as the
last symbol of the chunk (§3.3).

### 3.3 Bits

The bits of a block are read in words of 16 bits, a word's bits from its high bit to its low, a
code's bits first to last. The decoder keeps 32 bits (`nb`) and a count `extra` of those past the
first 16, starting from the first two words after the table and `extra = 16`. A symbol is the
code that `nb`'s top bits start; skipping `n` bits shifts `nb` left by `n`, takes `n` from
`extra`, and if `extra` is then negative reads the next word into `nb << (−extra)`'s low bits and
adds 16 to `extra`. The offset's extra bits are the top `b` bits of `nb` after the symbol is
skipped. A length's byte, word and dword are read from where the words have got to: the next
bytes of the stream, whatever the bits (the writer puts them just after the last word it has
reserved).

The writer ([MS-XCA] 2.1.4.3): it reserves two words after the table at once, `p1` and `p2`,
and `FreeBits = 16`. Writing `n` bits `v` while `FreeBits ≥ n` shifts them in; otherwise it
completes the word with the top bits, stores the word at `p1`, moves `p1` to `p2` and `p2` to
the end of the output, reserves two bytes there, and starts the next word with the rest. Bytes
of lengths go at the end of the output. At the end of a block, the pending word, shifted left by
`FreeBits` (the padding bits zero), is stored at `p1`; `p2` stays zero; the output so far is
the block. The next block starts there.

The last block of a chunk is followed by the end symbol (its code, written as any), or by
nothing (a buffer cut after a whole block, or another encoder's). The record says which (§4.3);
in the former the code length of symbol 256 is not 0.

### 3.4 What is read

A chunk reads whole if, given its data length `N`: its blocks' tables are valid (§3.1); no match
passes the chunk's start or `N` bytes of data or 65,535 back; its last block has all the `N`
bytes, then the end symbol or not; nothing follows; and writing the same parse (§3.1, §3.3) gives
the chunk back byte for byte. The last needs the padding bits zero, the lengths in their
shortest forms, and the words and bytes as the writer lays them out.

## 4. The record

A stream's record is a binary arithmetic code of its parse given its data, its contexts going
on from chunk to chunk within the stream.

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

A decoder that has read more than 4 bytes past the end of the record, after a chunk, fails.

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

`lb(n)`, the bucket of a length: 0 for `n < 3`, then 1 for 3; 2 for 4–5; 3 for 6–8; 4 for 9–16;
5 for 17–32; 6 for 33–128; 7 above. `ob(o)`: 0 for 0, else `floor(log2(o)) + 1`.

All contexts are fresh at the start of a stream's record:

| context | size | index |
|---|---|---|
| `stored` | 1 | |
| `end` | 1 | |
| `is_match` | 8 × 17 × 3 × 2 × 2 | `((((lb(first) × 17 + ob(off1)) × 3 + rel) × 2 + prev) × 2 + (longest > first))` |
| `is_setter` | 8 × 8 | `fx = lb(first) × 8 + lb(longest)` |
| `setter` | 8 × 8 (gamma) | `fx` |
| `escape` | 1 | |
| `cand` | 8 × 8 (gamma) | `fx` |
| `short` | 1 | |
| `short_by` | 1 (gamma) | |
| `tree_eq` | 16 × 2 × 2 | `(pred × 2 + m) × 2 + agree` |
| `tree_val` | 16 × 2 (tree5) | `pred × 2 + m` |

### 4.2 The match finder

The contexts of tokens come from a match finder over the stream's data `D` (all chunks; a
position is its offset in the stream), which sees one chunk's matches at a time: the **reach** at
`pos` is `lo(pos) = max(chunk start, pos − 65535)`.

- A chain: heads `head[65536]` by `h3(p) = (k3(p) × 0x9E3779B1, wrapping) >> 16` with
  `k3(p) = D[p] | D[p+1] << 8 | D[p+2] << 16`, for `p + 3 ≤ |D|`, and a ring of links
  `prev[65536]`. Every entry starts as `NONE` (at the start of each stream).
- Inserting `p` (if `p + 3 ≤ |D|`): `prev[p mod 65536] = head[h3(p)]`, `head[h3(p)] = p`.
- The **candidates** at `pos` (with `pos + 3 ≤ |D|`) start at `c = head[h3(pos)]`, but at its link
  if `c ≥ pos`, and go on by `prev[c mod 65536]` while `c ≠ NONE` and `c ≥ lo(pos)`.
- **longest(pos, cap, lo)**: 0 (all four results) if `cap < 3`. Else walk the first 16
  candidates `c`; the first (`first`, with its offset `off1 = pos − c`) always, the others only if
  `lcp(c, pos, cap) > best` can hold: with `k = min(best, cap − 1)`, the 4 bytes of `D` ending at
  `c + k` and at `pos + k` agree (the one byte at `k` alone if `k < 3`); `n = lcp(c, pos, cap)`.
  The walk stops where `best ≥ cap`. `best` is the greatest `n` (the first of them, nearest, on ties) and its
  offset. Results: `(best, offset, first, off1)`, all four 0 if `best < 3`.
- **setters(pos, cap, lo)**: walking the first 4096 candidates, with `best = 2`: each candidate
  `c` with `lcp(c, pos, cap) = n > best` (tested as above) is a **record setter**, of offset
  `pos − c` and length `n`, and sets `best = n`; the walk stops where `best ≥ cap`. They come
  nearest first.
- **offsets(pos, lo)**: of the first 4096 candidates `c`, in order, those with `lcp(c, pos, 3) = 3`
  give the offsets `pos − c`, nearest first.

Before the tokens at `pos` are looked at, `pos` is inserted (§4.4); in the stream's mode `all`
(§4.3), after a match at `pos` its other positions are inserted too, else only the positions
where tokens start are in the chains.

### 4.3 A stream and its chunks

A record starts with the **chunk size** `S`: `gamma` under a table of 32 adaptive bits of its
own, then one bit of an adaptive bit of its own, `all` (a fresh one: the bit costs one bit).
`S` is at least 1 and at most the length of the stream's data `L`; the chunks are the `S`-byte
pieces of the data, the last shorter. A stream of no data has an empty record (and fails
otherwise). A data of 2^32 − 65536 bytes or more fails.

For each chunk (`N` bytes of data from `base`), the bit `stored`: 1 for a stored chunk, which is
the data as it is. Otherwise, for each block until the chunk's data is gone, from `at` = the
chunk's start:

1. The tokens (§4.4) from `at` until the block has 65,536 bytes of data or more, or the chunk
   has no more. `at` is where they end.
2. If `at` is the chunk's end: the bit `end` (1: the end symbol follows).
3. The code lengths (§4.5).

The block is written as soon as it is known (§3.3), after its code lengths.

### 4.4 Tokens

At `pos`, with `cap = chunk end − pos` and `lo = lo(pos)`:

1. `(longest, _, first, off1) = longest(pos, cap, lo)`. Insert `pos`.
2. `rel`: 0 if `longest < 3` or `cap ≤ 3`; else with `next` the first result of
   `longest(pos + 1, cap − 1, lo(pos + 1))`: 0 if `next ≤ longest`, 1 if `next = longest + 1`,
   else 2. (`pos` is among the candidates of `pos + 1`.)
3. If `cap < 3` the token is a literal and nothing is coded. Otherwise the bit `is_match`, with
   `prev` the last token's kind in this block (1 for a match, 0 for a literal or the first).
   0: a literal, `pos` advances by 1.
4. A match: `fx` as in §4.1. The bit `is_setter[fx]`.
   - 1: `rank = gamma(setter[fx])`; the match is the `rank`-th record setter of
     `setters(pos, cap, lo)` (from 0); none such fails. It is set where the match is the whole
     of its offset's (`len = lcp(pos − off, pos, cap)`), within the reach, and a record setter.
   - 0: the bit `escape`: 1: the offset is 16 raw bits. 0: `rank = gamma(cand[fx])`, and the
     offset the `rank`-th of `offsets(pos, lo)` (none such fails). Then with `all =
     lcp(pos − off, pos, cap)`, the bit `short`: 1: `by = gamma(short_by)` and `len = all − by − 1`
     (it fails if that is below 1); 0: `len = all`.
5. The match fails unless `3 ≤ len ≤ cap`, `1 ≤ off ≤ pos − lo` and `lcp(pos − off, pos, len) =
   len`. The token is pushed and `pos` advances by `len`.

The encoder sets `is_setter` where it can, and `escape` where the offset is none of the
candidates (past 4096 of them).

### 4.5 Code lengths

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

After the block's tokens (and `end`, for the last), the frequencies are counted over the block's
tokens, `p` running over its data: a literal counts `freq[D[p]]`; a match counts
`freq[256 + min(len − 3, 15) + 16 × floor(log2(off))]`; and `freq[256]` is increased by `end`
(1 for a last block with the end symbol). `P = huffman(freq, 15)`.

For `s` from 0 to 511, with `agree` starting at 1 and the length `v` of symbol `s`: the bit
`tree_eq[(P[s] × 2 + m) × 2 + agree]`, `m = 1` for `s ≥ 256`: 1 → `v = P[s]`; 0 → `v =
tree5(tree_val[P[s] × 2 + m])`, which fails if `v > 15` or `v = P[s]`. Then `agree` is
`v = P[s]`. The lengths must be a valid code (§3.1) with a length for every symbol the block
uses (and the end symbol, if it has one), or the stream fails.

## 5. Formula module

The module is `xpress-recons`, version 1. It rebuilds one or more streams, each from its data
and its record, between bytes it is given as they are: its output is a part of a container as it
is.

**Parameters**: none; anything else fails.

**Inputs**: exactly four, in this order.

| slot | content |
|---|---|
| 0 | the data of every stream, one after another |
| 1 | the records of every stream, one after another |
| 2 | the bytes kept: before each stream, then after the last |
| 3 | the index: for each stream, three varints: the bytes kept before it, the length of its data, the length of its record |

The index cuts inputs 2, 0 and 1 in that order, from their starts; inputs 0 and 1 must be used
up exactly, and what is left of input 2 is the bytes kept after the last stream.

The output is, for each stream in order, the bytes kept before it, then the stream its data and
record rebuild (§4); then the bytes kept after the last. Streams are rebuilt independently of
each other (each record's contexts fresh). The output must be the application's declared size;
a stream passing it fails at the token that does.

**Resources**: none.

The module fails on other parameters, another count of inputs or resources, an index that does
not cut the inputs exactly, a record that fails (§4), or an output of another length.

**Memory** *(informative)*: the module lays its memory out as a stack (`aqr_api::stack`), and
the plan of its memory is exact (`plan::decode`): besides the inputs and the output (with 64
bytes to spare), the match finder (the head of 65,536 words and the ring of as many), made once
for all the streams; for each stream its contexts and the tokens of a block (8 bytes each, as
many as the chunk size or 65,536 at most), and 64 KiB (`SCRATCH`) for the work of a block's
code lengths (taken once and given back, so that the plan does not depend on the data). A block
is written onto the end of the output as soon as its record is read. Streams are rebuilt one
after another on one thread.

## 6. Windows' encoders *(informative)*

What `RtlCompressBuffer` (`COMPRESSION_FORMAT_XPRESS_HUFF`, the standard and the maximum engine)
and WOF (XPRESS4K/8K/16K) write, as seen on the samples of the tests; the record is cheap
because of it.

- Blocks are cut as §3.1 says, and the last match of a block may pass 65,536 (a match is never
  cut). The end symbol follows the last block even where that block is full, spilling into a
  word of its own: the stream then ends two bytes later than its last token's bits. Streams
  of WOF's chunks all have it.
- Symbol 256 also appears as a match of 3 bytes at offset 1, as the format has it.
- The code lengths are those of `huffman` (§4.5) over the block's frequencies with the end symbol
  counted in the last block: not one of the samples' tables differs, in any engine or chunk size.
- Matches are always the whole match at their offset, and in the samples a match of 3 bytes is
  never farther than 4,095.
- The standard engine's candidates are the positions where tokens start (not the ones inside
  matches), the match taken usually the nearest of the chain, otherwise a longer one farther
  back (a record setter, §4.2); its decisions between a literal and a match follow the nearest
  candidate's length and distance. The record's `all` is 0 for it.
- The maximum engine sees every position and takes record setters: usually the longest, else a
  shorter and nearer one (the cost of the offset's bits against the length). `all` is 1.
- An incompressible buffer is written as a stream larger than its data (the standard engine's
  noise: 1,088 bytes more for 256 KiB), not stored; WOF stores a chunk that does not shrink.
