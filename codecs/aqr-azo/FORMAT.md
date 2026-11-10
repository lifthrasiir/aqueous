# aqr-azo format

aqr-azo splits an AZO stream (ESTsoft's compressor in ALZip's EGG archives, method 3) into the
**data** its blocks decode to and a **record** of the parse its compressor chose. From the data
and the record the stream is rebuilt bit for bit. This document describes AZO streams as far as
the module needs them, the record, and the [formula module](#6-formula-module).

The record does not reproduce ALZip's compressor. It codes the compressor's tokens given the
data, under contexts of what a match finder sees in the data. Any AZO stream whose blocks end as
ALZip ends them has a record. ALZip's matches are nearly always the nearest ones of their lengths
and as long as the data allows, which cost little.

The document is normative and complete: an implementation of the module can be written from it
alone. Sections marked *informative* describe the reference encoder's choices; a decoder needs
none of them.

External reference: OctopusET's
[EGG Archive Format Specification](https://gist.github.com/OctopusET/0ad7da5da238b9153960d0bc4dfe2a50)
(CC0 1.0), Appendix A, cited below as *the spec*. §2 restates what this document needs of it;
where the two differ, this document says so. aqr-azo was written from the spec and from ALZip's
output (the test archives of EggDotNet and alzgg), without any AZO decoder's code.

Contents:

1. [Conventions](#1-conventions)
2. [AZO streams](#2-azo-streams)
3. [The match finder](#3-the-match-finder)
4. [The record of a block](#4-the-record-of-a-block)
5. [The record of a stream](#5-the-record-of-a-stream)
6. [Formula module](#6-formula-module)
7. [Encoder choices](#7-encoder-choices-informative) *(informative)*

## 1. Conventions

- Integers are unsigned. `be32` is a big-endian 32-bit integer. Arithmetic is exact unless
  marked *wrapping* (modulo 2^32).
- `D` is a block's data and `N` its length. `D[a..b)` are the bytes from `a` to `b − 1`.
- `bitlen(x)` is the number of bits of `x` (0 for 0).
- `lcp(a, b)`, `a < b`: the length of the common prefix of `D[a..N)` and `D[b..N)`.
- **varint**: unsigned LEB128, least significant group first, in the shortest form.
- `L = 2^19` (524288), the block size.

## 2. AZO streams

### 2.1 Structure

A stream is the byte `'1'` (0x31, the version), a byte of flags (0 or 1; bit 0 is the x86
filter, §2.6), then blocks, each a header of three `be32`: the data's size `n`, the code's size
`c` and their XOR `n ^ c`, then `c` bytes of code. A header of sizes 0 and 0 ends the stream. A
block of `c = n` is **stored**: its code is its data. Otherwise it is **coded**: its code is the
range code of its tokens (§2.2–§2.5).

Streams the module rebuilds have blocks of `L` bytes of data but for the last, which has 1 to
`L`; the empty stream has none.

### 2.2 Tokens

A coded block's data is its first byte, a literal, then **tokens**, each one of:

| token | meaning |
|---|---|
| `Literal` | the next byte |
| `ReuseRecent(k)`, `k < 2` | the remembered match at `recent_index[k]` (§2.3) again |
| `Reuse(i)`, `i < 128` | the remembered match `i` again |
| `Near(k, n)`, `k < 2` | a match of the recent distance `recent_dist[k]`, of length `n` |
| `Far(d, n)` | a match of distance `d`, length `n` |

A match `(d, n)` at `pos` copies `n` bytes from `d` back one at a time (`d < n` repeats the last
`d` bytes): `1 ≤ d ≤ pos`, `2 ≤ n ≤ 32793` (`MAX_LENGTH`, below), `pos + n ≤ N`.

### 2.3 The history

A block's **history** starts as `recent_dist = [1, 2]`, `recent_index = [0, 1]`, `lengths[i] =
2 + i` and `places[i] = 0` for `i < 128`. `take(list, i)` returns `list[i]` and moves it to the
front (the entries before it move back one); `add(list, v)` puts `v` at the front and drops the
last. The match of a token at `pos`, and the history after it:

- `ReuseRecent(k)`: `i = take(recent_index, k)`; then as `Reuse` below from `n = take(lengths,
  i)`.
- `Reuse(i)`: `add(recent_index, i)`; `n = take(lengths, i)`, `p = take(places, i)`; the match is
  `(pos − p, n)` (wrapping; it must be valid, §2.2).
- `Near(k, n)`: `d = take(recent_dist, k)`; `add(lengths, n)`, `add(places, pos)`.
- `Far(d, n)`: `add(recent_dist, d)`; `add(lengths, n)`, `add(places, pos)`.

### 2.4 The models

The coder's probabilities are counts of a total `2^t`; a bit is 0 with probability `p / 2^t`.

- An **adaptive bit** (the spec's BoolState) has a state `s < 256` (initially 0) and `p[256]`
  (initially 2048), `t = 12`. Coding bit `b` with `p[s]`: `p[s] += (4096 − p[s]) >> 6` for 0,
  `p[s] −= p[s] >> 6` for 1; then `s = (s << 1 | b) & 255`.
- A **tree** of `B` bits (the spec's EntropyBitProb) has `p[2^B]` (initially 512), `t = 10`. A
  value is coded highest bit first, bit `b` with `p[m]`, `m = 1` then `2m + b`; after each,
  `p[m] += (1024 − p[m]) >> 4` for 0, `p[m] −= p[m] >> 4` for 1. *Updating* a tree with a value
  adapts the same probabilities without coding.
- A **predictor** (the spec's PredictProb) of `K` keys, `B` bits and shift `S` has a fine tree
  for each key, a coarse tree for each `key >> S`, and `lucky[K]` (initially 0). A value `v` of
  key `k`: where `lucky[k] ≥ 0`, coded with the fine tree and the coarse one updated with it;
  else the reverse. Then, comparing the two trees as they are now: `P1` and `P2` start at 1; for
  each bit `b` of `v`, highest first, at node `m` of each tree: if `(P1 | P2) & 0xFFC00000 ≠ 0`,
  both are shifted right by 10; then `P1 = P1 × (b ? 1024 − p1[m] : p1[m])` and likewise `P2`
  (wrapping). `lucky[k]` grows by 1 where `P1 > P2` and shrinks by 1 where `P1 < P2`.

A block's models, all fresh at its start: adaptive bits `flag`, `reuse`, `reuse_short`,
`dist_short`; trees `reuse_recent` (1 bit), `reuse_index` (7), `dist_recent` (1), `dist_code`
(7); predictors `alpha` (256 keys, 8 bits, shift 5) and `length` (128 keys, 7 bits, shift 4).

**Codes.** Lengths and distances are each one of 128 codes, a base and extra bits:
`len_extra[i] = 0` for `i < 32`, else `(i − 32) / 8`, `len_base[0] = 2`, `len_base[i + 1] =
len_base[i] + 2^len_extra[i]`; `dist_extra[i] = 0` for `i < 16`, else `(i − 16) / 4`,
`dist_base[0] = 1`, likewise. The code of a value is the last `i` with `base[i] ≤ value`.
`MAX_LENGTH = len_base[128] − 1`.

**A block's symbols.** The first byte: `alpha` of key 0. Then each token at `pos`:

1. `flag`: 0 for `Literal`, which is `alpha` of key `D[pos − 1]`.
2. `reuse`: 1 for `ReuseRecent` and `Reuse`: then `reuse_short`, 1 for `ReuseRecent` with `k`
   by `reuse_recent`, 0 for `Reuse` with `i` by `reuse_index`.
3. Else `dist_short`: 1 for `Near` with `k` by `dist_recent` (its distance `d =
   recent_dist[k]`), 0 for `Far` with `d`'s code by `dist_code` and its extra bits by a uniform
   value (§2.5). Then the length `n`: its code by `length` of key `d`'s code, its extra bits by
   a uniform value.

The history is updated after the token's symbols (§2.3).

### 2.5 The range coder

The coder keeps `low = 0` and `up = 2^32 − 1`. With `r = (up − low + 1) >> t`:

- a bit of count `p`: 1 sets `low += r × p`; 0 sets `up = low + r × p − 1`;
- a uniform value `v` of `t` bits: `up = low + r × (v + 1) − 1`, `low += r × v`.

Then it rescales (`MSB = 2^31`, `SMSB = 2^30`, values modulo 2^32): while `low` and `up` agree in
`MSB`, the encoder writes that bit, then as many opposite bits as it has put off, `low = low <<
1`, `up = up << 1 | 1`; while `low` has `SMSB` and `up` has not, it puts one bit off, `low = (low
<< 1) & (2^31 − 1)`, `up = up << 1 | 1 | MSB`. Bits are written highest first. The decoder reads
32 bits into a tag first and mirrors this (the spec's A.4), reading zeros past the end of the
code; a uniform value it reads above `2^t − 1` is `2^t − 1`.

**The end** (not in the spec): the encoder puts one more bit off, writes bit 30 of `low`, then
the bits put off (each the opposite), then zeros to a whole byte; and then drops the zero bytes
at the end of the code.

### 2.6 The x86 filter *(informative)*

With bit 0 of the flags, the data of each block is filtered, each block by itself, before it is
coded (the spec's A.12 describes it only in part). The encoder, over a block's data `D` of
length `N` (nothing is done for `N < 5`), from `i = 0` while `i < N − 4`:

- if `D[i]` is `E8` or `E9` (not `0F 8x`): let `a` be the little-endian 32-bit integer at
  `D[i+1..i+5)`. If `D[i+4]` is `00` or `FF`, then `a ← a + i` (*wrapping*, `i` counted from the
  block's start, not the stream's), and its bit 24 is extended: `a ← a | 0xFF000000` if bit 24 of
  `a` is set, else `a & 0x00FFFFFF`; `a` is written back. Either way (converted or not), `i ← i + 5`;
- otherwise `i ← i + 1`.

The decoder is the same with `a ← a − i`. That the five bytes are skipped even when the address
is not converted is what the spec's text does not say. `azo::filter` and `azo::unfilter` are this.

Known from the corpus: the decoder's direction as above (E8/E9 only, `00`/`FF`, subtracting the
position of the opcode in its block, bit 24, five bytes always). Not known, and taken from the
spec's description or by reasoning: the bounds (`N < 5`, `i < N − 4`: no stream at hand has a
candidate in a block's last four bytes) and the encoder's direction being this one's inverse.

Found from the CRC-32 of the files alone, with no code of ALZip read: `docs/azo-x86-filter-search.md`
tells how and with what evidence. The four x86 streams of the test archives (`.sys` files) are
all right with it. The module undoes it, and applies it again to rebuild (§6).

## 3. The match finder

The record's contexts come from a match finder over a block's data, holding every position
before the current one.

- **Keys**: `KEYS = [2, 3, 5, 8, 16]`. The key of `k` bytes at `p` (`p + k ≤ N`): for `k = 2`,
  `D[p] | D[p+1] << 8`; else, from `h = 0`, for each byte `b` of `D[p..p+k)`, `h = (h ^ b) ×
  0x100000001B3` (64 bits, wrapping), and the key is `(h × 0x9E3779B97F4A7C15) >> 48` (64 bits,
  wrapping).
- Each key has a chain: `head[65536]` and `prev[N]`, all `NONE` at first. Inserting `p`: for each
  key with `p + k ≤ N`, `prev[p] = head[key]` and `head[key] = p`. Positions are inserted in
  order: before a query at `pos`, every position below `pos`.
- **pairs(pos)**, given `cap`: for each key `k` in order, while `pos + k ≤ N` and `k ≤ cap`:
  `best = k − 1`; for the first `DEPTH = 32` candidates `c` of its chain from `head[key(pos)]`:
  where `D[c + best] = D[pos + best]`, `l = min(lcp(c, pos), cap)`; if `l > best`, `(pos − c, l)`
  is *found* and `best = l`; stop the chain at `best = cap`. Stop the keys once a chain reached
  `cap`. Then the found `(distance, length)` are sorted by distance, then length; going through
  them, each longer than all before it is a **pair** `(length, distance)`. The pairs go by
  increasing distance and increasing length: each is the nearest found of its length.

## 4. The record of a block

### 4.1 Coding

The record is a binary arithmetic code, as aqr-lzo's (`aqr-lzo/FORMAT.md` §5.1): carry-less, 32
bits, adaptive bits of 16-bit probabilities learnt at `RATE[c]`, raw bits of probability 1/2,
and `gamma(t)` coding `v` as `x = v + 1` (`bitlen(x) − 1` ones then a zero with the adaptive bits
of `t`, the last one for the rest, then the bits of `x` below its top one, raw).

All contexts are fresh at the start of a block:

| context | size | index |
|---|---|---|
| `is_match` | 4 × 12 × 6 × 8 × 3 | `(((prev × 12 + lb(best)) × 6 + far) × 8 + order) × 3 + lazy` |
| `is_reuse` | 4 × 4 × 8 | `(prev × 4 + rf) × 8 + order` |
| `is_recent` | 4 × 8 | `rf × 8 + order` |
| `which_recent` | 4 | `rf` |
| `rank` | 64 (gamma) | |
| `is_near` | 4 × 4 × 8 | `(prev × 4 + nf) × 8 + order` |
| `which_near` | 4 | `nf` |
| `full` | 4 | the kind: 2 for `Near`, 3 for `Far` |
| `shorter` | 64 (gamma) | |
| `pair` | 3 × 4 × 4 × 2 | `((min(j, 2) × 4 + dl) × 4 + dd) × 2 + [i = 0]` |
| `escape` | 32 + 32 (two gammas) | |

with `lb(l) = l` for `l < 8` else `min(5 + bitlen(l), 11)`; `db(d) = [d ≥ 16] + [d ≥ 256] + [d ≥
4096] + [d ≥ 65536]`; `prev` the kind of the token before (0 a literal, also at the start; 1
`ReuseRecent` or `Reuse`; 2 `Near`; 3 `Far`).

### 4.2 The walk

The record codes the tokens in a walk over `D` with its own history (§2.3), from `pos = 1` until
`pos = N`:

```text
insert the positions below pos;  cap = min(N − pos, MAX_LENGTH);  P = pairs(pos)
fits(d, n): 1 ≤ d ≤ pos, 2 ≤ n ≤ cap and lcp(pos − d, pos) ≥ n
near(k) = min(lcp(pos − recent_dist[k], pos), cap) if recent_dist[k] ≤ pos, else 0
remembered(i) = (pos − places[i] wrapping, lengths[i]);  rfit[k] = fits(remembered(recent_index[k]))
rl = the largest lengths[recent_index[k]] of the k with rfit[k], or 0
nl = max(near(0), near(1));  fl = the longest pair's length, or 0
order = [rl ≥ fl and rl > 0] + 2 [nl ≥ fl and nl > 0] + 4 [rl ≥ nl and rl > 0]
best = max(rl, nl, fl);  far = db(the longest pair's distance), or 5 without pairs
lazy: if best ≥ 2 and pos + 1 < N: insert pos; with cap' = min(N − pos − 1, MAX_LENGTH),
      nx = the larger of the longest of pairs(pos + 1) and min(lcp(pos + 1 − recent_dist[0],
      pos + 1), cap') (0 where recent_dist[0] > pos + 1); lazy = [nx > best] + [nx > best + 1]
      (the pairs of pos + 1 are those of the next position, if it is pos + 1); else lazy = 0
rf = rfit[0] + 2 rfit[1];  nf = [near(0) ≥ 2] + 2 [near(1) ≥ 2]

bit is_match: 0 a Literal (pos += 1), next
bit is_reuse: 1 a remembered match:
  bit is_recent: 1 ReuseRecent(bit which_recent)
  0: V = the i < 128 with fits(remembered(i)), in order;  r = gamma(rank)
     Reuse(V[r]) where r < |V|, else Reuse(7 raw bits)
0: bit is_near: 1 Near(k, n): k = bit which_near; n = length(2, near(k))
   0 Far: m = |P|;  j = 0; while j < m: i = m − 1 − j; bit pair[...] (below): 0 stops, 1: j += 1
      j < m: (top, d) = P[m − 1 − j];  Far(d, length(3, top))
      j = m: Far(gamma(escape[0..32]), gamma(escape[32..64]))
fails unless the token's match (§2.3, the history updated) fits;  pos += its length
```

`length(kind, f)`: bit `full[kind]`: 1 `f`; 0 `f − 1 − gamma(shorter)`, failing below 0.
For the pair bits, `dl, dd = 3, 3` for `i = 0`, else `dl = min(P[i].length − P[i−1].length − 1,
2)` and `dd = min(bitlen(P[i].distance) − bitlen(P[i−1].distance), 3)`.

The record's arithmetic code ends after the walk (aqr-lzo's end).

## 5. The record of a stream

A stream's record is:

- a byte, the flags of the stream (0 or 1);
- a varint, the block size `S`, `1 ≤ S ≤ L`;
- for each block (the data cut in pieces of `S` bytes, the last of 1 to `S`): a byte 1 for a
  stored block; or a byte 0, a varint `r` and the `r` bytes of the block's record (§4).

The stream (§2.1) is the version, the flags, each block (its header and code: a stored block's
data; a coded block's code, the range code of the tokens its record codes over its data, which
must not be as long as its data), then the end header. A decoder fails on bytes left in the
record, a byte other than 0 and 1, or a block coded as long as its data.

## 6. Formula module

The module is `azo-recons`, version 1. It rebuilds one or more streams, each from its data and
its record, and its output is the streams one after another. A container cuts the output by
the streams' lengths (a Split instruction of the formula), and adds what lies around them.

**Parameters**: none (empty).

**Inputs**: exactly three, in this order.

| slot | content |
|---|---|
| 0 | the data of every stream (as its blocks decode, with any x86 filter undone: the files), one after another |
| 1 | the records of every stream (§5), one after another |
| 2 | the index: for each stream, the length of its data and the length of its record (varints); or empty, for a single stream of all the data and the whole record |

The index must use up the data and the records exactly. Streams are rebuilt independently of
each other.

**Resources**: none.

The module fails on any parameters, any other count of inputs or resources, an index that does
not cut the inputs exactly, a record that fails above, or an output whose length is not the
application's declared output size.

## 7. Encoder choices *(informative)*

- `recons::split` parses a stream, decodes each coded block, checks that encoding its tokens
  gives its code back, and codes the tokens (§4). Streams of blocks of another size than the
  first's but the last, or of blocks over `L`, are not taken.
- ALZip's matches (the test archives at hand): every `Far` is the nearest of its length with the
  whole common prefix, 90% the longest; `Near` and remembered ones the whole prefix too. What is
  left of the record is mostly whether a match starts (a lazy or cost-based choice) and which
  kind: 1.0% of the streams in all, 5% of a PE file, 7% of digits.
- aqr-egg takes the AZO blocks of EGG archives (method 3), each a stream.
