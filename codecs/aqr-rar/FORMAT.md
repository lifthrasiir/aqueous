# aqr-rar format

aqr-rar takes RAR archives apart. It splits each compressed stream (a solid group's files, or a
file alone) into the **data** it decodes to, the files' contents, and a **record** of how its
compressor wrote it: the filters, the parse, the blocks and their tables, and for RAR 3.x the
PPMd blocks' parameters. From the data and the record the stream is rebuilt bit for bit. This
document describes the streams as far as the modules need them, the records of RAR5 streams and
of RAR 3.x streams (the "2.9" algorithm of RAR 3.x and 4.x), and the
[formula modules](#7-formula-modules) `rar5-recons` and `rar3-recons`.

The record does not reproduce any compressor. It codes the parse given the data, under contexts
of what a match finder sees in the data; how each match is written against RAR's rule; each
block's cut against RAR's buffer; each table against the lengths of a Huffman construction over
the symbols it serves; each filter and filter record against what RAR writes for it. Any stream
that decodes (§3), its filters standard and apart, has a record. RAR's own streams mostly take
the predicted choice everywhere (§8).

The document is normative and complete: an implementation of the modules can be written from it
and the references below. Sections marked *informative* describe the archives and the reference
encoder's choices; a decoder needs none of them.

External references, all in this repository or public domain:

- [`docs/rar5-decoding.md`](../../docs/rar5-decoding.md) and
  [`docs/rar3-decoding.md`](../../docs/rar3-decoding.md): clean-room descriptions of the two
  stream formats. §3 refers to them for the bit formats and states only what the modules need
  beyond them.
- aqr-lzo's [`FORMAT.md`](../aqr-lzo/FORMAT.md) §5.1: the binary arithmetic code of the records.
- lzma-sdk's `C/Ppmd7.c` and `C/Ppmd7Enc.c` (Igor Pavlov, public domain): the PPMd var.H model
  of RAR 3.x's PPMd blocks (§3.4).

aqr-rar was written from these descriptions and from the output of RAR used as a black box,
without any RAR implementation's code.

Contents:

1. [Conventions](#1-conventions)
2. [The archives](#2-the-archives-informative) *(informative)*
3. [Streams](#3-streams)
4. [The match finder](#4-the-match-finder)
5. [The RAR5 record](#5-the-rar5-record)
6. [The RAR 3.x record](#6-the-rar-3x-record)
7. [Formula modules](#7-formula-modules)
8. [Encoder choices](#8-encoder-choices-informative) *(informative)*

## 1. Conventions

- Integers are unsigned. `le32` is a little-endian 32-bit integer. Arithmetic is exact unless
  marked *mod 2^k*.
- `U` is a stream's unfiltered data (§3.1) and `N` its length; `U[a..b)` are the bytes from `a`
  to `b − 1`. Positions are positions in `U`, the stream's, unless said to be of a file.
- `bitlen(x)` is the number of bits of `x` (0 for 0).
- `lcp(a, b)`, `a < b`: the length of the common prefix of `U[a..N)` and `U[b..N)`.
- `[c]` is 1 where `c` holds, else 0.
- **varint**: unsigned LEB128, least significant group first.
- `NONE` is a position that is none.
- `lb(l) = l` for `l < 8`, else `min(5 + bitlen(l), 11)`: a length's bucket, 0 to 11.
- `db(d)`: the count of `16, 256, 1024, 4096, 16384, 2^16, 2^20` that are at most `d`: a
  distance's bucket, 0 to 7.

## 2. The archives *(informative)*

An archive is RAR5's (signature `Rar!` 1A 07 01 00, `docs/rar5-decoding.md` §1) or of RAR 1.5 to
4.x (`Rar!` 1A 07 00, `docs/rar3-decoding.md` §2). It is taken at the start of a file only. Its
headers are read in turn up to the end header, or up to the first that does not read (a header
CRC that fails, encrypted headers); the formula keeps every byte but the data areas it rebuilds.

The entries are the file headers (not service headers or subblocks, not directories) with a
data area. A stored file whose data area is as long as its unpacked size is its contents. The
compressed files make **streams**: a file without the solid flag starts one; a file with it
joins the stream before, unless a stored file came between (which ends a stream). A stream is
taken only where its first file has no solid flag and all its files are whole (not split across
volumes), not encrypted, and of known size; for RAR5, of the algorithm version 0 or 1 (the
data's: version 1 with bit 20 is version 0's) and of one algorithm and dictionary; for RAR 3.x,
of version 29, method 0x31 to 0x35 and one dictionary.

Streams are taken in turn while the total of their files' unpacked sizes stays within the budget
(64 MiB plus 32 times the archive's size). A stream taken is split (§8); it is kept as it is
where that fails or its record is not shorter than its packed data. The formula:

- a stream `rar` of the bytes kept;
- the stored files' contents and the split streams' data, as streams `raw` of the files' kinds
  by their names, so that they may be recompressed in turn;
- the records one after another (`rar/record`) and the index (`rar/index`, §7);
- one application of `rar5-recons` or `rar3-recons` rebuilding every split stream, its output
  cut into the files' data areas.

## 3. Streams

### 3.1 Common to both

A stream is the concatenation of its files' **parts** (their data areas). Decoding a part goes
on with the state the part before left (the history, the codes, the distances, the PPMd model,
the filters' programs); its bytes make the stream's unfiltered data `U`, all files' in turn. A
file's contents are its part of `U` with the file's filters applied (positions in a file count
from its first byte). The stream's **data** is the files' contents in turn.

Bits are written highest first (`docs/rar5-decoding.md` §2.1). A **code** is given by lengths 0
to 15, canonical (`docs/rar5-decoding.md` §2.3, `docs/rar3-decoding.md` §3.4); lengths whose
codes run past `2^l` (over-full) are a failure, incomplete ones are not; writing a symbol of
length 0 is a failure.

**Huffman lengths** `lengths(f)` of symbols `0 … n − 1` of counts `f`, at most 15:

```text
used = the symbols s with f[s] > 0, in order
|used| = 0: every length 0;  |used| = 1: used[0] and one more symbol (1 if used[0] = 0, else 0)
  get the length 1, the rest 0
nodes: leaf i (i < |used|) of weight f[used[i]], depth 0, id i
while more than one node is left: a, b = the two least by (weight, depth, id), a first;
  a node of weight wa + wb, depth max(da, db) + 1, id the next (|used|, |used| + 1, …),
  the parent of a and b
len[used[i]] = the depth of leaf i below the root (edges)
limit: where some length is over 15:
  cnt[l] = the count of lengths l ≥ 1, those over 15 counted at 15
  K = sum of cnt[l] × 2^(15 − l)
  while K > 2^15: l = the largest below 15 with cnt[l] > 0;
    cnt[l] −= 1, cnt[l + 1] += 1, K −= 2^(14 − l)
  the symbols with lengths, ordered by (length decreasing, f increasing, symbol increasing):
  going through l from 15 down to 1, the next cnt[l] of them get the length l
```

**Precode nibbles** (both formats' 20 length-code lengths, `docs/rar5-decoding.md` §2.3): going
through the lengths, a run of `r ≥ 3` zeros from here is the nibbles 15 and `min(r, 17) − 2`
(and `min(r, 17)` lengths are done); a zero otherwise is 0; a 15 is 15, 0; another length `l` is
`l`.

### 3.2 RAR5 streams

The format is `docs/rar5-decoding.md` §2. A stream's parts are each a sequence of blocks
(§2.2), the last with the last flag; history, codes, the distance cache and the last length `L`
carry over from one file to the next (§2.11). `v1` (the algorithm version 1, §2.10) gives 80
distance symbols and 446 code lengths, else 64 and 430.

A part's symbols are **tokens**:

| token | written as (§2.4) |
|---|---|
| `Lit(b)` | main symbol `b` |
| `Filter(f)` | main 256 and the record of §2.7: `f = (start, len, kind, channels, sb, lb)`, `sb` and `lb` the byte counts (1 to 4) of the start and length fields |
| `Last` | main 257 |
| `Rep(i, n)` | main `258 + i`, then `n` (2 to 4097) as a slot of the repeat-length code and its extra bits (§2.5) |
| `Copy(n, d)` | main `262 + slot(n − bonus(d))` and its extra bits, then `d` (§2.6): its slot by the distance code, extra bits below 4 raw, else `e − 4` raw high bits and the low 4 bits by the low-distance code |

`n` of `Copy` counts the bonus: `bonus(d) = [d > 0x100] + [d > 0x2000] + [d > 0x40000]`, and
`n − bonus(d)` is 2 to 4097. The length slot of `n` is `n − 2` for `n < 10`, else `4(e + 1) +
((n − 2) >> e) − 4` with `e = bitlen(n − 2) − 3`; the distance slot of `d` is `d − 1` for `d ≤
4`, else `2(e + 1) + ((d − 1) >> e & 1)` with `e = bitlen(d − 1) − 2`.

The **symbols** of a token, by code: `Lit(b)`: main `b`; `Filter`: main 256; `Last`: main 257;
`Rep(i, n)`: main `258 + i`, repeat-length `slot(n)`; `Copy(n, d)`: main `262 + slot(n −
bonus(d))`, distance `slot(d)`, and for slots of `e ≥ 4` extra bits, low-distance `(d − base) mod
16`.

**Writing** a part from its blocks, each `(tables or none, tokens, last)`; the codes run on
through the stream:

```text
for each block:
  bits = empty
  tables: the precode nibbles (§3.1) of the 20 lengths `pre`, then rle5(lens) (below), each
          symbol by the code of `pre` and its extra bits; the four codes from `lens` (§2.3)
  none: the codes of the block before; fails where there is none
  each token as above
  fails where bits is empty
  S = the bytes of bits, the last padded with zeros; k = 1, 2, 3 for S < 2^8, 2^16, 2^24, else fails
  B = |bits| − 8(S − 1);  flags = (B − 1) | (k − 1) << 3 | last << 6 | [tables] << 7
  the bytes: flags, 0x5A ^ flags ^ (S & 0xFF) ^ (S >> 8 & 0xFF) ^ (S >> 16 & 0xFF), S in k
  bytes (little-endian), the body
```

`rle5(lens)`, the run-length symbols (`docs/rar5-decoding.md` §3.5): going through the lengths
from `i = 0`: where `lens[i] = 0` and the run of zeros from `i` is `r ≥ 3`: `k = min(r, 138)`,
the symbol 18 with `k − 3` (3 bits) for `k ≤ 10`, else 19 with `k − 11` (7 bits), `i += k`; any
other zero is the symbol 0. A length `v > 0` is the symbol `v`, `i += 1`, then while the run of
`v` from `i` is `r ≥ 3`: `k = min(r, 138)`, 16 with `k − 3` (3 bits) for `k ≤ 10`, else 17 with
`k − 11` (7 bits), `i += k`.

The writer fails on a token it cannot write: a filter field of a value of more than its bytes, a
kind over 3, channels not 1 to 32 for kind 0 or not 0 for the others, `Rep` of `i > 3` or `n`
out of 2 to 4097, `Copy` of `d = 0` or `n − bonus(d)` out of 2 to 4097, a symbol without a code.

The filters of a file are placed by their declaration: a `Filter` token at file position `p`
(the bytes of the file's tokens before it) acts on `[p + start, p + start + len)` of the file
(§2.7, §2.8).

### 3.3 RAR 3.x streams

The format is `docs/rar3-decoding.md` §3 to §5. A part is a sequence of **blocks**, LZ or PPMd,
then the end of the file; state carries over (§3.9). A part is:

- its blocks: `Lz(table or none, tokens)` or `Ppmd(F, M, E, syms)`, `F` the 7 flag bits, `M` sent
  where `F & 0x20`, `E` sent where `F & 0x40` (0 where not sent), `syms` the symbols coded;
- `T` (the bit after the end of an LZ stream, §3.9: whether the next file starts with a header);
- its **tail**: the bytes after the end of its stream.

A `table` is `(keep, pre, lens)`: the keep bit, the 20 precode lengths, the 404 code lengths
(main 299, distance 60, low distance 17, length 28).

LZ tokens (§3.5 to §3.7):

| token | written as |
|---|---|
| `Lit(b)` | main `b` |
| `Last` | main 258 |
| `Rep(i, n)` | main `259 + i`, then `n − 2` (0 to 255) as a slot of the length code (table B) and its extra bits |
| `Short(d)` | main `263 + i`, `i` the last index of table C with base `< d`, and `d − 1 − base` in its bits; `d` 1 to 256, length 2 |
| `Copy(n, d, run)` | main `271 + slot(n − bonus(d) − 3)` and its extra bits (table A), then the distance slot `s` (the slot with `base(s) < d ≤ base(s + 1)`, §3.6) and `x = d − 1 − base(s)`: for `s ≤ 9`, `b(s)` bits of `x` (`run` must be 0); else `b(s) − 4` bits of `x >> 4`, then the low bits (below) |
| `Filter(r)` | main 257 and the bytes of the file's filter record `r` (§5.1), 8 bits each |

`bonus(d) = [d ≥ 0x2000] + [d ≥ 0x40000]`, `n − bonus(d) − 3` is 0 to 255; the length slot of `v`
is the last index of table A whose base is at most `v`. The **low bits** keep `(lo, left)`,
`(0, 0)` at each table: where `left > 0`, nothing is written (`run` must be 0 and `x mod 16 =
lo`), `left −= 1`; else where `run`, the low symbol 16 (`x mod 16` must be `lo`) and `left = 15`;
else the low symbol `x mod 16`, `lo = x mod 16`.

The **symbols** of a token, by code (the low state as when writing, from `(0, 0)` at each
table): `Lit(b)`: main `b`; `Filter`: 257; `Last`: 258; `Rep(i, n)`: main `259 + i`, length
`slot(n − 2)`; `Short(d)`: main `263 + i`; `Copy`: main `271 + slot(…)`, distance `s`, and for
`s > 9` the low symbol where one is written (16 for a run).

PPMd `syms` are the data's bytes with the escapes of §4.4 (`E` the escape byte in force): a byte
`b ≠ E` is `b`; `E, 0` ends the block; `E, 1` is the byte `E`; `E, 2` ends the file; `E, 3` and
a filter record's bytes; `E, 4, b1, b2, b3, b4` a match of distance `(b1 b2 b3, big-endian) + 2`,
length `b4 + 32`; `E, 5, b` a match of distance 1, length `b + 4`.

**Writing** a part, `H` whether it starts with a header (the part before's `T`; true for the
first):

```text
w = empty bits
for each block, the last one last:
  Lz with a table: align w (zero bits); bit 0; keep; the precode nibbles of pre (§3.1);
     if not keep: P = 404 zeros; rle3(lens, P) (below), each symbol by pre's code and its
     extra bits; P = lens; the four codes from lens; the low state (0, 0)
  Lz without a table: only as the first block where not H; fails otherwise
  Lz: its tokens; main 256; then for the last block 0 and T, else 1
  Ppmd: fails as the first block where not H; align w; bit 1; F (7 bits); M (8 bits) where
     F & 0x20, and then a new model (§3.4); fails without F & 0x20 where there is no model;
     E (8 bits) where F & 0x40.  Out: w's bytes (zero padded); w = empty.
     Out: syms by the model and the range encoder, flushed (§3.4). The last block: out the tail; done
fails where a code is missing (no table yet)
out: w's bytes (zero padded), then the tail
```

`P` (the previous lengths) runs on through the stream, 404 zeros at its start.
`rle3(lens, P)` (`docs/rar3-decoding.md` §7.2): from `i = 0`, `v = lens[i]`: where `v = 0` and
the run of zeros from `i` is `r ≥ 3`, 18 or 19 as `rle5`'s, `i += min(r, 138)`; else where `v >
0`, `i > 0`, `lens[i − 1] = v` and the run of `v` from `i` is `r ≥ 3`, 16 or 17 as `rle5`'s, `i
+= min(r, 138)`; else the symbol `(v − P[i]) mod 16`, `i += 1`.

The writer fails on a token it cannot write: `Rep` of `i > 3` or `n` out of 2 to 257, `Short`
of `d` out of 1 to 256, `Copy` of `d = 0` or `n − bonus(d) − 3` out of 0 to 255, low bits
against their run (above), a symbol without a code; and on an order below 2 (§4.1: `o = (F &
0x1F) + 1`, `o > 16` giving `16 + 3(o − 16)`) or a model that cannot be had.

### 3.4 PPMd

A new model (`F & 0x20`) is lzma-sdk's `CPpmd7` of `(M + 1) × 2^20` bytes (`Ppmd7_Alloc`)
started with the order `o` (`Ppmd7_Init`); without `F & 0x20` the model goes on as it is. Each
symbol is coded as `Ppmd7Enc.c` codes it, the inverse of `docs/rar3-decoding.md` §4.2, with
RAR's range encoder in place of 7z's, each PPMd block starting a new one:

```text
Low = 0, Range = 0xFFFFFFFF
Encode(start, size, total): Range = Range / total; Low += start × Range; Range ×= size
  (mod 2^32); then: loop: if Low ^ (Low + Range) ≥ 2^24: if Range ≥ 2^15: stop;
  Range = (2^32 − Low) & (2^15 − 1);  out Low >> 24; Range <<= 8; Low <<= 8
a binary context of probability p (scale 2^14): its symbol Encode(0, p, 2^14), else
  Encode(p, 2^14 − p, 2^14)
the frequency paths: Encode with the intervals where Ppmd7Enc.c calls its range coder
flush: Low, 4 bytes, most significant first
```

## 4. The match finder

The record's contexts come from a match finder over `U`, positions within **reach** `R` back (a
number of the record). It holds chains of five **keys**:

| chain `c` | 0 | 1 | 2 | 3 | 4 |
|---|---|---|---|---|---|
| key bytes `k_c` | 2 | 3 | 5 | 8 | 16 |
| ring bits `b_c` | 10 | 18 | 22 | 22 | 22 |

- The key of `k` bytes at `p` (`p + k ≤ N`): from `h = 0`, for each byte `x` of `U[p..p+k)`, `h
  = (h ^ x) × 0x100000001B3` (mod 2^64); the key is `(h × 0x9E3779B97F4A7C15 mod 2^64) >> 48`.
- Chain `c` has `head_c[65536]` and a ring `prev_c` of `ring_c = min(2^⌈log2 max(N, 2)⌉,
  2^b_c)` entries, all `NONE` at first. Inserting `p`: for each chain with `p + k_c ≤ N`,
  `prev_c[p mod ring_c] = head_c[key]` and `head_c[key] = p`. Positions are inserted in order;
  "insert below `q`" inserts those not inserted yet below `q`.
- The **walk** of chain `c` at `q`: from `head_c[key(q)]`, then `prev_c[p mod ring_c]` of each
  `p` walked, stopping at `NONE` or at one more than `far_c(q) = min(R, ring_c − 1, q)` before
  `q`. A position `≥ q` (`q` itself, where the lazy step of §5.5 inserted it) is walked past:
  it counts among the positions walked, and the walk goes on to its `prev_c`.
- **pairs(q, cap, ok)**: `found = []`; for each chain `c` in order, stopping where `q + k_c > N`
  or `k_c > cap`: `top = k_c − 1`; for the first `DEPTH = 32` positions `p` of its walk at `q`:
  `l = min(lcp(p, q), cap)`; where `l > top` and `ok(l, q − p)`: `top = l`, `(q − p, l)` is
  appended to `found`, and the chain stops where `l = cap`. The chains stop once one ended with
  `top = cap`. Then `found` is sorted by distance, then length; going through it, each longer
  than all before it is a **pair** `(length, distance)`. The pairs go by increasing length and
  distance. The **longest** is the last, `(0, 0)` without pairs.
- **candidates(q, n, ok)**, `n ≥ 2`: chain `c`, the last with `k_c ≤ min(n, 5)` (chain 0 for `n =
  2`, 1 for 3 and 4, 2 beyond); none where `q + k_c > N` or `q + n > N`; else the first `LIMIT =
  4096` positions `p` of its walk at `q`, those with `lcp(p, q) ≥ n` and `ok(q − p)`, in order:
  their distances `q − p`, nearest first. The **rank** of a distance `d` is its index among them
  (their count where it is not among them); `nth(r)` is the one of index `r`, none past them.

## 5. The RAR5 record

### 5.1 Coding

The record is a byte `A`, the algorithm version (0, or 1 for `v1`; other values are a failure),
then a binary arithmetic code as aqr-lzo's (`aqr-lzo/FORMAT.md` §5.1): carry-less, 32 bits,
adaptive bits of 16-bit probabilities learnt at `RATE[c]`, raw bits of probability 1/2, its end
as aqr-lzo's. Here:

- `gamma(t)` codes `v` as `x = v + 1`: `bitlen(x) − 1` ones then a zero, the `i`-th with the
  adaptive bit `t[min(i, |t| − 1)]`, then the bits of `x` below its top one, raw. Decoding fails
  at a 64th one, and where the decoder has read more than 4 bytes past the code's end after any
  one.
- `tree_n(t)` codes `v < 2^n` highest bit first, with `t[m]`, `m = 1` then `2m + bit` (`t` of
  `2^n` bits, `t[0]` unused).
- `raw_n` are `n` raw bits, highest first.
- An **overrun** check fails where the decoder has read more than 4 bytes past the code's end;
  the record makes one where marked *(overrun)*.

The contexts, all fresh at the start; `misc_i` is `misc[32i..32i+32)`:

| context | size | index |
|---|---|---|
| `is_match` | 2 × 12 × 9 × 3 × 3 | `(((prev × 12 + lb(L)) × 9 + dc) × 3 + lazy) × 3 + repc` |
| `is_rep` | 3 × 12 | `repc × 12 + lb(L)` (`repc ≥ 1` there) |
| `rep_idx` | 4 × 4 (`tree_2`) | from `4 rarg` |
| `rep_full` | 12 | `lb(m)` |
| `rep_shorter` | 12 × 32 (gamma) | from `32 lb(m)` |
| `pair` | 3 × 4 × 4 × 2 | `((min(j, 2) × 4 + dl) × 4 + dd) × 2 + [i = 0]` |
| `full` | 12 | `lb(top)` |
| `shorter` | 12 × 32 (gamma) | from `32 lb(top)` |
| `escape` | 12 × 16 (gamma) | from `16 lb(L)` |
| `rank` | 12 × 24 (gamma) | from `24 lb(n)` |
| `form` | 2 × 2 | `[pred is Rep] × 2 + seg` |
| `form_alt` | 8 (`tree_3`) | |
| `cut` | 35 | `[0]`, `[1]`: `first`; `[2..34)` a gamma; `[34]` more blocks |
| `tables` | 4 | `first × 2 + [the file is not the first]` |
| `predicted` | 2 | 0 the code lengths, 1 the precode's |
| `lens` | 16 | the length predicted (both kinds of lengths) |
| `filter` | 220 | §5.3 (`[180..212)` and `[216..220)` unused) |
| `misc` | 8 × 32 | gammas: `misc_0` far distances, `misc_1` the header, `misc_2` the count of corrections, `misc_3` their gaps; `misc_4` to `misc_7` unused |

### 5.2 Layout

`D` is the stream's data (§7). After `A`:

```text
n = 1 + gamma(misc_1);  fails if n > |D| + 1
size_0 … size_{n−1}: each gamma(misc_1);  fails unless they add up to |D|
R = gamma(misc_1)
the filters of each file (§5.3)
the unfiltered data and its corrections (§5.4)
the walk (§5.5)
the blocks (§5.6) and their tables (§5.7)
```

File `f` is `D[s_f .. s_f + size_f)` of the data and `U[s_f .. s_f + size_f)` of the unfiltered
data, `s_f` the sum of the sizes before it.

### 5.3 Filters

For each file `f` in turn, of `size = size_f`:

```text
m = gamma(filter[20..52]);  fails if m > size
pk = 4;  end = 0;  decl = 0
m times:
  kind = tree_2(filter[4 pk .. 4 pk + 4));  pk = kind
  channels = 1 + gamma(filter[52..84]) for kind 0, failing over 32; else 0
  at = end + gamma(filter[84..116]);  fails unless at < size
  len = 65536 if bit filter[212], else gamma(filter[116..148]);  fails if len ≥ 2^32
  end = at + len;  fails if end > size
  d = decl if bit filter[213], else at − gamma(filter[148..180]) (fails below 0)
  fails if d < decl or at − d ≥ 2^32;  decl = d
  start = at − d
  sb = minb(start) if bit filter[214], else 1 + raw_2
  lb = minb(len) if bit filter[215], else 1 + raw_2
  the filter (start, len, kind, channels, sb, lb), declared at the file's position d,
  acting on [at, at + len) of the file
(overrun)
```

`minb(v) = 1 + [v > 0xFF] + [v > 0xFFFF] + [v > 0xFFFFFF]`. The filters of a file are in the
order of their declarations, and of their regions, which do not overlap. (Channels over 32 fail
when the stream is written, §3.2.)

### 5.4 The unfiltered data

`U` is `D` with each file's filters undone in order, then corrected. A filter of a file acting
on `X = [at, at + len)` of it (file positions) is undone in place, `x` its bytes before:

- **delta** (kind 0), `c` channels: `k = 0`; for each channel `ch < c`, `prev = 0`; for `q = ch,
  ch + c, … < len`: `X[k] = (prev − x[q]) mod 2^8`, `prev = x[q]`, `k += 1`.
- **x86** (kinds 1, E8, and 2, E8 and E9): `i = 0`; while `i + 4 < len`: `b = x[i]`, `i += 1`;
  where `b = 0xE8`, or `b = 0xE9` for kind 2: `pos = (at + i) mod 2^24`, `v = le32 x[i..i+4)`,
  `a = (v + pos) mod 2^32`; `X[i..i+4)` becomes `le32` of `a` where `a < 2^24`; else of `a' = (v −
  2^24) mod 2^32` where `a' ≥ 2^31` and `(a' + pos) mod 2^32 < 2^31`; else `v`. Then `i += 4`.
- **ARM** (kind 3): for `i = 0, 4, …` while `i + 4 ≤ len`: where `x[i + 3] = 0xEB`, `X[i..i+3)`
  becomes the 24 bits (little-endian) of `(v + ⌊(at + i) / 4⌋) mod 2^24`, `v` those of `x[i..i+3)`.

These are the inverses of `docs/rar5-decoding.md` §2.8 where it has one. Then the
**corrections**:

```text
c = gamma(misc_2);  fails if c > N
p = 0;  c times:  p += gamma(misc_3), fails unless p < N;  U[p] ^= raw_8;  p += 1
```

### 5.5 The walk

The record codes the tokens of each file but its filters in a walk over `U`, with the match
finder (§4) of reach `R`. `cache` (four distances, all 0) and `last` (0) run on through the
stream. For each file `f` of `U[fs..fe)`:

```text
pos = fs;  prev = 0
while pos < fe:
  insert below pos;  cap = min(fe − pos, 4100)
  ok(l, d) = l ≥ 2 + bonus(d) and d not in cache
  P = pairs(pos, cap, ok);  (L, Ld) its longest
  rl[i] = min(lcp(pos − cache[i], pos), cap) where 0 < cache[i] ≤ min(pos, R), else 0
  rbest = max rl;  rarg = the first i with rl[i] = rbest
  cur = max(L, rbest)
  lazy: where cur ≥ 2 and pos + 1 < fe: insert below pos + 1;
        nx = the longest length of pairs(pos + 1, min(fe − pos − 1, 4100), ok);
        lazy = [nx > cur] + [nx > cur + 1];  else lazy = 0
  repc = 0 where rbest < 2, 1 where rbest < L, else 2;  dc = 8 without pairs, else db(Ld)
  bit is_match[…]: 0 the literal U[pos]: pos += 1, prev = 0, next
  where rbest ≥ 2 and bit is_rep[…] = 1:
    i = tree_2(rep_idx[4 rarg ..]);  m = rl[i], fails if m < 2;  d = cache[i]
    n = min(m, 4096) if bit rep_full[lb(m)], else m − gamma(rep_shorter[32 lb(m) ..])
  else:
    j = 0;  while j < |P|: bit pair[…] (below): 0 stops, 1: j += 1
    j < |P|: (top, d) = P[|P| − 1 − j];
             n = min(top, 4096) if bit full[lb(top)], else top − gamma(shorter[32 lb(top) ..])
    j = |P| (an escape): n = 2 + gamma(escape[16 lb(L) ..]), fails if n > cap
             okd(e) = n ≥ 2 + bonus(e) and e not in cache
             r = gamma(rank[24 lb(n) ..]);  d = nth(r) of candidates(pos, n, okd), or where
             none, 1 + gamma(misc_0)
  (a length below 0 fails)
  fails unless 1 ≤ d ≤ min(pos, R), 2 ≤ n ≤ cap and lcp(pos − d, pos) ≥ n
  the form (below);  pos += n;  prev = 1
  (overrun)
```

The pair bits: `i = |P| − 1 − j`; `dl, dd = 3, 3` for `i = 0`, else `dl = min(P[i].length −
P[i−1].length − 1, 2)` and `dd = min(bitlen(P[i].distance) − bitlen(P[i−1].distance), 3)`.

**The form**. `pred` is `Last` where `d = cache[0]` and `n = last`; else `Rep(i)`, `i` the
first with `cache[i] = d`; else `Copy`. For `Copy` the form is `Copy`. Else `seg = [(pos − fs)
mod 2^17 = 0 or pos mod 2^17 = 0]`, and the bit `form[…]`: 1 the form `pred`; 0 `v =
tree_3(form_alt)`: 0 `Copy`, 1 to 4 `Rep(v − 1)`, 5 `Last`, else a failure. Then:

- `Copy`: fails unless `n ≥ 2 + bonus(d)` and `n − bonus(d) ≤ 4097`; `cache = [d, cache[0],
  cache[1], cache[2]]`; `last = n`; the token `Copy(n, d)`.
- `Rep(i)`: fails unless `cache[i] = d` and `n ≤ 4097`; `cache[i]` moves to the front, the ones
  before it one on; `last = n`; the token `Rep(i, n)`.
- `Last`: fails unless `d = cache[0]` and `n = last`; the token `Last`.

The pairs computed for `pos + 1` at the lazy step are those of the step at `pos + 1` where it
comes (the positions inserted and `cache` are the same). Note §4's last paragraph: where the
lazy step was taken, an escape's candidates are none, and its distance is `1 + gamma(misc_0)`.

### 5.6 The blocks

Each file's **tokens with filters**: going through the walk's tokens of the file with the file
position `p` (a `Last` counting `last` as the walk had it), before each token its filters
declared at `p`, in order; a filter declared before `p` and not put yet fails; after the last
token those declared at the file's end; one declared past it fails.

Then for each file `f` in turn, `T` its tokens with filters:

```text
a = 0;  blocks = []
loop:
  first = [blocks is empty];  room = 16384 − first
  k = the most tokens of T from a whose units add up to at most room
      (a Filter of start bytes sb is 6 + sb units, any other token 1)
  n = k if bit cut[first], else gamma(cut[2..34])
  fails if a + n > |T|, or n = 0 while a < |T|
  t = bit tables[first × 2 + [f > 0]]
  the block (n tokens, t);  a += n
  where a = |T|: bit cut[34]: 0 stops
  fails if |blocks| > |T| + 64;  (overrun)
```

A file's last block has the last flag.

### 5.7 The tables

Going through the blocks of all files in turn, a block with tables **serves** its tokens and
those of the blocks without tables after it, up to the next block with tables (across files).
For each block with tables in turn:

```text
f = the counts of the symbols (§3.2) of the tokens it serves, for the four codes
pred = lengths(f_main) ++ lengths(f_dist) ++ lengths(f_low) ++ lengths(f_rep)   (§3.1)
lens = coded(0, pred)
pc = the counts of the symbols of rle5(lens) (§3.2; extra bits not counted)
pre = coded(1, lengths(pc))
(overrun)
```

`coded(c, pred)`: the bit `predicted[c]`: 1 `pred`; 0 each length `l` of `pred` in turn: the bit
`lens[l]`: 1 `l`, 0 `raw_4`.

### 5.8 The stream

The stream is each file's part written (§3.2) from its blocks: their tables, tokens with filters
and last flags. A decoder fails on any failure above or of the writer.

## 6. The RAR 3.x record

### 6.1 Coding

The record is a byte 3 (other values are a failure), then the arithmetic code of §5.1. The
contexts, all fresh at the start; those of RAR5's name are as §5.1's:

| context | size | index |
|---|---|---|
| `is_match` … `rank` | as §5.1 | as §5.1 |
| `form` | 3 × 2 | `[pred is Rep]` (`[2..6)` unused) |
| `form_alt` | 8 (`tree_3`) | |
| `run` | 2 | `[0]` (`[1]` unused) |
| `pp_match` | 2 × 6 | `prev × 6 + min(rc, 5)` |
| `pp_kind` | 2 | `[run ≥ 4]` |
| `pp_full` | 2 | `[0]` (`[1]` unused) |
| `cut` | 35 | `[0]`, `[1]`: first of the file; `[2]` more blocks; `[3..35)` a gamma |
| `keep` | 4 | `[first of the region] × 2 + after_ppmd` |
| `predicted` | 2 | 0 the code lengths, 1 the precode's |
| `lens` | 2 × 16 | `16c + ` the length predicted (`c` as `predicted`'s) |
| `region` | 328 | §6.5 (`[106..328)` unused) |
| `filter` | 202 | §6.3 |
| `misc` | 8 × 32 | gammas: `misc_0` to `misc_3` as §5.1; `misc_4` escape 5's lengths, `misc_5` escape 4's distances, `misc_6` its lengths; `misc_7` unused |

### 6.2 Layout

After the byte 3: the header as §5.2's (`n`, the sizes, `R`); the filters of each file (§6.3);
the unfiltered data and its corrections (§6.4); the regions of each file (§6.5); the walk
(§6.6); the blocks and their tables (§6.7).

### 6.3 Filters

The **programs** (a list, each its kind and last length) and the last index used run on through
the stream, empty and 0 at its start. `base` is the stream position of the file's start. For each
file in turn, of `size`; `g_i` is `filter[8 + 32i .. 40 + 32i)`:

```text
m = gamma(g_0);  fails if m > size + 64
end = 0;  decl = 0
m times:
  in_ppmd = bit filter[201]
  kind = tree_3(filter[0..8]);  fails if kind ≥ 5   (0 delta, 1 E8, 2 E8E9, 3 RGB, 4 audio)
  at = end + gamma(g_1);  fails if at > size
  len = gamma(g_2);  fails if len ≥ 2^32;  end = at + len;  fails if end > size
  d = decl + gamma(g_3);  fails if d > at;  decl = d
  r0 = gamma(g_4) for delta, RGB, audio, else 0;  r1 = gamma(g_5) for RGB, else 0  (each failing
  at 2^32 or more)
  pred = the record predicted (below) for (P = base + d, start = base + at, len, kind, r0, r1)
  raw = pred where there is one and bit filter[200] = 1;
        else k = gamma(g_0), fails unless 1 ≤ k ≤ 65539, and k bytes of raw_8
  parse raw at P (docs/rar3-decoding.md §5.1, §5.2), updating the programs; fails unless the
    framing's length is raw's, the body is read within its bytes, a new program's bytecode
    (1 to 65536 bytes) is one of the five standard ones by length and CRC-32 (§5.3), the
    global data is at most 0x2000 − 0x40 bytes, and it means start base + at, length len, the
    filter kind and the registers r0, r1
  the filter record raw, declared at the file's position d, in a PPMd block or not, acting on
    [at, at + len) of the file
(overrun)
```

Parsing updates the programs as the format does: `N = 0` empties the list (index 0); a new
program is appended with its kind; the index's last length becomes the length, and the index
the last one used.

**The record predicted** (RAR's, `docs/rar3-decoding.md` §7.4):

```text
idx = the first program of the kind, or the count of programs;  new = [idx = count]
G = 0;  body = empty bits
where new, idx ≠ the last index used, or no program: G |= 0x80;
    number(0 where there is no program, else idx + 1)
s = start − P (none where negative);  where s ≥ 258: G |= 0x40, s −= 258
number(s) (none where s ≥ 2^32)
where new or the program's last length ≠ len: G |= 0x20; number(len)
where (r0, r1) ≠ (0, 0): G |= 0x10; 7 bits, bit 0 [r0 ≠ 0] and bit 1 [r1 ≠ 0];
    number(r0) where r0 ≠ 0, number(r1) where r1 ≠ 0
where new: number(C), then the C bytes of the kind's program (below)
body padded with zeros to bytes, b of them
framing: G | (b − 1) for b ≤ 6;  G | 6, b − 7 for b ≤ 262;  else G | 7, b >> 8, b & 0xFF
```

`number(v)`, a VM number in its shortest form (2 bits of form, then the value): `v < 16`: 0 and
4 bits; `v < 256`: 1 and 8 bits; `v ≥ 0xFFFFFF00`: 1, 8 bits `(v >> 4) & 15`, 4 bits `v & 15`;
`v < 65536`: 2 and 16 bits; else 3 and 32 bits.

The standard programs' bytes (as RAR writes them; `docs/rar3-decoding.md` §5.3):

```text
delta  2f019a4180ec27482f09766dd3ea415b5944e8175ce16c914c4e3f7700
e8     971b012807069808000000d13a101592ec50cb9920b925f0291915530312ae511035592b6004156d4066ab023449043602523e9700
e8e9   841b0128111069808000000d13a101c689d280ac9762855cc905c92f8148c8aa981895728881aac95b0020ab6a03355811a24821b01291f4b8
rgb    c5019a4195c9a64dba4b140af49b804c0015a6a807262ac9c48b8662320f8664240666711998cc433331990066883330ccd10e980b3334400cd14666199a28cc4980b3334500cd18666199a30cc8980b3334604cd10668a520626688334628050f320c4cd14668c50041e48fc8855e027cc9268183b09dc2de9c78acd668b40e71dbb249386e022a2c412b109882490314f4e19700
audio  47019a4195e5720dc26482749324b14006d8384400a8013411dca1ba01990cc4033119a4066622604d9a400d668e60d030401826c1c8f6e62613789208e850bc5a07c6e9f520a9a0ed3733473966907019a39bcf258380c1bd30166e233493811609b050183b4dc84c059b88c528e0769390980b37118a59c480424843a947ee43346047d44a0dbbd359a486ee05094026c9342476a0306a20ea022004a041509e503fe6e128944601bd8b40f0681136c9a1923811419ca89510ee50662b00209511040262ac668c6aca2640b2671b4b26cc648a6271a2b8
```

### 6.4 The unfiltered data

As §5.4: `U` is `D` with each file's filters undone in order (each acting at or after the end
of the one before, within the file; else a failure), then the corrections of §5.4. A filter on
`X = [at, at + len)` of the file is undone in place, `x` its bytes before:

- **delta**, `c = r0` channels (1 to 128, else a failure): as §5.4's.
- **E8**, **E8E9**: `i = 0`; while `i + 5 ≤ len`: where `x[i] = 0xE8`, or `0xE9` for E8E9: `b =
  le32 x[i+1..i+5)` as signed, `c = (at + i + 1) mod 2^32`; `a = b + c` (signed, mod 2^32); the 4
  bytes become `a` where `0 ≤ a < 2^24`, else `a' = b − 2^24` where `a' < 0` and `c ≥ −a'`, else
  `b`; `i += 5`. Else `i += 1`.
- **RGB**, `S = r0`, `B = r1` (`3 ≤ len`, `3 ≤ S ≤ len`, `B ≤ 2`, else a failure): `o = x`;
  for `i = B, B + 3, …` while `i + 2 < len`: `o[i] −= o[i+1]`, `o[i+2] −= o[i+1]` (mod 2^8);
  then `k = 0`; for each channel `ch < 3`, `p = 0`; for `j = ch, ch + 3, … < len`: where `j ≥ S`,
  `p` is predicted from `up = o[j − S + 3]`, `ul = o[j − S]` and `p` as in
  `docs/rar3-decoding.md` §5.5; `X[k] = (p − o[j]) mod 2^8`, `p = o[j]`, `k += 1`.
- **audio**, `c = r0` channels (1 to 128, else a failure): for each channel `ch < c` a fresh
  predictor of §5.5; for `j = ch, ch + c, … < len`, `X[k] = (pred − x[j]) mod 2^8`, `k += 1`,
  `pred` the predictor's prediction, which is then updated as §5.5 does with `d = X[k]` and `v
  = x[j]`.

These are the inverses of `docs/rar3-decoding.md` §5.5 where they have one (where RGB's
prediction reads a byte its pass has not written, they are not; the corrections mend it).

### 6.5 Regions

Each file is cut into **regions**: an LZ region (LZ blocks one after another) or a PPMd region
(one PPMd block), each of `n` bytes of `U`. For each file of `size`, at `fs` in `U`:

```text
pos = 0;  pp = 0;  regions = []
loop:
  p = bit region[pp] where regions is empty or pp = 1, else 1   (1 a PPMd region)
  p = 1: F = raw_7;  M = raw_8 where F & 0x20, else 0
         fails where F & 0x20 and M + 1 is more than the module's limit (§7)
  n = size − pos if bit region[2 + p], else gamma(region[8 + 32p .. 40 + 32p)),
      failing if n > size − pos
  p = 1 and F & 0x40: e = esc(U[fs + pos .. fs + pos + n));
         E = e if bit region[4], else raw_8;  (else E = 0)
  the region;  pos += n;  pp = p
  where pos = size: bit region[5 + p]: 0 stops
  fails if |regions| > size + 64;  (overrun)
the end: T = bit region[7] where the last region is LZ; T = 1 where it is PPMd
the tail: expected = 00 BF 88 67 F6 A9 FF D4 where the last region is PPMd, else empty;
  bit region[72 + [last is PPMd]]: 1 expected; 0 t = gamma(region[74..106]),
  fails if t > 2^20, then t bytes of raw_8
```

`esc(x)`, RAR's escape: the byte least frequent in the first `min(|x|, 65536)` bytes of `x`, the
highest of those.

### 6.6 The walk

`rep` (four distances, 0) and `last` (a distance and a length, `(0, 0)`) run on through the
stream; a PPMd region does not change them. The walk goes through each file's regions in turn,
`pos` from the region's start `rs` to its end `re` (positions of `U`).

**An LZ region** is walked as §5.5 with these changes: `re` in place of `fe`; `cap = min(re −
pos, 260)`; `ok(l, d) = writable(l, d)` and `d` not in `rep`, and `okd(e) = writable(n, e)` and
`e` not in `rep`, with `writable(l, d) = (l = 2 and d ≤ 256) or (l ≥ 3 + bonus(d) and l −
bonus(d) ≤ 258)`; `rep` in place of `cache`; 257 in place of 4096 (`rep_full`, `full`); `prev =
0` at each region's start. The form: `pred` is `Last` where `(d, n) = last`; else `Rep(i)`, `i`
the first with `rep[i] = d`; else `Short` where `n = 2`; else `Copy`. For `Copy` and `Short` the
form is `pred`; else the bit `form[[pred is Rep]]`: 1 `pred`; 0 `v = tree_3(form_alt)`: 0
`Copy`, 1 to 4 `Rep(v − 1)`, 5 `Short`, 6 `Last`, 7 a failure. Then:

- `Copy`: fails unless `n ≥ 3 + bonus(d)` and `n − bonus(d) ≤ 258`; `run` = the bit `run[0]`
  where `d > 32`, else 0; `d` pushed onto `rep`; the token `Copy(n, d, run)`.
- `Short`: fails unless `n = 2` and `d ≤ 256`; `d` pushed onto `rep`; the token `Short(d)`.
- `Rep(i)`: fails unless `rep[i] = d` and `n ≤ 257`; `rep[i]` to the front; `Rep(i, n)`.
- `Last`: fails unless `(d, n) = last`; `Last`.
- Then `last = (d, n)`.

**A PPMd region** codes only where its matches are:

```text
prev = 0
while pos < re:
  insert below pos
  run = min(lcp(pos − 1, pos), 259, re − pos) where pos > 0, else 0
  rc = 0 where run < 4, else 1 + ⌊lb(run) / 3⌋
  bit pp_match[prev × 6 + min(rc, 5)]: 0 the literal U[pos], pos += 1, prev = 0, next
  bit pp_kind[[run ≥ 4]]:
    1 (escape 5): n = min(run, 259) if bit pp_full[0], else 4 + gamma(misc_4);
        fails unless 4 ≤ n ≤ 259 and n ≤ run;  the match (1, n)
    0 (escape 4): d = 2 + gamma(misc_5);  n = 32 + gamma(misc_6);
        fails unless n ≤ 287, d ≤ 2^24 + 1, d ≤ pos, pos + n ≤ re and lcp(pos − d, pos) ≥ n;
        the match (d, n)
  pos += n;  prev = 1
```

The match finder's positions are inserted through PPMd regions too.

### 6.7 The blocks and their tables

Each LZ region's **tokens with filters**: going through the files, and their regions with the
file position `p` (PPMd regions adding their `n`): an LZ region's tokens, before each token at
`p` its filters (not in PPMd) declared at `p` (one declared before `p` and not put yet fails);
after its last token those declared at the region's end; any left after the file's last region
fails. (`Last` counts the length of `last` as the walk had it.)

The blocks, through all files' LZ regions in turn; `H` (whether the file starts with a header)
is 1 for the first file, then the file before's `T`; `after_ppmd` is 0 at the start, 1 after a
PPMd region, 0 after each LZ block:

```text
for each LZ region, T its tokens with filters:
  a = 0;  blocks = []
  loop:
    fof = [the region is the file's first and blocks is empty]
    k = min(|T| − a, 16384 − fof)
    n = k if bit cut[fof], else gamma(cut[3..35]);  fails if a + n > |T|
    where fof and not H: no table
    else a table of keep = bit keep[[blocks is empty] × 2 + after_ppmd]
    the block (n tokens, table or none);  after_ppmd = 0;  a += n
    where a = |T|: bit cut[2]: 0 stops
    fails if |blocks| > |T| + 64;  (overrun)
```

The tables: a block with a table serves its tokens and those of the blocks without a table
right after it (in the order above, across files). `P` is 404 zeros at the start. For each
block with a table in turn:

```text
f = the counts of the symbols (§3.3) of the tokens it serves, the low state from (0, 0),
    and one main 256 for each block it serves
pred = lengths(f_main) ++ lengths(f_dist) ++ lengths(f_low) ++ lengths(f_len)
lens = coded(0, pred)
where not keep: P = 404 zeros
pc = the counts of the symbols of rle3(lens, P);  pre = coded(1, lengths(pc));  P = lens
(overrun)
```

`coded(c, pred)` as §5.7's, with `lens[16c + l]` for the bit of each length.

### 6.8 The stream

Each file's part (§3.3) is put together from its regions:

- An LZ region: its blocks, each its table (or none) and its tokens with filters (`Filter(r)`
  the file's filter record `r`, in their order).
- A PPMd region: one block `Ppmd(F, M, E, syms)`; `E` in force is the region's where `F &
  0x40`, else the one before (2 at the stream's start; it runs on through the files). `syms`:
  going through the region's tokens with the file position `p`: first, while the first filter
  in PPMd not put yet is declared at `p`, `E, 3` and its record's bytes; then a literal `b` as
  `b` (`E, 1` for `b = E`), a match `(1, n)` as `E, 5, n − 4`, a match `(d, n)` as `E, 4`, the
  three bytes of `d − 2` (big-endian), `n − 32`. After the tokens, while the first filter in
  PPMd not put yet is declared at or before the region's end, `E, 3` and its bytes. Then `E, 2`
  for the file's last region, else `E, 0`. Filters in PPMd left after the file's last region
  fail.
- `T` and the tail of §6.5.

The stream is each part written (§3.3). A decoder fails on any failure above or of the writer.

## 7. Formula modules

The modules are `rar5-recons`, version 1, and `rar3-recons`, version 1. Each rebuilds one or
more streams, each from its data and its record (§5 or §6), and its output is each stream's
files' parts one after another, the streams in turn. A container cuts the output by the parts'
lengths (a Split instruction of the formula) and adds what lies around them.

**Parameters**: none (empty) for `rar5-recons`. For `rar3-recons`, exactly one varint, at most
256: the limit of the PPMd models, in MiB (§6.5: a region with `F & 0x20` and `M + 1` over it
fails); 0 where no PPMd block starts a model.

**Inputs**: exactly three, in this order.

| slot | content |
|---|---|
| 0 | the data of every stream, one after another |
| 1 | the records of every stream, one after another |
| 2 | the index: for each stream, the length of its data and the length of its record (varints); or empty, for a single stream of all the data and the whole record |

The index must use up the data and the records exactly. Streams are rebuilt independently of
each other.

**Resources**: none.

The module fails on other parameters, any other count of inputs or resources, an index that
does not cut the inputs exactly, a record that fails (§5, §6), or an output whose length is not
the application's declared output size.

**Memory** *(informative)*: besides the inputs and the output, the work of the largest stream of
`n` bytes of data: `U` and the parts written (`2n`), the tokens (twice), the match finder (four
bytes each of `head_c` and `prev_c`, from 1.3 MiB up to about 52 MiB), 1 MiB more; for `rar3-recons`
also the parameter's MiB of PPMd model.

## 8. Encoder choices *(informative)*

- `split` (`recons5`, `recons3`) decodes a stream (`lz5`, `lz3`), takes its blocks and tokens
  apart into the record's parts, codes them, puts the stream together again from what it coded
  and checks that it is the stream; `R` is the largest distance of its explicit matches
  (`Copy`, and for RAR 3.x `Short`). A stream that does not decode or does not come out the same
  is kept.
- The walk's contexts follow RAR's parsers, which take the longest match, nearly always the
  nearest of its length, lazily; the repeated distances first where they are as long.
- The forms: RAR writes `Last` where it can, else a repeated distance, else (RAR 3.x) `Short`
  for 2 bytes, else an explicit match (`docs/rar5-decoding.md` §3.3, `docs/rar3-decoding.md`
  §7.3), except where its parser does not know the distances: RAR5's segments of 128 KiB
  (`seg`), RAR 3.x's chunks (every 30000 bytes, at 2^20); the form bits code those.
- Blocks: RAR5 cuts a block at 16384 units (16383 for a file's first), a filter record 6 units
  and its start field's bytes, at segment ends and at file ends; RAR 3.x at 16384 symbols (16383)
  before the 256 ending the block, besides at filters, PPMd switches and file ends
  (`docs/rar3-decoding.md` §7.2). Other cuts are coded by their length; blocks of a table alone
  after the last token (RAR 3.x's dummy table at a stream's end) by the more-blocks bit.
- Tables: RAR 5.x to 7.x and RAR 6.24's RAR 3.x tables are the lengths of §3.1 over the symbols
  the table serves (with the 256s ending RAR 3.x's blocks); older WinRARs' RAR 3.x tables
  differ, and are coded length by length against the prediction.
- Filters: RAR5 declares a segment's filters at its first symbol, x86 filters on 64 KiB chunks;
  RAR 3.x declares a file's filters at its start in its first LZ block, and never in PPMd. Field
  bytes and filter records are RAR's (§5.3, §6.3).
- PPMd: RAR picks the escape of §6.5, and ends a file's PPMd stream with the trailer.
- `formula` splits the streams of an archive side by side, those decoding within the budget by
  their headers' sizes, keeping a stream whose record is not shorter than its packed data; the
  parameter of `rar3-recons` is the largest `M + 1` of the PPMd blocks starting a model.
