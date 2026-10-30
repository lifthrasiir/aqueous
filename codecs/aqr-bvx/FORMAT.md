# aqr-bvx format

aqr-bvx splits a stream of Apple's compression blocks (LZFSE, LZVN and stored blocks, each of a
magic `bvx?`) into its decompressed **data** and a **record** of everything else: how the data
is cut into blocks, the parse of each block (its literals and matches), and the few values of an
LZFSE block that are not what the compressor writes. From the data and the record the stream is
rebuilt bit for bit. This document describes the streams (§2–§4), how a parse and the values
determine a stream (§5), the record (§6), and the [formula module](#7-formula-module).

The record does not reproduce the compressor's parse: it codes any parse given the data, under
contexts of what a match finder sees there. In LZFSE blocks one of those match finders is the
compressor's own table of candidates (§6.3), whose matches the compressor nearly always takes;
the record of libcompression's streams costs 1–2.5% of the streams.

The document is normative and complete: an implementation of the module can be written from it
alone. Sections marked *informative* describe the compressor and the reference encoder's
choices; a decoder needs none of them.

External reference: none. LZFSE and LZVN have no published specification. This document
describes them as aqr-bvx reads and writes them; aqr-bvx was written from what Apple's
libcompression (macOS) outputs and accepts, used as a black box, without the code of Apple's
or anyone else's implementation.

Contents:

1. [Conventions](#1-conventions)
2. [Streams](#2-streams)
3. [LZFSE blocks](#3-lzfse-blocks)
4. [LZVN blocks](#4-lzvn-blocks)
5. [The plan of a stream](#5-the-plan-of-a-stream)
6. [The record](#6-the-record)
7. [Formula module](#7-formula-module)
8. [The compressor and the encoder](#8-the-compressor-and-the-encoder-informative) *(informative)*

## 1. Conventions

- Integers are unsigned. `le16`, `le32` and `le64` are little-endian integers of 16, 32 and 64
  bits. Arithmetic is exact unless marked *wrapping* (modulo 2^32).
- `D` is the data of a stream, `N` its length; `D[a..b)` the bytes from `a` to `b − 1`.
- `bitlen(x)` is the number of bits of `x` (0 for 0).
- `lcp(a, b)`, `a < b`: the length of the common prefix of `D[a..E)` and `D[b..E)`, where `E`
  is the end of the segment being walked (§6.2).
- **varint**: unsigned LEB128, least significant group first.
- A **bit string** of bytes is read with bit `i` being bit `i mod 8` (least significant first)
  of byte `⌊i / 8⌋`.

## 2. Streams

A stream is a sequence of blocks, each starting with 4 bytes of magic, and ends with the end
block `bvx$` (4 bytes alone). Its data is the data of its blocks one after another.

| magic | block | after the magic |
|---|---|---|
| `bvx-` | stored | `le32 n`, then `n` bytes of data |
| `bvxn` | LZVN | `le32 n` (its data's bytes), `le32 m`, then an LZVN stream of `m` bytes (§4) |
| `bvx2` | LZFSE | §3 |
| `bvx$` | the end | nothing |

LZFSE's first block version (`bvx1`) is not taken. Matches of LZFSE and LZVN blocks may reach
into the data of the blocks before them. A stream taken has at least one block before its end
(for an empty stream, the compressor writes a stored block of no data).

## 3. LZFSE blocks

### 3.1 The header

| bytes | content |
|---|---|
| 0–3 | `bvx2` |
| 4–7 | `le32` the block's bytes of data |
| 8–15 | `le64 F0` |
| 16–23 | `le64 F1` |
| 24–31 | `le64 F2` |
| 32 … `H − 1` | the frequencies (§3.2) |
| `H` … | the literals' stream (`Lb` bytes), then the triples' stream (`Tb` bytes) |

with these fields (bits `a–b` of a word, the least significant bit 0):

| word | bits | field |
|---|---|---|
| `F0` | 0–19 | `NL`, the literals (a multiple of 4) |
| | 20–39 | `Lb`, bytes of the literals' stream |
| | 40–59 | `NT`, the triples |
| | 60–62 | `lit_bits`: the literals' stream's top `7 − lit_bits` bits are unused |
| | 63 | 0 |
| `F1` | 0–39 | the literals' four coders' states, 10 bits each, the first lowest |
| | 40–59 | `Tb`, bytes of the triples' stream |
| | 60–62 | `lmd_bits`, as `lit_bits` for the triples' stream |
| | 63 | 0 |
| `F2` | 0–31 | `H`, bytes of the header (at least 32) |
| | 32–61 | the states of the coders of `L`, `M` and `D`, 10 bits each |
| | 62–63 | 0 |

The block ends at byte `H + Lb + Tb`.

### 3.2 The frequencies

Four tables of frequencies follow, of `L` (20 symbols), `M` (20), `D` (64) and the literals
(256), 360 values in this order, as a bit string. Each value has this code, read from its lowest
bit (the bits are listed lowest first, `v…` the bits of `v` lowest first):

| value `f` | bits | code |
|---|---|---|
| 0 | 2 | `0 0` |
| 1 | 2 | `0 1` |
| 2 | 3 | `1 0 0` |
| 3 | 3 | `1 0 1` |
| 4–7 | 5 | `1 1 0`, then `f − 4` in 2 bits |
| 8–23 | 8 | `1 1 1 0`, then `f − 8` in 4 bits |
| 24–1047 | 14 | `1 1 1 1`, then `f − 24` in 10 bits |

The bits after the last value up to `H` are zeros, fewer than 8 of them: `H = 32 + ⌈B / 8⌉` for
`B` bits of codes.

### 3.3 Tables of states

A table of `S` states (64 for `L` and `M`, 256 for `D`, 1024 for the literals) is made from
frequencies `f[s]` whose sum is at most `S`. The symbols of nonzero frequency, in order, take
consecutive states, `f[s]` each from 0 on. For a symbol of frequency `f`, `k` is the shift with
`S ≤ f · 2^k < 2S`, and `j0 = ⌊2S / 2^k⌋ − f`; its `j`-th state (`0 ≤ j < f`) reads

- if `j < j0`: `k` bits, and the next state is `(f + j) · 2^k − S` plus them;
- else: `k − 1` bits, and the next state is `(j − j0) · 2^(k−1)` plus them.

The states beyond the sum of the frequencies are none: a coder in one fails.

`L`, `M` and `D` are values of symbols with extra bits:

| | symbols 0–15 | symbols 16–19 |
|---|---|---|
| `L` | value `s`, no extra bits | bases 16, 20, 28, 60; extra bits 2, 3, 5, 8 |
| `M` | value `s`, no extra bits | bases 16, 24, 56, 312; extra bits 3, 5, 8, 11 |

`D`'s symbol `s` (0 to 63) has `e = ⌊s / 4⌋` extra bits, and its base is the sum of `2^⌊t / 4⌋`
for `t < s` (0, 1, 2, 3, 4, 6, 8, 10, 12, 16, …). A value's symbol is the last whose base is at
most it, and its extra bits are the value less that base. The largest values are `L ≤ 315`,
`M ≤ 2359` and `D ≤ 262139`.

### 3.4 The streams

Both streams are read backward: a stream of `b` bytes and `u` unused top bits holds the bit
string's bits `0 … 8b − u − 1`, read from the top down, each field highest bit first (a field of
`n` bits read at position `p` is bits `p − n … p − 1`, its top bit the field's top bit). The bits
below the last field read must be zeros.

**Literals.** `NL` literals are read by four coders in turn, literal `i` by coder `i mod 4`,
each starting at its state of `F1` in the literals' table: a coder in state `x` gives the
symbol of `x` and reads its bits for the next state.

**Triples.** `NT` triples are read, each `L`, then `M`, then `D`, each by its coder starting
at its state of `F2`: in state `x` of symbol `s` with `e` extra bits, `k + e` bits are read as
one field `v` (`k` the bits of state `x`); the value is `s`'s base plus the low `e` bits of `v`,
and the next state is `x`'s base (above) plus `v >> e`.

### 3.5 The data

The decoder's distance `dist` is 0 at the start of each block. For each triple in turn: the
next `L` literals are data; if `D ≠ 0`, `dist = D`; then `M` bytes are copied from `dist` back,
one at a time (so `dist < M` repeats the last `dist` bytes). `M > 0` with `dist = 0`, or a
distance beyond the data decoded so far, fails. The triples must use at most `NL` literals (the
others pad the literals to a multiple of 4) and give exactly the block's bytes of data.

### 3.6 Writing a block

A block is written from its literals (with the pad), its triples, its four tables of
frequencies, the states its coders end at (`lit_start[4]`, `lmd_start[3]`) and the zero bits
below its streams' fields (`lit_low`, `lmd_low`):

- The literals' stream: `lit_low` zero bits, then for `i` from `NL − 1` down to 0, with `x` the
  state of coder `i mod 4` (initially `lit_start`): the state `x'` of the literal's symbol from
  which reading its bits leads to `x` (there is one), and those bits, written above the bits
  before (the next state's bits); `x = x'`. The bits, padded with zeros to a whole byte, are the
  stream, and `lit_bits = 7 −` the padding; the coders' last states are `F1`'s.
- The triples' stream: `lmd_low` zero bits, then for each triple from the last back to the first,
  `D`, then `M`, then `L`, each as one field (the state's bits above its extra bits), likewise.
- The frequencies as in §3.2, and `NL`, `NT`, `Lb`, `Tb` and `H` as they come.

## 4. LZVN blocks

An LZVN stream is a sequence of instructions, each an opcode byte and its operands, followed by
the literals it copies; a match is copied after its literals. `prev` is the distance before (0 at
the start of the block).

| opcode | name | literals | match | distance |
|---|---|---|---|---|
| `LLMMMDDD`, `DDD` < 6, then a byte `X` | `sml_d` | `L` (0–3) | `M + 3` | `DDD · 256 + X` |
| `LLMMM110`, `LL > 0` | `pre_d` | `L` (1–3) | `M + 3` | `prev` |
| `LLMMM111`, then `le16 X` | `lrg_d` | `L` (0–3) | `M + 3` | `X` |
| `101LLMMM`, then `le16 X` | `med_d` | `L` (0–3) | `(MMM · 4 + (X & 3)) + 3` | `X >> 2` |
| `1110LLLL`, `L > 0` | `sml_l` | `L` (1–15) | | |
| `11100000`, then a byte `X` | `lrg_l` | `X + 16` | | |
| `1111MMMM`, `M > 0` | `sml_m` | | `M` (1–15) | `prev` |
| `11110000`, then a byte `X` | `lrg_m` | | `X + 16` | `prev` |
| `00000110` | `eos` | the end | | |
| `00001110`, `00010110` | `nop` | | | |

Other opcodes (`0x1E`, `0x26`, `0x2E`, `0x36`, `0x3E`, `0x70`–`0x7F`, `0xD0`–`0xDF`) fail. A
distance becomes `prev`. A distance of 0, or beyond the data decoded so far, fails. The block's
data is exactly its bytes; the stream is written ending with `eos` and seven zero bytes.

## 5. The plan of a stream

A stream is determined by its data and its **plan**:

- the **parts**, one per block in order: *stored* of `n` bytes; *LZVN* of `n` bytes of data;
  *LZFSE* of `n ≥ 1` triples and a **side** (below);
- the **parse** of each **segment**: of an LZVN part, or of a run of LZFSE parts one after
  another (LZFSE parts next to each other are always one segment). A parse is a list of
  matches, each `(lits, len, dist)` (`len ≥ 1`, `dist ≥ 1`: `lits` literals, then `len` bytes
  copied from `dist` back), and `tail`, the literals after the last; it covers the segment's
  data exactly.

An LZFSE part's side: the frequencies (or *normalized*: §5.3), `lit_start`, `lmd_start`,
`lit_low`, `lmd_low` (§3.6), the **pad** (the literals after the block's own) and the **D of
each triple of no match**, in order. It is **usual** where the frequencies are normalized, the
start states all 0, `lit_low = 0` and `lmd_low = 64`.

### 5.1 LZVN instructions

An LZVN part's stream is the instructions of its parse, then the end (§4). `prev = 0`; for each
match `(r, n, d)`:

1. If `r ≥ 4`: literal instructions for `r` (below), and `r = 0`. Let `cap = 10 − 2r`.
2. If `d = prev`: `sml_m` (`r = 0`) or `pre_d` (`r > 0`) of `k = min(n, cap)`.
3. Else if `d < 1536`: `sml_d` of `r` literals and `k = min(n, cap)`.
4. Else if `d < 16384` and `cap < n ≤ 34 − 2r`: `med_d` of `r` literals and `k = n`.
5. Else `lrg_d` of `r` literals and `k = min(n, cap)` (`d < 65536`).
6. The rest, `n − k`, as match instructions (below); `prev = d`.

Then the tail as literal instructions. **Literal instructions** for `r`: while `r > 0`, one of
`t = min(r, 271)` (`sml_l` for `t < 16`, else `lrg_l`), `r −= t`; **match instructions** alike,
`sml_m` and `lrg_m`. An instruction whose fields do not hold (a match of `k < 3` in steps 2–5
but `sml_m`, a distance beyond its field) fails.

### 5.2 LZFSE triples

The triples of an LZFSE segment's parse: for each match `(r, n, d)`, `⌊(r − 1) / 315⌋` triples
`(315, 0, —)` for `r > 0`, then with `r'` the literals left, `(r', min(n, 2359), d)` and
`(0, min(n', 2359), d)` for what is left `n'` of the match, until all of it is. Then the tail
alike: its triples `(315, 0, —)` and `(r', 0, —)` for `r' > 0`. A triple of no match (`—`) has
its `D` from the side.

The parts cut these triples in order, `n` each, and must use them all. In each block, a match's
distance is written as `D = 0` where it is the block's previous nonzero `D` (none at the start of
a block), else as itself; a triple of no match takes the next of its part's `D`s (a part's `D`s
must be as many as its triples of no match). The literals are the data of the triples' `L`,
then the pad, which makes them a multiple of 4 (its length is `(4 − l mod 4) mod 4` for `l`
literals of the triples).

### 5.3 Normalized frequencies

The block's symbols are counted: of each triple's `L`, `M` and `D` (`D` as written), and of its
literals with the pad. A table of counts `c` with total `T > 0` is normalized to `S` states:

1. `f[s] = 0` for `c[s] = 0`, else `max(1, ⌊(2 · c[s] · S + T) / (2T)⌋)`. `left = S − Σ f`; `m`
   the first symbol of the largest `f`.
2. If `−left < ⌊f[m] / 4⌋` (as integers, `left` may be negative): `f[m] += left`. Done.
3. Else, with `over = −left` and `shift = 3`, while `over > 0`: for each symbol in order with
   `f[s] > 1`: `t = min(⌊(f[s] − 1) / 2^shift⌋, over)`, `f[s] −= t`, `over −= t`, stopping the
   pass where `over = 0`; then `shift = max(shift − 1, 0)`.

A table of no symbols (no triples, or no literals) is all zeros.

## 6. The record

### 6.1 Coding

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
the adaptive bit `t[min(i, 63)]` (`t` has 64); then the `bitlen(x) − 1` bits of `x` below its
top one, raw. Decoding fails at a 31st one.

The record codes, in order: the segments and parts with their parses and sides (§6.2–§6.5),
then the pads and `D`s of every LZFSE part (§6.6). All contexts are fresh at the start; LZFSE
and LZVN segments have their own contexts of §6.4.

### 6.2 Segments and parts

If `N = 0`: an adaptive bit `rest`, 1 for a single stored part of 0 bytes (else no part), and
nothing more. Else, from `at = 0` until `at = N`:

- the kind: adaptive bit `kind[0]`, 0 for LZFSE; else `kind[1]`, 0 for LZVN, 1 for stored;
- the bytes `n` of data: adaptive bit `rest`, 1 for `n = N − at`; else `n = 1 + gamma(len)`,
  which must be below `N − at`;
- stored: a stored part of `n` bytes. LZVN: an LZVN part of `n` bytes, and its parse (§6.4)
  over `D[at..at + n)`. LZFSE: the parse (§6.4) over `D[at..at + n)`, then its parts: with `T`
  its triples (§5.2) and `done = 0`, while `done < T`: `u = min(9992, T − done)`; adaptive bit
  `full_block`, 1 for a part of `u` triples, else `1 + gamma(block_len)` triples, which must be
  at most `T − done` and not `u`; then the part's side (§6.5); `done +=` its triples.
- `at += n`.

### 6.3 Match finders

A segment's parse is walked over `D[0..E)`, `E` the segment's end, the positions before its
start in reach. Two structures of positions are kept.

**The chain.** `min` is 4 in LZFSE segments, 3 in LZVN's, and `W` (the farthest a match
reaches) 262139 and 65535. `key(p)` is the `le32` at `p` (4 bytes) or `D[p] | D[p+1] << 8 |
D[p+2] << 16`; `h(p) = (key(p) × 0x9E3779B1, wrapping) >> 14`. `head[2^18]` and a ring
`prev[2^18]` start as none. Inserting `p` (where `p + min ≤ E`): `prev[p mod 2^18] =
head[h(p)]`, `head[h(p)] = p`. Positions are inserted in order, all up to the one looked at,
but for the middle of long matches: after a match at `p` of `n > 64` bytes, the positions from
`p + 32` to `p + n − 33` are left out (those below `p + 32` are inserted first).

The **candidates** at `pos` (`pos + min ≤ E`): from `c = head[h(pos)]`, skipping `pos` itself,
then `c = prev[c mod 2^18]`, while `c` is not none, `pos − c ≤ W`, the next `c` is below the one
before, and at most `4q + 64` positions are seen for `q` candidates wanted: the positions `c`
with `key(c) = key(pos)`, nearest first, their **rank** counted from 0.

- `near(pos)`: the longest `lcp(c, pos)` of the first 4 candidates (0 for none).
- `pairs(pos)`: the list of `(length, position, rank)`: of the first 32 candidates, each `c`
  whose `lcp(c, pos) = l` is longer than every one before it, stopping once `l = E − pos`. Its
  lengths increase, and so do its distances.
- `nearest(pos, n)`, `n ≥ min`: the first of the first 1024 candidates with `lcp(c, pos) ≥ n`.

**The table** (LZFSE segments only): `2^14` buckets of 4 positions, all none at the start.
`b(p) = (le32 at p × 0x9E3779B1, wrapping) >> 18`. Every position `p` with `p + 4 ≤ E` is
inserted in order from 0 (the bucket's positions moved one place down, the last dropped, `p`
first). A position's **candidates** are looked up just before it is inserted: the positions of
its bucket `q` with `pos − q ≤ 262139` and the same 4 bytes. Then, for `pos`:

- `A`: the best of the candidates of `pos`, of length `lcp(q, pos)` and distance `pos − q`;
- `B`: the best of the candidates `q` of `pos + 1` (if `pos + 1 < E`) with `q > 0` and
  `D[q − 1] = D[pos]`, of length `1 + lcp(q, pos + 1)` and distance `pos + 1 − q`;

the best being the longest, then the nearest; none (length 0) where there is no candidate. A
table's lengths are taken up to `cap` (∞, or 64 below): `min(length, cap)`.

**Guesses** of `(A, B)`: the first is `B` if `B` is longer than `A` by 2 or more, else `A`; the
second is the other. Their **class**: 0 if neither is; 1 if only `B` is; 2 if only `A` is; 3 if
`B` is at most as long as `A`; else 3 + `min(B − A, 3)` (lengths `A`, `B`).

### 6.4 The walk

The parse of a segment from `pos = start` with `run = 0`, until `pos = E`:

```text
LZFSE:  (g0, g1, class) = guesses of the table at pos;  l0 = the longer of g0, g1
        l1 = the length of the first guess at pos + 1, lengths up to 64 (0 if pos + 1 = E)
LZVN:   l0 = near(pos), the chain holding the positions below pos;
        l1 = near(pos + 1), the chain holding those below pos + 1 (0 if pos + 1 = E)
(pairs and nearest below look at the chain holding the positions below pos + 1)
cmp = 2 if l1 > l0, 1 if l1 = l0, else 0
bit start[(bucket(run) × 12 + lb(l0)) × 3 + cmp]: 0 a literal (pos += 1, run += 1), next
1: a match after run literals:
  LZFSE, where g0 is not none:
    bit best[class × 12 + lb(len g0)]: 1 the match is g0
    else, where g1 is not none and not g0: bit second[class × 12 + lb(len g1)]: 1 it is g1
    a match so given ends the step (below)
  P = pairs(pos), of m entries
  j = 0;  while j < m and bit take[min(j, 3) × 9 + r] = 0: j += 1
      (r the rank of P[j + 1], at most 7, or 8 if j + 1 = m)
  if j = m:
    n = top + 1 + gamma(longer)  (top the length of P[m − 1], or 0);  fails if n > E − pos
    near = nearest(pos, n) if n ≥ min, else none
  else:
    (top, c) = P[j];  lo = (the length of P[j − 1]) + 1 if j > 0, else 1
    bit full[lb(top)]: 1 n = top;  0: n = top − 1 − gamma(short[lb(top)]), which must be ≥ lo
    near = c
  if near is none, or bit far[lb(n)] = 1:
    b by a tree of 5 adaptive bits dist_bits (highest first, node m = 1 then 2m + bit),
      b = the leaf − 32, which must be 1 … 18;  dist = 2^(b−1) + (b − 1 raw bits)
  else dist = pos − near
  fails unless dist ≤ min(pos, W), n ≤ E − pos and lcp(pos − dist, pos) ≥ n
the match (run, n, dist);  pos += n, run = 0
at the end: tail = run
```

with `bucket(x) = min(bitlen(x), 7)` and `lb(l) = l` for `l < 8`, else `min(5 + bitlen(l), 11)`.

| context | size |
|---|---|
| `start` | 8 × 12 × 3 |
| `best`, `second` | 7 × 12 each |
| `take` | 4 × 9 |
| `longer` | 64 (gamma) |
| `full` | 12 |
| `short` | 12 × 64 (gamma) |
| `far` | 12 |
| `dist_bits` | 64 (tree) |

### 6.5 Sides

For each LZFSE part, an adaptive bit `usual`: 1 for a usual side (§5). Else: adaptive bit
`freqs`, 1 for frequencies of the side's own, then the 360 values (§3.2's order) as
`gamma(value)` each; then `lit_start` and `lmd_start`, 10 raw bits each; then
`gamma(value)` for `lit_low` and for `lmd_low`. A side coded so that is usual fails. (`value`
is one context of 64 bits shared by these.)

### 6.6 Pads and distances of no match

Once all parts are known, for each LZFSE part in order, its pad (whose length §5.2 gives) and
its `D`s (as many as its triples of no match): each byte of the pad, adaptive bit `pad_zero`, 1
for 0, else 8 raw bits; each `D`, adaptive bit `zd_zero`, 1 for 0, else `1 + gamma(zd)`.

## 7. Formula module

The module is `bvx-recons`, version 1. It rebuilds a stream from its data and its record.

**Parameters**: none (empty).

**Inputs**: exactly two, in this order.

| slot | content |
|---|---|
| 0 | the data |
| 1 | the record |

The output is the stream of the plan that the record codes over the data (§5, §6), up to and
with its end block.

**Resources**: none.

The module fails on any parameters, any other count of inputs or resources, a record that fails
above, a plan whose stream cannot be written (§3.6, §5), or an output whose length is not the
application's declared output size.

## 8. The compressor and the encoder *(informative)*

What libcompression's compressor (`COMPRESSION_LZFSE`; the buffer and the stream interfaces, as
`compression_tool` and asset catalogs have them) was seen to do, which the record's contexts
follow:

- Data of fewer than 4096 bytes or so is one LZVN block (a few bytes, a stored block); more, LZFSE
  blocks of 9992 triples, the last fewer. The stream interface cuts its input into blocks of its
  own (73 KB or so).
- LZFSE: every match is at least 4 bytes, cannot be extended forward or backward, and is the
  nearest of its length. It comes from the table of §6.3 (14-bit hash, buckets of 4, every
  position inserted): the best match of the position's bucket, or one of the next position's
  extended back a byte; the record's guesses are those two. When a match starts depends on the
  lengths there and at the next position, and on more than the record models (a match of 4
  bytes with nothing after is taken about half the time).
- LZFSE blocks: start states 0, 64 zero bits below the triples' stream and none below the
  literals', the pad zeros, frequencies normalized as §5.3, a repeated distance always as
  `D = 0`. The `D` of the last triple (of no match) is sometimes not 0. Rarely, a match is cut
  in two at a window edge of the compressor (at 0xBFFFF of a stream of 1.4 MB): the record
  codes both pieces.
- LZVN: the instructions of §5.1, the end `eos` and seven zero bytes.

The encoder (`recons::split`) decodes the stream, reads its plan (a triple of `L = 0` and
`D = 0` after a triple of the longest match continues that match; likewise LZVN's match
instructions after a match), checks that the plan writes the stream back, and codes the record.
`formula::encode` takes a stream at the start of its input that holds some data, as far as the
blocks' headers tell (a stream found by aqr-compress in other data: asset catalogs hold one per
image); the data goes in a stream of class `raw`, which the formula may recompress in turn, the
record in `bvx/record`, and what follows the stream in one of class `bvx`.
