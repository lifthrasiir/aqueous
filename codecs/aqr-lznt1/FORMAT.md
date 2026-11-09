# aqr-lznt1 format

aqr-lznt1 splits LZNT1 (NTFS's compression of files, `RtlCompressBuffer`'s buffers) into its
decompressed **data** and a **record** of everything else its bytes say: how the data is cut into
chunks, which chunks are stored, the tokens of the others, the end headers and the padding. From
the data and the record the bytes are rebuilt bit for bit. This document describes the streams
taken, the record, and the [formula module](#5-formula-module) `lznt1-recons` that rebuilds
streams from data and records.

No encoder is reproduced. Windows' is closed. The record codes the parse given the data, under
contexts of what a match finder sees in the data. Any stream that reads whole (§3.4) has a
record.

The document is normative and complete: an implementation of the module can be written from it
alone. Sections marked *informative* describe what was seen of Windows' encoder; a decoder needs
none of it.

External reference: the LZNT1 variant of Microsoft's Xpress Compression Algorithm ([MS-XCA],
§2.5). §3 restates what this document needs of it. aqr-lznt1 was written from that description,
without any implementation's code.

Contents:

1. [Conventions](#1-conventions)
2. [Overview](#2-overview)
3. [LZNT1](#3-lznt1)
4. [The record](#4-the-record)
5. [Formula module](#5-formula-module)
6. [Windows' encoder](#6-windows-encoder-informative) *(informative)*

## 1. Conventions

- Integers are unsigned unless said otherwise. Words are little-endian.
- `D` is the data of a unit, `N` its length; `D[a..b)` are the bytes from `a` to `b − 1`.
- `lcp(a, b, cap)`, `a < b`: the length of the common prefix of `D[a..)` and `D[b..)`, at most
  `cap` (and at most the data that is there).
- **varint**: unsigned LEB128, least significant group first (the encoder writes the shortest
  form).

## 2. Overview

A **stream** is **units** one after another. A unit holds **U** bytes of data, the same for every
unit of the stream but the last, which holds fewer: NTFS's compression units (16 clusters, 64 KiB
with clusters of 4 KiB), or a whole buffer of `RtlCompressBuffer`'s as the one unit. The data of
a stream is its units' data one after another. A unit is

- a **hole**: no bytes, its data zeros (a sparse unit of a compressed NTFS file);
- **stored**: as many bytes as its data, which they are (the unit did not shrink);
- **compressed**: LZNT1 chunks (§3), perhaps an end header, then padding up to the unit's
  allocation.

The record of a stream tells U and the cluster the allocations are multiples of, and for each
unit its kind and, for a compressed one, its chunks' parse (§4.4) and what follows (§4.5). A
container (NTFS in aqr-disk) cuts a file's clusters into units; the module (§5) rebuilds the
units one after another, between bytes the container keeps.

A compressed unit may hold more data than its unit has: NTFS compresses the unit of a file's end
whole, and its zeros past the end are compressed with the rest. They are not in the data; they
are zeros, and the record only has their chunks (§4.3).

## 3. LZNT1

### 3.1 Chunks

A compressed unit's bytes start with chunks. A chunk is a header word, then its bytes. Bits 12 to
14 of the header are 3, bits 0 to 11 are the size of the chunk's bytes after the header, less 1
(1 to 4096 bytes); bit 15 tells whether the chunk is compressed. A **stored** chunk (bit 15
clear) holds its data as it is, 1 to 4096 bytes. A **compressed** chunk holds **groups**: a flag
byte, then up to 8 tokens, one for each flag bit from the lowest. A flag bit 0 is a literal, one
byte; 1 is a match, a word `w` (§3.2). The last group has fewer than 8 tokens where the chunk's
bytes end; its other flag bits are the **slack**, which the writer leaves as it likes (zeros, in
every sample). A flag byte is always followed by a token. A chunk holds 1 to 4096 bytes of data
(a chunk that expands to more is not read). Chunks follow one another.

### 3.2 Matches

Of a chunk with `done` bytes of data made so far, a match word `w` has `m` bits of displacement
and `16 − m` of length, where `m` is the largest value in 4 to 12 with `2^(m−1) < done`, else 4:
`disp_bits(done) = clamp(bitlen(done − 1), 4, 12)`. The offset is `(w >> (16 − m)) + 1` and the
length `(w & (2^(16−m) − 1)) + 3`. The offset is at most `done` (a match stays within its chunk)
and the data of the chunk is at most 4096 bytes. A match copies `length` bytes from `offset`
back, one by one (it may overlap itself). The longest match at `done` is thus
`2^(16 − m) + 2` bytes: 4098 at the chunk's start, 18 from 2049 bytes on.

### 3.3 Units

After the chunks of a compressed unit comes an **end header**, a word of 0, if the unit has room
for it (and the writer writes it); the unit's allocation then goes on with padding whose bytes
the writer leaves as they were. A unit is read as: chunks while a chunk reads whole (the first
must), then the end header if the next two bytes are 0, then the padding to the end. The
**allocation** `A` of a unit is its length in bytes.

### 3.4 What is read

A unit is read if its first chunk reads whole (§3.1, §3.2). Whatever after the chunks is not a
chunk that reads whole is padding. A stream of units is read if every compressed unit is, the
units all have U bytes of data but the last, which has 1 to U, every compressed unit's chunks
hold at least its bytes of data and at most U, and the data they hold past its own is zeros.
Writing the same chunks (§3.1, §3.2, with the lengths in their only form) gives the bytes
back byte for byte.

## 4. The record

A stream's record is a binary arithmetic code of its units given their data, its contexts going
on from unit to unit within the stream.

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

A decoder that has read more than 4 bytes past the end of the record, after a unit, fails.

Each **adaptive bit** has `p1` (initially 32768) and a count `c` (initially 0). After coding a
bit: `p1 += floor((t − p1) × RATE[c] / 65536)` with `t = 65535` for a 1 and 0 for a 0,
`RATE[i] = floor(131072 / (2i + 3))`, then `p1` is clamped to 32 … 65504 and `c`, if below 30,
grows by 1. A **raw bit** has `p1 = 32768` and is not adapted; the raw bits of a value go
highest first.

- `gamma(t)`, `t` a table of 32 adaptive bits, codes `v` as `x = v + 1`, `b = bitlen(x) − 1`:
  `b` ones then a zero, the `i`-th of them (from 0) with `t[min(i, 15)]`; decoding fails at a
  31st one. Then the `b` bits of `x` below its top one, highest first: the first with
  `t[16 + min(b, 15)]`, the others with `t[31]`.

`lb(n)`, the bucket of a length: 0 for `n < 3`, then 1 for 3; 2 for 4–5; 3 for 6–8; 4 for 9–16;
5 for 17–32; 6 for 33–128; 7 above. `ob(o)`: 0 for 0, else `floor(log2(o)) + 1`.

All contexts are fresh at the start of a stream's record:

| context | size | index |
|---|---|---|
| `compressed`, `hole` | 3 each | the last unit's kind: 0 compressed, 1 stored, 2 hole (0 at the start) |
| `std` | 1 | |
| `count`, `len`, `slack`, `alloc_by` | 1 each (gamma) | |
| `stored` | 2 | whether the last chunk of the unit was stored |
| `slack_nz`, `alloc_eq`, `pad_nz` | 1 each | |
| `end` | 2 | whether the last compressed unit had an end header (0 at the start) |
| `is_match` | 8 × 17 × 3 × 2 × 2 | `((((lb(win) × 17 + ob(winoff)) × 3 + rel) × 2 + prev) × 2 + two)` |
| `ab_win`, `ab_other` | 8 each | `lb(win)` |
| `is_setter` | 8 × 8 | `fx = lb(near) × 8 + lb(win)` |
| `setter` | 8 × 8 (gamma) | `fx` |
| `escape` | 1 | |
| `cand` | 8 × 8 (gamma) | `fx` |
| `short` | 1 | |
| `short_by` | 1 (gamma) | |

### 4.2 The match finder

The contexts of tokens come from a match finder over the unit's data `D` (all its chunks, the
zeros past its data too: for a short unit the module holds them in a copy; a position is its
offset in the unit), which sees one chunk's matches at a time: the **reach** is the chunk's start
`lo`.

- A chain: heads `head[8192]` by `h3(p) = (k3(p) × 0x9E3779B1, wrapping) >> 19` with
  `k3(p) = D[p] | D[p+1] << 8 | D[p+2] << 16`, for `p + 3 ≤ |D|`, and a ring of links
  `prev[4096]`. The heads start as `NONE` at the start of each compressed unit (the links need
  no start: a link is read only after being written).
- Inserting `p` (if `p + 3 ≤ |D|`): `prev[p mod 4096] = head[h3(p)]`, `head[h3(p)] = p`.
- The **candidates** at `pos` (with `pos + 3 ≤ |D|`) start at `c = head[h3(pos)]`, but at its link
  if `c ≥ pos`, and go on by `prev[c mod 4096]` while `c ≠ NONE` and `c ≥ lo`.
- **two(pos, cap)**: `(0, 0)` twice if `cap < 3`. Else walk the first 16 candidates `c` (links of
  the chain, hash collisions among them), and let the first two with `n = lcp(c, pos, cap) ≥ 3`
  be `A` and `B`, each the pair `(n, pos − c)`; a missing one is `(0, 0)`.
- **setters(pos, cap)**: walking the first 4096 candidates, with `best = 2`: each candidate `c`
  with `lcp(c, pos, cap) = n > best` is a **record setter**, of offset `pos − c` and length `n`,
  and sets `best = n`; the walk stops where `best ≥ cap`. They come nearest first.
- **offsets(pos)**: of the first 4096 candidates `c`, in order, those with `lcp(c, pos, 3) = 3`
  give the offsets `pos − c`, nearest first.

Before the tokens at `pos` are looked at, `pos` is inserted (§4.4); in the stream's mode `all`
(§4.3), after a match at `pos` its other positions are inserted too, else only the positions
where tokens start are in the chains.

### 4.3 A stream and its units

A record starts with the **unit length** `U`: `gamma` under a table of 32 adaptive bits; then
the **cluster** `C`: `gamma` under the same table (as it is after `U`); then one bit of an
adaptive bit of its own, `all` (a fresh one: the bit costs one bit). `U` and `C` are at least 1;
`U` is at most the greater of the stream's length of data `L` and 1 MiB. The units are the
`U`-byte pieces of the data, the last shorter. A stream of no data has an empty record (and
fails otherwise). A data of 2^32 − 4096 bytes or more fails.

For each unit (`n` bytes of data), `kind`: the bit `compressed`; if it is 0, the bit `hole` (1:
a hole, 0: stored). A hole has no bytes. A stored unit is its `n` bytes of data. A compressed
unit goes on:

1. The chunk lengths: the bit `std`. 1: the **standard** lengths for `U`: `ceil(U / 4096)`
   chunks of 4096 bytes of data, the last holding `U mod 4096` instead if that is not 0. 0:
   `gamma(count)` gives the number of chunks less 1 (at most `U`), then for each `gamma(len)` its
   data less 1 (at most 4095). The sum `S` of the lengths must be at least `n` and at most `U`;
   the unit's chunks hold `S` bytes of data, `D` is the unit's `n` bytes of data and `S − n`
   zeros. The heads of the match finder are reset.
2. Each chunk, `base` the data before it: the bit `stored[prev]`. 1: a stored chunk of its data.
   0: its tokens (§4.4) from `base` to its end; then, if the number of tokens is not a multiple
   of 8 (`k` of them in the last group), the bit `slack_nz`: 1: `v = gamma(slack) + 1` is the
   slack (the unused flag bits, `0 < v < 2^(8−k)`, else it fails); 0: zero.
3. The bit `end[prev]`: 1: an end header follows the chunks.
4. The allocation `A`. With `from` the bytes of the chunks and the end header, `pred = from`
   rounded up to a multiple of `C`: the bit `alloc_eq`. 1: `A = pred`. 0: `A = from +
   gamma(alloc_by)`.
5. If `A > from`: the bit `pad_nz`. 1: the `A − from` bytes of padding follow, 8 raw bits each.
   0: they are zeros.

The unit is written as soon as it is known: stored chunks and chunks with their header (§3.1,
§3.2), the end header, the padding.

### 4.4 Tokens

At `pos`, with `done = pos − (chunk start)`, `cap = min(chunk end − pos, 2^(16 − disp_bits(done)) +
2)` and `lo` the chunk's start:

1. `(A, B) = two(pos, cap)`, or `(0, 0)` both at the chunk's start (`done = 0`). `win` is `B` if
   `B`'s length is more than `A`'s, else `A`; `other` is the other one. Insert `pos`.
2. `rel`: 0 if the length of `win` is below 3 or `cap ≤ 3`; else with `next` the greater length
   of `two(pos + 1, cap')`, `cap' = min(chunk end − pos − 1, 2^(16 − disp_bits(done + 1)) + 2)`:
   0 if `next` is at most `win`'s, 1 if it is one more, else 2. (`pos` is among the candidates of
   `pos + 1`.)
3. If `cap < 3` or `done = 0` the token is a literal and nothing is coded. Otherwise the bit
   `is_match`, with `prev` the last token's kind in this chunk (1 for a match, 0 for a literal or
   the first) and `two` 1 if `other` exists. 0: a literal, `pos` advances by 1.
4. A match. If `win` exists, the bit `ab_win`: 1: the match is `win`, with its length and offset.
   Else, if `other` exists, the bit `ab_other`: 1: the match is `other`. Else `fx` as in §4.1 and
   the bit `is_setter[fx]`:
   - 1: `rank = gamma(setter[fx])`; the match is the `rank`-th record setter of
     `setters(pos, cap)` (from 0); none such fails. It is set where the match is the whole of its
     offset's (`len = lcp(pos − off, pos, cap)`), within the reach, and a record setter.
   - 0: the bit `escape`: 1: the offset is 13 raw bits. 0: `rank = gamma(cand[fx])`, and the
     offset the `rank`-th of `offsets(pos)` (none such fails). Then with `m =
     lcp(pos − off, pos, cap)`, the bit `short`: 1: `by = gamma(short_by)` and `len = m − by − 1`
     (it fails if that is below 1); 0: `len = m`.
5. The match fails unless `3 ≤ len ≤ cap`, `1 ≤ off ≤ done` and `lcp(pos − off, pos, len) =
   len`. The token is pushed and `pos` advances by `len`.

The encoder sets `ab_win` and `ab_other` where the match is the whole match of such a candidate
(`ab_win` 0 and `ab_other` 0 are coded as the bits say whether it is not), `is_setter` where it
can, and `escape` where the offset is none of the candidates.

### 4.5 What the record leaves to the stream

The padding and the end header come from the record alone; the data of the stream is only the
data. A decoder checks that each chunk written is of at most 4096 bytes after its header (a
token that would make more fails) and that the output does not pass the application's length.

## 5. Formula module

The module is `lznt1-recons`, version 1. It rebuilds one or more streams, each from its data and
its record, between bytes it is given as they are: its output is a part of a container as it is.

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

The output is, for each stream in order, the bytes kept before it, then the bytes its data and
record rebuild (§4): its units' bytes one after another (holes none); then the bytes kept after
the last. Streams are rebuilt independently of each other (each record's contexts fresh). The
output must be the application's declared size; a stream passing it fails at the chunk or unit
that does.

**Resources**: none.

The module fails on other parameters, another count of inputs or resources, an index that does
not cut the inputs exactly, a record that fails (§4), or an output of another length.

**Memory** *(informative)*: the module lays its memory out as a stack (`aqr_api::stack`), and the
plan of its memory is exact (`plan::decode`): besides the inputs and the output (with 4,112
bytes to spare for the chunk that passes), the match finder (the heads of 8,192 words and the
ring of 4,096), made once for all the streams; for each stream its contexts, the tokens of a chunk
(4 bytes each, as many as the unit length or 4,096) and, where the data's length is not a multiple
of `U`, a copy of `U` bytes for the short last unit and its zeros. Streams are rebuilt one after
another on one thread.

## 6. Windows' encoder *(informative)*

What NTFS (a Windows 11 volume, `compact /c`, units of 16 clusters of 4 KiB) and
`RtlCompressBuffer` (`COMPRESSION_FORMAT_LZNT1`, the standard engine) write, as seen on the
samples of the tests; the record is cheap because of it.

- A compressed unit is 16 chunks of 4096 bytes of data, then the end header, then zeros up to
  the cluster: the allocation is always the chunks and the end header rounded up to the cluster
  (`alloc_eq` always 1). The last unit of a file is compressed whole, 16 chunks: the 57,535
  bytes of data of a file's end and then zeros, which cost a few bytes a chunk.
- In `RtlCompressBuffer`'s output the chunks are 4096 bytes of data but the last, and there is
  no end header. A buffer that does not compress is written as stored chunks (64 of 4098 bytes
  for 256 KiB: 128 bytes more than the data); the units of NTFS in the samples all compressed.
  Stored chunks inside a compressed unit were not seen.
- The slack (the unused flag bits of the last group) is always zero.
- A match is always the whole match at its offset, up to the longest the position gives (4098
  bytes at the start of a chunk, 18 from 2049 bytes on): lengths cost nothing in the record.
  Matches of 3 bytes are common (a third of them in text).
- The encoder is greedy: it takes a match wherever it has one, whatever the next position
  has (`rel` does not matter). Its candidates are the positions where tokens started (not the
  ones inside matches; the record's `all` is 0, and the record is about 8 times larger
  otherwise), and it looks at two of them for each position: the nearest two whose first 3 bytes
  agree (the match was never the third in the samples). It takes the longer of the two, the
  nearer on ties; in under 1% of the matches it takes the shorter, presumably where its own
  table has lost the longer. That is nearly all of the record's offsets, 0.15% of the stream.
- Where it has a candidate it still writes a literal now and then (about 1 in 70 positions
  with one after the first 2 KiB of a chunk, far fewer before 512 bytes): its hash table
  probably loses candidates there, and the model cannot see which. These decisions are most of
  the record, 0.3% of the stream.
