# aqr-flate format

aqr-flate splits a raw DEFLATE stream into its decompressed **data** and a **record** of
everything else the encoder chose. From the data and the record the stream is rebuilt bit for
bit. This document describes the record, the `.flate.aqr` file that stores a record with
its data, and the [formula module](#formula-module) that rebuilds the stream from the two.

This document is normative together with the data files it pins by SHA-256 (see
[Model data](#model-data)) and the external references below. Everything else a decoder
needs is written out here: where it describes an algorithm, it describes exactly what the
reference implementation does, odd corners included. Sections marked *informative* describe
how the reference encoder makes its choices; a decoder needs none of them.

External references:

- RFC 1951 (DEFLATE): block layout, the fixed Huffman codes, the length and distance codes
  with their extra bits (`LEN_BASE`, `LEN_EXTRA`, `DIST_BASE`, `DIST_EXTRA` below are its
  tables), canonical codes from code lengths, and the transmission order of the code-length
  code lengths `CL_ORDER = 16 17 18 0 8 7 9 6 10 5 11 4 12 3 13 2 14 1 15`.
- RFC 8878 (Zstandard) for the data of a `.flate.aqr` file.
- zlib 1.3.1 (`deflate.c`, `trees.c`), zopfli, brotli, ECT and libdeflate are cited for
  orientation where an algorithm here is a port of theirs. The pseudocode in this document is
  normative, not the cited code.

## Conventions

- Integers in byte fields are little endian.
- **varint**: unsigned LEB128 (7 bits per byte, least significant group first, high bit set
  on every byte but the last), at most 10 bytes; bits beyond 64 are dropped, and a tenth byte
  with its high bit set is invalid.
- **Bit streams** are packed least significant bit first, as DEFLATE does. An `n`-bit field
  holding `v` is written least significant bit first. A bit stream ends with zero bits up to
  a byte boundary. Reading past the end of a bit stream makes the record invalid; bits left
  over at its end are ignored.
- **gamma(v)** (`v ≥ 0`) is Elias gamma of `v + 1` in a bit stream: with
  `k = floor(log2(v + 1))`, it is `k` one bits, a zero bit, then the low `k` bits of `v + 1`
  as a `k`-bit field. More than 63 leading ones are invalid. Where this document gives a
  bound for a gamma value, a larger value makes the record invalid.
- **pad(p)** is a padding value of 0 to 127: one bit, 0 for zero, else 1 followed by `p` in 7
  bits.
- `D` is the data, `N` its length, `D[a..b)` its bytes `a` to `b − 1`. `x div y` is integer
  division, `≫` a logical right shift.
- `common(a, p, m)` (`a < p`) is the largest `l ≤ m` with `D[a..a+l) = D[p..p+l)`.
- `maxlen(p) = min(N − p, 258)`, the longest match possible at `p`.

## The `.flate.aqr` file

| bytes | field |
|---|---|
| 6 | magic `69 04 5f 95 ab 77` (`aQRflat3` in base64) |
| 2 | profile (u16) |
| 4 | length of the record body (u32) |
| … | the record body |
| … | the data: the rest of the file, one or more zstd frames, decompressed and concatenated |

The profile and the body together are the record. `aqr-flate compress` writes the data
with zstd at level 19 by default.

## The record

A record is a profile (u16) followed by its body. Formats that embed aqr-flate (such as
aqr-pdf) store the record without the magic, with their own length framing.
`aqr_flate::deconstruct_with` returns a record, and `reconstruct` takes one.

### Profiles

The profile sets the rule catalog that the body indexes rules in, and how corrections are
coded.

| profile | corrections | rule catalog |
|---|---|---|
| 0 – 9 | static codes, default models `model/models.txt` | `model/rules-lλ.txt` with λ = 200, 100, 50, 30, 20, 10, 5, 2, 1, 0 |
| 10 | adaptive, with lookahead and zlib emulation; priors `model/priors-10.txt` | `model/rules-l0.txt` (λ = 0) |
| 11 – 65535 | reserved; a record with one is invalid | |

λ is the speed penalty each catalog was fitted with (bits per kilobyte of data, for every
ns/B a rule takes). Profile 0 is the fastest, and profile 9 the smallest of the static ones.
Profile 6 (λ = 5) is the library's default. Further adaptive profiles (11, 12, …) will be new
sets of priors, possibly with a different context layout.

### Model data

The catalogs, default models and priors are part of the format: a profile is defined with
exactly these files, and changing any of them requires a new profile. The library compiles
them in and uses them as they are (nothing is sorted, merged or deduplicated on load).

| file | SHA-256 |
|---|---|
| `model/rules-l200.txt` | `f9d18a19aeca24c032d00e872a0f8a665843610c923e0a7d2eb536293ad3beed` |
| `model/rules-l100.txt` | `9aeb4a0af0c6a625f701c2e57c586a187037a25efc3d5d18905f21a31deab481` |
| `model/rules-l50.txt` | `09a6346a297cce5c4c4af7f4190f7bbd3bc261300673593dba0fa9785dd95a95` |
| `model/rules-l30.txt` | `14e3c361a5d94f33f845007a962e9d4ae02cf11e70508d7f8bd5e4b6c8fa7ec6` |
| `model/rules-l20.txt` | `0474f2c96e4ef2181de8d0ae7e2885a68b2fd2f4f5af3d24a8c92e2c0a56d8c3` |
| `model/rules-l10.txt` | `708dc38245fa1db2990a9c29f35c16fd064d71861bec324cc9efacfb9748ec70` |
| `model/rules-l5.txt` | `2425b5559b2d9076a38306adf1c888929eb741b4c7bfdcd8a432618c7cedfe37` |
| `model/rules-l2.txt` | `bf2c434cca2c13ae661f8686524d63cde486870b3d27a9770a28ee2ab7304bb7` |
| `model/rules-l1.txt` | `b16b1dbb27b7e29583c5f5c7434437da40e50e19ea9a7882e73d90b6bd6b2443` |
| `model/rules-l0.txt` | `8e1deb1b3599c70789ffaba569276a203b11568c776c6b5cfdec0edf3e58e5ff` |
| `model/models.txt` | `63ef0a9db69e512f36c96fef78ef7e8c9cee5ec55f5ddedb3d29ad6a62716a01` |
| `model/priors-10.txt` | `aef58b2c2ebf6463f31d9fa2d4b4dbf63f45efa90669f54fb11483ea81cc1b01` |

They are ASCII text, in lines separated by LF.

**Rule catalogs** (`rules-l*.txt`). Each line is trimmed of surrounding whitespace, then:

- empty lines and lines starting with `#` are comments;
- lines starting with `map ` are `map FINGERPRINT INDEX…` (a 64-bit hexadecimal
  fingerprint, then one or more decimal rule indices, separated by single spaces). They tell
  the encoder which rule to choose; decoders ignore them;
- every other line is a rule. A rule's index is the index of its line among the rule lines,
  counting from 0. Each catalog holds 32 rules and no chain rule.

A rule line is a comma-separated list of items, applied left to right to the default rule
(kind search, key 3, ways 1, long 0, rank off, inner 259, lazy 0, margin 0, min_len 3,
far_len 0, far_dist 4096, window 32768, depth 0, nice 0; see [Rule](#rule)). Values are
decimal, below 65536 (key, ways, long, margin, min and farlen take their value modulo 256;
inner, lazy, far and window keep it). The resulting rule must be
valid.

| item | effect |
|---|---|
| `literals`, `runs`, `chain` | kind 1, 2 or 3 |
| `key=K`, `ways=W`, `long=L` | key length, ways, second key length |
| `rank` | rank on |
| `starts` | inner = 0 |
| `inner=N`, `lazy=N`, `margin=N`, `min=N` | inner, lazy, margin, min_len |
| `far` | far_len = max(far_len, 3) |
| `far=N` | far_len = max(far_len, 3), far_dist = N |
| `farlen=N` | far_len = N |
| `window=N` | window |
| `depth=N`, `nice=N` | chain depth and nice length: `N` must be in DEPTHS or NICES (below), whose index is stored |

**Default models** (`models.txt`). Lines whose first character is `#` and lines that are
empty after trimming are comments. The other lines, in order, are the default models of
correction fields 0 to 8: each is 72 whitespace-separated integers, the `q` (1 to 63) of
bucket symbols 0 to 71. The file has exactly nine such lines.

**Priors** (`priors-10.txt`). Lines are split at whitespace. Empty lines and lines whose
first word starts with `#` are comments. Every other line is `ctx ID P N`: context `ID`
(below 6480) starts with probability `P` (0 to 65535, a 16-bit integer) and count
`min(N, 40)` (`N` 0 to 255). A later line for the same context overrides an earlier one.
Unlisted contexts start at `P = 32768`, `N = 0`.

### Body

| field | |
|---|---|
| varint | length of the main stream |
| varint | length of the header stream |
| bytes | the main stream (a bit stream) |
| bytes | the header stream (a bit stream) |
| bytes | each segment's corrections, in order, with the lengths given in the main stream |

The body must end exactly where the last segment's corrections end.

## Main stream

The main stream is, in order: the rule, the blocks (with the final padding and the trailing
bytes), the coding parameters, the segments, then zero bits up to a byte boundary.

### Rule

One bit. If it is 1, gamma(index) (at most 65536) follows: the rule at that index in the
profile's catalog, which must exist. If it is 0, the rule follows in full: its kind in 2 bits
(0 search, 1 literals, 2 runs, 3 chain). A chain rule has only its search depth and nice
length, as indices into DEPTHS = 1, 2, 4, …, 32768 (4 bits) and
NICES = 8, 16, 24, 32, 64, 128, 192, 258 (3 bits); its other fields keep their defaults. The
other kinds continue (all of them, though only search uses most):

| field | bits | value | valid values |
|---|---|---|---|
| key | 1 | key length − 3 | 3, 4 |
| ways | 3 | ways − 1 | 1, 2, 4, 8 |
| long | 4 | key length of the second table, 0 for none | 0, 5 – 8 |
| rank | 1 | whether distances are ranked | |
| inner | gamma | positions inserted inside a match up to this length (259: every position) | 0 – 259 |
| lazy | gamma | lazy evaluation below this length (0: greedy) | 0 – 259 |
| margin | gamma | lazy margin | 0 – 255 |
| min_len | gamma | shortest match − 3 | min_len 3 – 255 |
| far_len | gamma | TOO_FAR length limit (0: none) | 0, 3 – 255 |
| far_dist | gamma | TOO_FAR distance | 0 – 65535 |
| window | gamma | farthest distance − 1 | window 1 – 32768 |

A value outside its valid values makes the record invalid. The encoder of an adaptive profile
writes a chain rule in full when it chooses one.

### Blocks

gamma(number of blocks), then for each block its type in 2 bits (0 stored, 1 fixed, 2
dynamic; 3 is invalid). The last block is the final one (BFINAL set), the others are not.

- **Stored**: pad(the value of the padding bits before its length fields), then a 2-bit
  length code: 0 for the previous stored block's length (invalid for the first stored
  block), 1 for 65535, 2 for a 16-bit length that follows; 3 is invalid.
- **Fixed or dynamic**: one bit, 1 if the block has as many tokens as the last fixed or
  dynamic block that had a count of its own (invalid if there is none). Otherwise, as long
  as no earlier block took the rest, one more bit follows: 1 means the block holds all the
  tokens not given to other blocks (*the rest*). If that bit is 0 (or was not read),
  gamma(token count) follows, at most `N`. A block that takes the rest has no count of its
  own for the next block to repeat.

Then come pad(the value of the padding after the final block), gamma(number of trailing
bytes after the stream, at most 2^30), and each trailing byte in 8 bits.

The total token count is the sum of the segments' counts (below). The rest is that total
minus the blocks' own counts; it must not be negative, and if it is positive a block must
take it. The encoder gives the rest to the last fixed or dynamic block.

A stored block comes between two tokens: before token `k`, where `k` is the number of tokens
in the blocks before it. Its bytes are the next `len` bytes of the data at that point.

### Coding parameters

**Static profiles**: one model for each of the nine correction fields (see
[Corrections](#corrections)), in field order. Each model starts with one bit. If it is 1,
the field uses its default model (its line of `model/models.txt`). If it is 0, a table
follows:

- one bit, 0 if the field has no symbols at all (no further bits);
- otherwise, the last symbol used in 7 bits (below 72), then for every symbol up to it one
  bit (used or not) and, if used, `q − 1` in 6 bits (`q` in 1 to 64). Symbols not listed are
  unused.

**Adaptive profiles**: one bit, whether zlib emulation is used. If it is 1, the parameters
follow: level (4 bits, 1 to 9), strategy (3 bits: 0 default, 1 filtered, 2 Huffman only,
3 RLE, 4 fixed), windowBits − 9 (3 bits, windowBits up to 15) and memLevel (4 bits, 1 to 9).
Other values are invalid.

### Segments

The tokens are cut into segments that are coded independently.

gamma(number of segments, at most `N + 1`), then for each segment:

- gamma(token count, at most `N`);
- static profiles only: gamma(misses of class 0) and gamma(misses of class 1), each at most
  the token count;
- for every segment but the first: gamma(position, at most `N`), the data position where the
  segment's first token would start were the stored blocks that come right before that token
  left out: the end of the previous token. The first segment's position is 0. Positions must
  not decrease.
- gamma(length of its corrections in bytes, at most the record's length: 2 + the body's);
- adaptive profiles with zlib emulation, for every segment but the first: one bit, whether
  the segment resumes the emulation, then if so its checkpoint (see
  [zlib emulation](#zlib-emulation)).

The main stream then pads to a byte boundary.

A segment's **end** `e` is the next segment's position, or `N` for the last segment. Its
tokens are the next token-count tokens of the stream, its *first token* the index of the
first of them.

## Header stream

The header stream holds, for each dynamic block in order, its Huffman header, coded against
the headers that known encoders would build for the block's tokens. Decoding a header needs
the block's symbol frequencies, so headers are read after the tokens:

- `lf[0..288)`: `lf[256] = 1` (the end of block); each literal `b` counts in `lf[b]`; each
  match counts in `lf[257 + c]`, with `c` its length code (RFC 1951), or `c = 27` for length
  258 written as symbol 284 with extra bits 31;
- `df[0..32)`: each match counts in `df[c]`, with `c` its distance code.

A *recipe* carries over from one dynamic block to the next. It holds a candidate index for
the code lengths, the run-length coding and the code-length code, all 0 at the start
(zlib's).

The **natural cut** of candidate code lengths `ll[0..288)`, `dl[0..32)` is
HLIT = max(257, 1 + the last index with `ll` nonzero) and HDIST = max(1, 1 + the last index
with `dl` nonzero) (with no nonzero entry, the last index counts as −1). Cutting them to
HLIT and HDIST means taking `ll[0..HLIT)` then `dl[0..HDIST)`, a list `lens` of
HLIT + HDIST code lengths. The **natural HCLEN** of code-length code lengths `cl[0..19)` is
max(4, 1 + the last `i` with `cl[CL_ORDER[i]]` nonzero).

1. One bit. If it is 1, the header is exactly what the recipe builds: the recipe's candidate
   code lengths at their natural cut, its run-length coding of them, its code-length code for
   those ops, and HCLEN at its natural value (the code-length code lengths whose position in
   `CL_ORDER` is HCLEN or beyond are zero). Nothing else follows, and the recipe is unchanged.
2. Otherwise:
   - gamma(k) (at most 64; `k` from 23 to 64 is invalid, there being 23 candidates) chooses
     candidate code lengths `k`, which become the recipe's. Then HLIT − 257, then HDIST − 1,
     each as one bit 1 if equal to the candidate's natural cut, else 0 and the value in 5 bits
     (HLIT up to 288, HDIST up to 32). `lens` is the candidate cut to them. Then gamma(number of lengths that differ from
     the candidate's, at most |lens|), and for each in order gamma(`i − j`) (at most |lens|)
     and the length in 4 bits, which replaces `lens[i]`: `j` is 0 for the first and the
     previous difference's index plus one after it, and `i` must be within `lens`. Then
     `lens[256]` must be nonzero, and neither `lens[0..HLIT)` nor `lens[HLIT..)` may be
     oversubscribed (Σ 2^(15 − l) over nonzero `l` at most 2^15; incomplete codes are
     allowed).
   - The run-length coding: one bit 1 and gamma(k) (at most 64; 23 to 64 invalid) for
     candidate coding `k` of `lens` (now the recipe's), or one bit 0 and the coding written
     out (the recipe's coding is unchanged). Written out, it is gamma(number of ops, at most
     |lens|), then per op 2 bits: 0 for a literal length (the symbol is `lens[i]`, no more
     bits), 1 for symbol 16, 2 for 17, 3 for 18, with `i` the number of lengths the ops so far
     cover (`i` must be within `lens`). For 16, 17 and 18, one bit follows, 1 if the run is
     as long as it can be, else the symbol's extra bits (2, 3 or 7: a run of 3, 3 or 11 plus
     their value). The longest run is the number of consecutive lengths from `lens[i]` on
     equal to the repeated value (`lens[i − 1]` for 16; zero for 17 and 18), capped at 6, 10
     or 138; a 16 at `i = 0` has no value to repeat and is invalid. A run must be at least 3, 3 or 11 and at most that longest run,
     and the ops must cover `lens` exactly.
   - The code-length code, in 2 bits: 0 for zlib's, 1 for package-merge (either becomes the
     recipe's), 2 for explicit (the recipe's is unchanged); 3 is invalid. HCLEN − 4 follows
     as one bit 1 if HCLEN is the natural HCLEN of the chosen code (of code 0 when
     explicit), else 0 and 4 bits. Then the lengths of `CL_ORDER[0..HCLEN)` are the chosen
     code's (explicit: 3 bits each, in that order), and the others zero. The code-length
     code must not be oversubscribed, and every symbol of the ops must have a nonzero
     length.

The candidate code lengths, run-length codings and code-length codes are given by
[Header candidates](#header-candidates).

Explicit HLIT up to 288 and HDIST up to 32, and incomplete codes made by the differences, are
accepted on purpose: they reproduce streams that RFC 1951 or zlib's inflate may reject (zlib
refuses HLIT above 286 and HDIST above 30), as long as the reference parser read them.

## Tokens and their prediction

A segment's tokens are predicted one by one, from the data alone. A token is either a hit
(exactly the prediction) or a miss, for which a *correction* is coded. Tokens are literals
or matches (length 3 to 258, distance 1 to 32768); a match of length 258 has a flag
**alt258**, set when it is written as symbol 284 with extra bits 31 rather than as symbol
285. Predictions never set it.

**Positions.** A segment starts at its position `p = s`. Before its token `k` (global index),
every stored block that comes before token `k` is skipped over: `p` grows by its length (and
zlib emulation runs over it, see below). Then `p` must be below `min(N, e)`; the token is
predicted and decided at `p`, given to the rule's table and to the emulation, and `p` grows
by its length (1 for a literal). After its last token, `p` must equal `e`, except for the
last segment, where `p` plus the lengths of the stored blocks after the last token of the
stream must equal `N`.

### The rule

Every rule predicts, at position `p`, a token and a **class**: 0 when no candidate was found
(a literal is predicted), 1 when one was (whether a match or a literal is predicted). Class 2
is zlib emulation's (adaptive profiles only).

**Literals** predict `D[p]`, class 0.

**Runs** predict, with `l = common(p − 1, p, maxlen(p))`: if `p = 0`, `maxlen(p) < 3` or
`l < 3`, a literal of class 0; if `l < min_len`, a literal of class 1; else a match of length
`l` and distance 1, class 1.

Literals and runs have no table: they offer no bucket entries and no ranks.

#### Search rules

A search rule keeps a table of recent positions per segment, rebuilt from scratch at the
segment's start.

**Size.** With `span = max(1, e − max(0, s − 32768))`, the hash width is
`bits = min(15, max(10, ceil(log2 span)))` (`ceil(log2 1) = 0`).

**Hashes.** `hash32(k) = ((k · 0x9E3779B1) mod 2^32) ≫ (32 − bits)`.

- The first table is keyed by the first K = key bytes of a position `q` (defined for
  `q + K ≤ N`): `H1(q) = hash32(D[q] + 2^8 D[q+1] + 2^16 D[q+2] (+ 2^24 D[q+3] if K = 4))`.
- The second table (long = L > 0) is keyed by L bytes (defined for `q + 8 ≤ N`):
  `H2(q) = ((Σ_{i<L} D[q+i] · 2^(8i + 64 − 8L)) · 0x9E3779B97F4A7C15 mod 2^64) ≫ (64 − bits)`.
- The rank chains use `H3(q) = hash32(D[q] + 2^8 D[q+1] + 2^16 D[q+2])` (equal to `H1` when
  K = 3).

**Buckets.** The first table has 2^bits buckets of W = ways entries, the second 2^bits
buckets of one entry; all start empty. **Inserting** `q` (only if `q + K ≤ N`, else nothing
happens): `q` goes to the front of bucket `H1(q)` (the others move back one, the last one
drops out); and if L > 0 and `q + 8 ≤ N`, `q` replaces the entry of bucket `H2(q)`. So a
bucket lists positions most recent first.

**Which positions are inserted:**

- inner = 259 (*every position*): whenever the table is used at a position `x` (a search
  at `x`, the entries of `x` or a rank walk at `x`), every position `q` with
  `max(0, s − 32768) ≤ q < x` not inserted yet is inserted first, in increasing order. This
  takes in the window before the segment's start and the bytes of stored blocks. The
  lookahead of `cmp` (below) inserts nothing.
- inner < 259 (*token-driven*): the table starts empty. A token's start `p` is inserted
  once: by the lazy step at `p` if it runs, else after the token is decided. After a match of
  length `l` at `p` with `l ≤ inner`, `p + 1, …, p + l − 1` follow in order. Nothing else is
  inserted (inner 0 and 1 insert only token starts).

**Search** at `x` within distance `ω`: none if `maxlen(x) < 3` or `x + K > N`. The
candidates are the entries of bucket `H1(x)`, and if L > 0 and `x + 8 ≤ N` the entry of
bucket `H2(x)`. A candidate `c` counts if `c < x`, `x − c ≤ ω` and `D[c..c+K) = D[x..x+K)`;
its length is `common(c, x, maxlen(x))`. The result is the longest counting candidate, the
closest (smallest `x − c`) among equals, or none.

**Prediction** at `p` with `ω` = window:

1. Search at `p`. None: a literal of class 0. Else `(len, dist)`, class 1 from here on.
2. If `len < min_len`, or `len ≤ far_len` and `dist > far_dist`: a literal.
3. If `len < lazy` and `p + 1 < N`: the lazy step. Token-driven tables insert `p` (if not
   inserted yet); then search at `p + 1` (an every-position table inserts `p` for it). If
   that finds a length above `len + margin`: a literal.
4. Otherwise the match `(len, dist)`.

**Entries** of `x` (the *entry* field): none if `x + K > N`. Else the entries `c < x` of
bucket `H1(x)` in bucket order, then, if L > 0 and `x + 8 ≤ N`, the entry `c < x` of bucket
`H2(x)` unless already listed. They are taken as the table is when the field is coded, with
no check of distance or content. (After a lazy step at `x`, `x` itself is in the bucket: it
is not listed, but it has pushed the oldest entry out.)

**Rank chain** of `x` (rank on): none if `x + K > N`. Else the positions `q` with
`max(0, s − 32768) ≤ q < x` and `H3(q) = H3(x)`, in decreasing order, up to (not including)
the first with `x − q > 32768`. If K = 3 and inner = 259, the first W of them are skipped
(they are the bucket's entries). At most 1024 positions after those are *visited*.

- The **rank** of a distance `dist` for a match of length `len` at `x`: walk the visited
  positions with a count `r = 0`. At `q` with `x − q ≥ dist`, stop: the rank is `r` if
  `x − q = dist`, else there is none. Otherwise `r` grows by one if
  `common(q, x, len) = len`. If the walk ends first, there is none.
- **Selecting** rank `k`: the `k`-th (from 0) visited position `q` with
  `common(q, x, len) = len` gives the distance `x − q`; if there is none, the record is
  invalid.

#### Chain rules

A **chain** rule (depth `DEPTHS[depth]`, nice `NICES[nice]`) uses exact chains: the chain of
`x` (for `x + 3 ≤ N`; otherwise empty) lists the positions `q < x` with
`D[q..q+3) = D[x..x+3)`, in decreasing order, up to (not including) the first with
`x − q > 32768`. It covers the whole data, stored blocks and earlier segments included.

**Prediction** at `p`: if `maxlen(p) ≥ 3`, walk the first `depth` positions `q` of the chain
of `p`, with `best = 0`: if `l = common(q, p, maxlen(p))` exceeds `best`, `best = l` and
`dist = p − q`, and the walk stops if `l ≥ nice` or `l = maxlen(p)`. If `best ≥ 3` the match
`(best, dist)` is predicted with class 1, else a literal with class 0. There is no window,
min_len, TOO_FAR or lazy evaluation.

A chain rule offers no bucket entries (the entry field is always 0) and always ranks. Its
**rank** of `dist` for length `len` at `x` walks the first 8192 positions of the chain of
`x`: at `x − q = dist` it is the number of earlier walked positions with
`common(q, x, len) = len`; if `dist` is not among them, there is none. **Selecting** rank `k`
gives the `k`-th (from 0) of those 8192 positions with `common(q, x, len) = len`.

### Corrections

A miss is coded as these fields, in this order. The kind and the lengths depend on the
prediction (`pl` its length, 0 for a literal); the distance fields are relative to the rule's
table at `p`, also for zlib emulation's predictions.

1. **kind** (field 2 for class 0, field 3 for classes 1 and 2): 0 for a literal where a match
   was predicted; 1 for a fresh match where a literal was predicted; 2 for a match whose
   length differs from the predicted match (or not: a different distance alone is kind 2
   with delta 0); 3 for length 258 written as symbol 284 (alt258; any prediction), in which
   case no length field follows. Kind 0 or 2 with a literal predicted, or 1 with a match
   predicted, is invalid. Kind 0 gives the literal `D[p]` and nothing follows.
2. The length: for kind 1, **fresh length** (field 4), the length − 3. For kind 2, **length
   delta** (field 5), the zigzag of the actual minus the predicted length (0, −1, 1, −2, …
   map to 0, 1, 2, 3, …). The length must be 3 to `maxlen(p)`.
3. For a match, the distance:
   - **entry** (field 6): 1 + its index among the entries of `p`, or 0. An index beyond them
     is invalid.
   - If 0 and the rule ranks (a search rule with rank on, or a chain rule), **rank**
     (field 8): 1 + its rank, or 0. A rank that cannot be selected is invalid.
   - If still 0, **raw distance** (field 7): the distance − 1.

   The distance must be 1 to `min(p, 32768)`, and `D[p − dist..)` must match `D[p..)` for the
   whole length. The encoder codes a distance by its entry if it has one, else by its rank if
   it has one, else raw.

Fields 0 and 1 are the gaps of the static coding (below).

Field values are coded as **bucket symbols** (72 of them) with extra bits. A value `v < 16`
is symbol `v` with no extra bits. Otherwise, let `n` be the bit length of `v` (5 to 32): the
symbol is `16 + 2(n − 5) + b`, where `b` is the bit below the leading one, and the extra bits
are the low `n − 2` bits of `v`. Symbol `s ≥ 16` thus has `(s − 16) div 2 + 3` extra bits.

### Static coding (profiles 0 – 9)

Each segment's corrections are a sequence of field values, coded with the field's model. The
first gap of each class comes first (class 0 then class 1, only for classes with misses).
Then, for each miss in token order, its fields follow, then the gap to the next miss of the
same class (if there is one). A gap is the number of hits of that class before the miss
(since the previous miss of the class, or the segment's start). Once a class has no misses
left, its tokens are hits. Every miss the segment's header announces must be read by its
end.

**Models.** A model is the `q` (1 to 64, or 0 for unused) of each of the 72 symbols. Weights
are `w(q) = (q odd ? 181 : 128) · 2^(q div 2)`, taken at `q − 1`. With `S` the sum of the
used symbols' weights, a used symbol's frequency is `max(1, floor(w(q − 1) · 2^15 / S))`, an
unused one's 0. If `S > 0`, 2^15 minus the sum of the frequencies is added to the most
frequent symbol (the lowest of equals); it may be negative (the excess is at most 71, from
the frequencies raised to 1, while the most frequent is at least 454), so every used symbol
keeps a frequency of at least 1. `cum[s]` is the sum of the
frequencies of the symbols below `s`. A field whose frequencies do not sum to 2^15 (one with
no symbols) cannot be read: reading it makes the record invalid.

**Layout.** The corrections of a segment are a varint length `m` of the rANS bytes, the `m`
rANS bytes, then the extra bits as a bit stream (to the end of the corrections). The encoder
writes nothing at all for a segment without fields. With no bytes at all, or `m = 0`, the
state is 0 (so no symbol can be read).

**rANS** has a 32-bit state, L = 2^23 and 8-bit renormalization (after ryg's `rans_byte`).
The decoder loads the state from the first 4 rANS bytes, big endian (`m` must be 0 or at
least 4), and reads the following rANS bytes in order. Then, per symbol:

- the state must be at least 2^23;
- `slot = x mod 2^15`, giving symbol `s` with `cum[s] ≤ slot < cum[s+1]`;
- `x = freq[s] · (x ≫ 15) + slot − cum[s]`;
- while `x < 2^23`, `x = 2^8 x + next rANS byte` (running out of rANS bytes is invalid);
- the symbol's extra bits are read from the bit stream (as one field, least significant bit
  first).

Bytes and bits left over at the end are ignored.

### Adaptive coding (profiles 10 and up)

Each segment's corrections are one binary arithmetic coded stream. Every token codes a hit
flag, and a miss codes its fields down binary trees. Every bit has a context, with an
adaptive probability that starts from the profile's priors.

**Arithmetic coder** (carry-less, after lpaq). The state is two 32-bit bounds `x1 = 0` and
`x2 = 2^32 − 1`. A bit with probability `p` of being 1 (in units of 2^−16) splits the range
at `xm = x1 + ((x2 − x1) · p ≫ 16)`: a 1 keeps `[x1, xm]`, a 0 keeps `[xm + 1, x2]`. While
the top bytes of `x1` and `x2` agree, that byte is output and both shift left by 8 (`x2`
filling with ones). The stream ends with the single byte `(x1 ≫ 24) + 1`, or is empty if no
bit was coded. The decoder starts from its first 4 bytes, big endian, takes a 1 when its
value is at most `xm`, shifts in one byte per output byte, and reads zeros past the end. A
decoder that has read more than 4 bytes past the end (at any point of the segment) makes the
record invalid.

**Probabilities.** Context `c` has a probability `p` (16 bits) and a count `n`, both from the
priors. A bit is coded at `p` clamped to [16, 65520]. Then `p += ((t − p) · r(n)) ≫ 16`,
with `t` = 65535 for a 1 and 0 for a 0, the shift arithmetic (rounding toward −∞), and
`r(n) = floor(131072 / (2n + 3))`. Then `n` grows by one, up to 40. Extra bits are coded at
`p = 32768` with no context and no update.

**Per token**, at position `p` with prediction `pred` (length `pl`, 0 for a literal) of class
`c`:

- `lb(l)` is the length bucket: 0 for `l < 3`, then 1 for 3, 2 for 4, 3 for 5, 4 for 6–8,
  5 for 9–16, 6 for 17–32, 7 for 33–128, 8 for 129–257 and 9 for 258.
- `cmp` is 0, except when the rule predicts a match (class 1). Then the longest match at
  `p + 1` is found (length 0 if `p + 1 ≥ min(N, e)`): for a search rule, a search at `p + 1`
  within its window on the table as it is at that point, inserting nothing (after a lazy step
  at `p` it holds `p`, otherwise not); for a chain rule, its prediction at `p + 1`; for runs,
  0. With that length `m`, `cmp` is 1 if `m = pl + 1`, and 2 if `m > pl + 1`.
- `h[c]` is the class's last two outcomes (bit 0 the latest, 1 for a miss), 0 at the start
  of a segment. Every segment also starts every context afresh from the priors (`p` and
  `n`), and its own arithmetic decoder.
- The flag (1 for a miss) has context `((c · 10 + lb(pl)) · 3 + cmp) · 4 + h[c]`.
  Afterwards `h[c] = (h[c] · 2 + flag) mod 4`.

**A miss** codes the fields of [Corrections](#corrections). The payload is that of class 1
for class 2 (the kind is field 3), but the contexts below take `c` as it is. Each field is
coded down a tree: a value's bits go most significant first, and node `j` (starting at 1,
then `2j` + bit) has context `base + j`.

| field | tree | base |
|---|---|---|
| kind | 2 bits (the kind itself) | `360 + ((c · 10 + lb(pl)) · 3 + cmp) · 4` |
| fresh length | 7 bits (bucket symbol) | `720 + c · 128` |
| length delta | 7 bits | `1104 + (lb(pl) · 3 + cmp) · 128` |
| entry | 7 bits | `4944 + lb(actual length) · 128` |
| rank | 7 bits | `6224` |
| raw distance | 7 bits | `6352` |

A symbol of 72 or above is invalid. The bucket's extra bits follow the symbol, most
significant first. The actual length used for the entry's context is 258 for kind 3. There
are 6480 contexts in all.

### zlib emulation

With zlib emulation (adaptive profiles), the emulator below runs alongside the tokens. It is
a port of zlib 1.3.1's `deflate_slow`, `deflate_fast`, `deflate_rle`, `deflate_huff`,
`longest_match` and `fill_window` (`deflate.c`), as for a single `deflate(Z_FINISH)` call
with all the data as input, made to follow the actual tokens. Its input is the whole data
`D`.

**Driving it.** In a segment with an emulator:

- Before each token, after the skips of that token's stored blocks (each runs `skip_to(end
  of the stored block)`), `predict()` gives the emulator's next token and its position. If
  there is one and it is at `p`, it is the prediction, with class 2: the rule is not asked
  (no search, no lazy step and no insertion happen for it, and `cmp` is 0). Otherwise the
  emulation ends for the rest of the segment and the rule predicts.
- After the token is decided, `commit(actual token)`; if that fails, the emulation ends for
  the rest of the segment. The rule's table takes every token as usual.
- If `skip_to` fails, the emulation ends for the rest of the segment.

The first segment starts with a fresh emulator. Any other segment starts with the emulator
rebuilt from its checkpoint, if it has one (a checkpoint that cannot be rebuilt makes the
record invalid); otherwise it has no emulation.

**Parameters.** `w = 2^windowBits`, `MAX_DIST = w − 262`; hashes are `memLevel + 7` bits
wide (`hmask = 2^(memLevel+7) − 1`), with `shift = ceil((memLevel + 7) / 3)`. zlib's
configuration table gives (good, lazy, nice, chain) per level:

| level | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 |
|---|---|---|---|---|---|---|---|---|---|
| good | 4 | 4 | 4 | 4 | 8 | 8 | 8 | 32 | 32 |
| lazy | 4 | 5 | 6 | 4 | 16 | 16 | 32 | 128 | 258 |
| nice | 8 | 16 | 32 | 16 | 32 | 128 | 128 | 258 | 258 |
| chain | 4 | 8 | 32 | 16 | 32 | 128 | 256 | 1024 | 4096 |

The algorithm is *huff* for strategy Huffman only, *rle* for RLE, else *fast* for levels 1
to 3 and *slow* for 4 to 9. Strategy filtered sets `filtered` (used by slow only); fixed
emulates as default.

**State.** `window[0..2w)`, all 0; `head[0..2^(memLevel+7))` and `prev[0..w)`, all 0 (window
positions; 0 is none); `base = 0` (the data position of `window[0]`); `read = 0`;
`strstart = 0`, `lookahead = 0`, `high_water = 0`; `match_length = 2`, `match_start = 0`,
`prev_length = 2`, `prev_match = 0` (`match_start` and `prev_match` are signed: a slide can
take them below 0); `match_available = false`; `last_head = 0`; no pending token; not done.

```
hash(s)   = ((window[s] << 2·shift) ^ (window[s+1] << shift) ^ window[s+2]) & hmask
insert(s) = h = hash(s); old = head[h]; prev[s mod w] = old; head[h] = s; return old

fill_window():
  repeat
    more = 2w − lookahead − strstart
    if strstart ≥ 2w − 262:                       # slide
      window[0 .. w − more) = window[w .. 2w − more)
      match_start −= w; strstart −= w; base += w
      every entry v of head and prev becomes (v ≥ w ? v − w : 0)
      more += w
    if read = N: leave the loop
    n = min(N − read, more)
    window[strstart + lookahead ..+ n) = D[read ..+ n); read += n; lookahead += n
  until lookahead ≥ 262 or read = N
  if high_water < 2w:                             # zero what may be read past the input
    curr = strstart + lookahead
    if high_water < curr:
      k = min(2w − curr, 258); window[curr ..+ k) = 0; high_water = curr + k
    else if high_water < curr + 258:
      k = min(curr + 258 − high_water, 2w − high_water)
      window[high_water ..+ k) = 0; high_water += k

longest_match(cur):
  chain' = chain; best = prev_length; nice' = min(nice, lookahead)
  limit = strstart > MAX_DIST ? strstart − MAX_DIST : 0
  if prev_length ≥ good: chain' = chain' div 4
  loop
    if window[cur+best] = window[strstart+best] and window[cur+best−1] = window[strstart+best−1]
       and window[cur] = window[strstart] and window[cur+1] = window[strstart+1]:
      l = 3                                       # byte 2 is not compared
      while l < 258 and window[cur+l] = window[strstart+l]: l += 1
      if l > best:
        match_start = cur; best = l
        if l ≥ nice': leave the loop
    cur = prev[cur mod w]
    if cur ≤ limit: leave the loop
    chain' −= 1; if chain' = 0: leave the loop
  return min(best, lookahead)

slow_search(h):
  match_length = longest_match(h)
  if match_length ≤ 5 and (filtered or (match_length = 3 and strstart − match_start > 4096)):
    match_length = 2
```

The steps that produce the next token, each returning `(kind, window position, token)` or
nothing. A match's distance is computed modulo 2^16.

```
step_slow():
  loop
    if lookahead < 262:
      fill_window()
      if lookahead = 0:
        if match_available: return (END, strstart−1, literal window[strstart−1])
        return nothing
    hash_head = lookahead ≥ 3 ? insert(strstart) : 0
    last_head = hash_head
    prev_length = match_length; prev_match = match_start; match_length = 2
    if hash_head ≠ 0 and prev_length < lazy and strstart − hash_head ≤ MAX_DIST:
      slow_search(hash_head)
    if prev_length ≥ 3 and match_length ≤ prev_length:
      return (PENDING, strstart−1, match (prev_length, strstart − 1 − prev_match))
    if match_available: return (PENDING, strstart−1, literal window[strstart−1])
    match_available = true; strstart += 1; lookahead −= 1

step_fast():
  if lookahead < 262: fill_window(); if lookahead = 0: return nothing
  hash_head = lookahead ≥ 3 ? insert(strstart) : 0
  if hash_head ≠ 0 and strstart − hash_head ≤ MAX_DIST: match_length = longest_match(hash_head)
  if match_length ≥ 3: return (AT, strstart, match (match_length, strstart − match_start))
  return (AT, strstart, literal window[strstart])

step_rle():
  if lookahead ≤ 258: fill_window(); if lookahead = 0: return nothing
  match_length = 0; s = strstart
  if lookahead ≥ 3 and s > 0 and window[s] = window[s+1] = window[s+2] = window[s−1]:
    l = 3; while l < 258 and window[s+l] = window[s−1]: l += 1
    match_length = min(l, lookahead)
  if match_length ≥ 3: return (AT, s, match (match_length, 1))
  return (AT, s, literal window[s])

step_huff():
  if lookahead = 0: fill_window(); if lookahead = 0: return nothing
  return (AT, strstart, literal window[strstart])
```

`predict()`: if there is no pending token and not done, run the algorithm's step; its
result becomes pending, or nothing sets done. Returns the pending token, at data position
`base + its window position`, or nothing. It is idempotent until `commit`.

`commit(a)`, with `a` the actual token (`alen` its length, 0 for a literal) and
`hit = (a = pending token)`; fails without a pending token. The pending token is cleared,
then by algorithm and kind:

```
slow, END:       fail unless hit; match_available = false
slow, PENDING:   if a is a match:
                   fail if alen − 1 > lookahead
                   max_insert = strstart + lookahead − 3; lookahead −= alen − 1
                   repeat alen − 2 times: strstart += 1; if strstart ≤ max_insert: insert(strstart)
                   match_available = false; match_length = 2; strstart += 1
                 else:
                   if not hit:                    # zlib wanted a match: search afresh
                     prev_length = 2; match_length = 2
                     if last_head ≠ 0 and strstart − last_head ≤ MAX_DIST: slow_search(last_head)
                   strstart += 1; lookahead −= 1
fast:            if a is a match:
                   fail if alen > lookahead
                   lookahead −= alen
                   if alen ≤ lazy and lookahead ≥ 3:
                     repeat alen − 1 times: strstart += 1; insert(strstart)
                     strstart += 1
                   else: strstart += alen
                   match_length = 0
                 else:
                   lookahead −= 1; strstart += 1; if not hit: match_length = 0
rle, huff:       n = max(alen, 1); fail if n > lookahead; lookahead −= n; strstart += n
                 match_length = 0
```

`skip_to(end)` lets zlib run on its own over a stored block: while `predict()` gives a
token at `q < end`, it commits that token, or the literal `D[q]` instead if the token would
end past `end`; it fails if a commit fails. A token at `end` or beyond stays pending.

These are the deliberate deviations from zlib: it is driven by the actual tokens (the
`not hit` paths above, and the actual token's length deciding the insertions); it assumes
the whole input at once, so zlib's deferred insertion (`s->insert`) and partial-input returns
never happen; and its hash is computed from three bytes directly, which equals zlib's rolling
hash after three updates. Block flushes do not affect zlib's matching and are not emulated.

**Checkpoints.** A checkpoint is the emulator's state at the segment's first token, coded
relative to the segment's position `s` and `w`:

| field | | value |
|---|---|---|
| match_available | 1 bit | slow's pending literal; `strstart = s + match_available` |
| base | gamma, at most `N` | `strstart div w − base div w` (base: the window's start, a multiple of `w`) |
| read | gamma, at most `2w` | `base + 2w − read` (read: the input read into the window) |
| high_water | gamma, at most `2w` | `2w − high_water` |
| match_length | gamma, at most 258 | `match_length − 2` |
| match_start | gamma, at most `w` | `strstart − match_start`, only if `match_length > 2`; else `match_start = 0` |

Here `strstart`, `base`, `read`, `high_water` (a window index) and `match_start` are data
positions where applicable. For rle and huff the encoder writes `match_length = 2` (field 0)
and no match_start; the rebuilt emulator holds the decoded value, which has no effect (rle
resets `match_length` before every use, huff never reads it). Negative results are invalid. The checkpoint is invalid unless:
the algorithm is not fast (levels 1 to 3 with strategies default, filtered and fixed cannot
be resumed, as their hash chains depend on the tokens); `match_available` is false unless the
algorithm is slow, and if true, `strstart > base`; `base ≤ strstart ≤ read ≤ N`;
`read − base ≤ 2w`; `high_water ≤ 2w`; `match_length ≤ 258`; and if `match_length ≥ 3`,
`base ≤ match_start`, `match_start + match_available < strstart` and
`strstart − match_start ≤ w`.

The emulator is rebuilt from the data and the checkpoint: a fresh one, then `base`, `read`,
`high_water`, `match_available` and `match_length` as given, `strstart` and `match_start`
(0 if `match_length = 2`) made window positions (minus `base`), and
`lookahead = read − strstart`. Window index `i` (data position `a = base + i`) holds `D[a]` if
`a < read`; else 0 if `i ≥ high_water`; else `D[a − kw]` for the least `k ≥ 1` with
`kw ≤ base` and `a − kw < N` (what it held before the last slides: once the input is all
read the window can still slide, past what it held before), or 0 if there is none. For slow,
every data position `a` from `max(base, strstart − w)` up to `strstart − 1` with
`a + 3 ≤ read` is inserted again, in increasing order, as window position `a − base`.

*Informative:* the encoder writes a checkpoint for a segment where the emulation of the
previous segment still follows the tokens, has no pending token, is not done, its
`strstart − match_available` is the segment's position, and the emulator rebuilt from the checkpoint exists; it codes that segment with the rebuilt emulator.
Rebuilding is exact: the rebuilt emulator predicts as the original would. When the stream
looks like zlib's but its emulation cannot be resumed (fast), the encoder codes the whole
stream as one segment.

## Header candidates

All builders work on 64-bit counts. Leaves are the symbols with a nonzero count.

### Code lengths

Candidate code lengths `k` (288 literal/length lengths, 32 distance lengths) are built from
`lf` and `df` as follows:

| k | builder |
|---|---|
| 0 | zlib's: `zlib(lf[0..286), 15)` and `zlib(df[0..30), 15)`, padded with zeros |
| 1 | `pm` of zopfli-smoothed counts, limit 15, with the distance patch |
| 2 | `pm` of brotli-smoothed counts, limit 15, with the distance patch (ECT's default) |
| 3 | `pm` of the counts as they are, limit 15, with the distance patch |
| 4 | `pm` of the counts as they are, limit 15, without the patch |
| 5 – 10 | `pm` of the counts as they are, limits 14 down to 9, with the patch |
| 11 – 16 | `pm` of zopfli-smoothed counts, limits 14 down to 9, with the patch |
| 17 – 22 | `pm` of brotli-smoothed counts, limits 14 down to 9, with the patch |

`pm` and the smoothing apply to the full 288 and 32 counts, separately.

**zlib's** (`build_tree` and `gen_bitlen` of zlib's `trees.c`), on `f[0..n)` with limit `M`:

```
heap = []; max_code = −1; depth[i] = 0
for i in 0..n: if f[i] ≠ 0: append i to heap; max_code = i
while |heap| < 2:                                # at least two codes
  if max_code < 2: max_code += 1; node = max_code
  else: node = 0
  append node to heap; f[node] = 1
key(i) = 256 · f[i] + depth[i]                   # by frequency, then depth
down(k): v = heap[k]; j = 2k
  while j ≤ |heap|:
    if j < |heap| and key(heap[j+1]) ≤ key(heap[j]): j += 1
    if key(v) ≤ key(heap[j]): stop
    heap[k] = heap[j]; k = j; j = 2j
  heap[k] = v                                    # heap is 1-based
for k = |heap| div 2 down to 1: down(k)
order = []; node = n
repeat
  a = heap[1]; heap[1] = last of heap; remove the last; down(1)
  b = heap[1]
  append a, b to order
  f[node] = f[a] + f[b]; depth[node] = max(depth[a], depth[b]) + 1
  dad[a] = dad[b] = node; heap[1] = node; down(1); node += 1
until |heap| < 2
append heap[1] to order                          # the root
len[root] = 0; overflow = 0; bl_count[*] = 0
for x in order without the root, from the last to the first:
  bits = len[dad[x]] + 1
  if bits > M: bits = M; overflow += 1           # internal nodes count too
  len[x] = bits; if x ≤ max_code: bl_count[bits] += 1
if overflow > 0:
  repeat
    bits = M − 1; while bl_count[bits] = 0: bits −= 1
    bl_count[bits] −= 1; bl_count[bits+1] += 2; bl_count[M] −= 1; overflow −= 2
  until overflow ≤ 0
  h = 0
  for bits = M down to 1:
    k = bl_count[bits]
    while k ≠ 0: x = order[h]; h += 1; if x ≤ max_code: len[x] = bits; k −= 1
result[i] = len[i] if i ≤ max_code and f[i] ≠ 0, else 0   # f as updated: forced nodes have f = 1
```

**`pm(f, M)`** (zopfli's `ZopfliLengthLimitedCodeLengths`). Sort the leaves by (count,
symbol). With 0 leaves all lengths are 0; with 1 or 2 they get length 1. Otherwise, first
try Huffman's two-queue algorithm: queue A holds the sorted leaves, queue B the packages in
the order made; repeatedly take the lightest front item twice (a package before a leaf of
equal weight) and append their sum to B, until one item is left; a leaf's length is its
depth. If no length exceeds `M`, that is the result. Else, if `2^M` is below the number of
leaves, all lengths are 0 (this cannot happen with the sizes used here). Else, package-merge
over arrays:

```
levels = min(M, leaves − 1)
list[0] = the sorted leaves (as weights, each marked leaf)
for j in 1 .. levels − 1:
  packages = sums of consecutive pairs of list[j−1] (an odd last item is dropped)
  list[j] = merge of the sorted leaves and the packages: a leaf goes first only if strictly
            lighter than the next package
need = 2 · leaves − 2
for j = levels − 1 down to 0:
  items = the first min(need, |list[j]|) items of list[j]
  count[j] = the number of leaves among items; need = 2 · (|items| − count[j])
length of the i-th sorted leaf (from 0) = the number of j with i < count[j]
```

**Distance patch** (zopfli's `PatchDistanceCodesForBuggyDecoders`), on the distance lengths
`d`: with no nonzero length among `d[0..30)`, `d[0] = d[1] = 1`; with exactly one,
`d[1] = 1` if `d[0]` is nonzero, else `d[0] = 1`.

**zopfli's smoothing** (`OptimizeHuffmanForRle`), in place on `c[0..n)`:

```
len = 1 + the last index with c nonzero; none: return
good[0..len) = false
sym = c[0]; stride = 0
for i in 0..len (inclusive):
  if i = len or c[i] ≠ sym:
    if (sym = 0 and stride ≥ 5) or (sym ≠ 0 and stride ≥ 7): good[i−stride .. i) = true
    stride = 1; if i < len: sym = c[i]
  else: stride += 1
stride = 0; limit = c[0]; sum = 0
for i in 0..len (inclusive):
  if i = len or good[i] or |c[i] − limit| ≥ 4:
    if stride ≥ 4 or (stride ≥ 3 and sum = 0):
      v = (sum = 0) ? 0 : max(1, (sum + stride div 2) div stride)
      c[i−stride .. i) = v
    stride = 0; sum = 0
    limit = (i + 3 < len) ? (c[i] + c[i+1] + c[i+2] + c[i+3] + 2) div 4
          : (i < len) ? c[i] : 0
  stride += 1
  if i < len: sum += c[i]
```

**brotli's smoothing** (`OptimizeHuffmanCountsForRle` as ECT uses it), in place on
`c[0..n)`, in signed arithmetic, with `c[j] = 0` for `j ≥ n`:

```
len, good: as for zopfli's (a run of 7 or more of any value, or of 5 or more zeros)
stride = 0; sum = 0; limit = floor(256 · (c[0] + c[1] + c[2]) / 3) + 420
for i in 0..len (inclusive):
  ci = (i < len) ? c[i] : 0
  if i = len or good[i] or (i > 0 and good[i−1]) or |256 · ci − limit| ≥ 1240:
    if stride ≥ 4:
      v = (sum + stride div 2) div stride; if v < 1 and sum ≠ 0: v = 1
      c[i−stride .. i) = v
    stride = 0; sum = 0
    limit = (i < len − 2) ? floor(256 · (c[i] + c[i+1] + c[i+2]) / 3) + 420 : 256 · c[i]
  stride += 1
  if i < len:
    sum += c[i]
    if stride ≥ 4: limit = (256 · sum + stride div 2) div stride
    if stride = 4: limit += 120
```

### Run-length codings

Candidate codings of `lens` (cut to HLIT and HDIST), as ops (a symbol 0 to 18 and the value
of its extra bits):

| k | coding |
|---|---|
| 0 | zlib's, on `lens[0..HLIT)`, then again on `lens[HLIT..)` |
| 1 | zlib's, on all of `lens` |
| 2 | libdeflate's (`deflate_compute_precode_items`), on all of `lens` |
| 3 – 22 | zopfli's / ECT's `EncodeTree` on all of `lens`, with flags number `k − 3` |

**zlib's** (`send_tree`), on `L[0..n)`, with `L[n]` reading as −1:

```
prevlen = −1; nextlen = L[0] (−1 if n = 0); count = 0
(max, min) = (nextlen = 0) ? (138, 3) : (7, 4)
for i in 0..n:
  curlen = nextlen; nextlen = L[i+1]; count += 1
  if count < max and curlen = nextlen: continue
  if count < min: emit curlen, count times
  else if curlen ≠ 0:
    if curlen ≠ prevlen: emit curlen; count −= 1
    emit (16, count − 3)
  else if count ≤ 10: emit (17, count − 3)
  else: emit (18, count − 11)
  count = 0; prevlen = curlen
  (max, min) = (nextlen = 0) ? (138, 3) : (curlen = nextlen) ? (6, 3) : (7, 4)
```

**libdeflate's**:

```
i = 0
while i < n:
  v = L[i]; end = the first index after i with L[end] ≠ v, or n
  if v = 0:
    while end − i ≥ 11: x = min(end − i − 11, 127); emit (18, x); i += 11 + x
    if end − i ≥ 3: x = min(end − i − 3, 7); emit (17, x); i += 3 + x
  else if end − i ≥ 4:
    emit v; i += 1
    repeat: x = min(end − i − 3, 3); emit (16, x); i += 3 + x  until end − i < 3
  while i < end: emit v; i += 1
```

**zopfli's / ECT's**: flags number `k` (0 to 19) is the `k`-th of the integers 0 to 31 that
are odd or below 8, in increasing order (0 to 7, 9, 11, …, 31). Its bits 0 to 4 are
`use_16`, `use_17`, `use_18`, `fuse_8` and `fuse_7` (the last two are ECT's).

```
i = 0
while i < n:
  v = L[i]; count = 1
  if use_16 or (v = 0 and (use_17 or use_18)):
    while i + count < n and L[i + count] = v: count += 1
  i += count
  if v = 0 and count ≥ 3:
    if use_18: while count ≥ 11: c = min(count, 138); emit (18, c − 11); count −= c
    if use_17: while count ≥ 3: c = min(count, 10); emit (17, c − 3); count −= c
  if use_16 and count ≥ 4:
    count −= 1; emit v
    while count ≥ 3:
      if fuse_8 and count = 8: emit (16, 1), (16, 1); count = 0
      else if fuse_7 and count = 7: emit (16, 1), (16, 0); count = 0
      else: c = min(count, 6); emit (16, c − 3); count −= c
  emit v, count times
```

### Code-length codes

The code-length code is built from the counts of the ops' symbols (`cf[0..19)`): code 0 is
zlib's, `zlib(cf, 7)`; code 1 is `pm(cf, 7)`.

## Rebuilding the stream

1. Read the main stream. Resolve the blocks' token counts against the segments' total, and
   note where the stored blocks fall between tokens.
2. Decode the segments (in parallel if wanted), each from its position and with its own
   table, models or contexts, and emulator.
3. Read the header of each dynamic block from the header stream, against its tokens.
4. Write the DEFLATE stream (RFC 1951), bits least significant first. Each block starts with
   BFINAL (1 for the last block) and BTYPE in 2 bits.
   - A stored block: its padding value in the bits up to the next byte boundary (it must fit
     in them), LEN and its complement NLEN in 16 bits each, then LEN bytes of the data from
     the current data position.
   - A fixed block: its tokens with the fixed codes, then the end of block (symbol 256).
   - A dynamic block: HLIT − 257 (5 bits), HDIST − 1 (5), HCLEN − 4 (4), the code-length code
     lengths of `CL_ORDER[0..HCLEN)` (3 bits each), the ops (each its canonical code-length
     code, then its extra bits), then its tokens with the canonical codes of
     `lens[0..HLIT)` and `lens[HLIT..)`, then the end of block.
   - A literal is its symbol; a match is the symbol `257 + c` of its length code (`c = 27`
     for alt258) with the length's extra bits, then its distance code with the distance's
     extra bits. Every symbol written must have a nonzero code length.

   Then the final padding value in the bits up to the next byte boundary (it must fit), and
   the trailing bytes. (A record with no blocks gives just the trailing bytes; encoders never
   write one.)

A record that breaks any rule of this document is invalid; what a decoder outputs for it is
not specified (the reference decoder reports an error, or may produce a stream that
reconstructs other data).

## Implementation limits

The reference decoder also rejects: more blocks than `N + 8(b + 1) + 2^20`, with `b` the bit
position in the main stream where the block count starts; and data of 2^32 − 1 bytes or more
(its tables hold 32-bit positions), for which this document does not define the rule's
tables. `deconstruct` keeps to tighter limits; see [Domain](#domain).

## Formula module

The same representation as a module of entry formulas (`aqr-formula`), in
`formula::apply` and `formula::decode`. The module is `flate-recons`, version 1. Its output
is the DEFLATE stream with its trailing bytes, whose length the application gives, rebuilt as
in [Rebuilding the stream](#rebuilding-the-stream).

### Domain

The module rebuilds any byte string that `deconstruct` reads as a raw DEFLATE stream (RFC
1951; no zlib or gzip wrapper, no preset dictionary) followed by trailing bytes, the bytes
after the one holding the final block's last bit. The stream must be valid under RFC 1951 up
to the end of its final block, with two relaxations:

- a dynamic header may declare HLIT up to 288 and HDIST up to 32, as long as the stream codes
  no literal/length symbol 286 or 287 and no distance symbol 30 or 31;
- its codes may be incomplete (but not oversubscribed), as long as every symbol read has a
  code.

It must also be within the limits of a record: `N < 2^32 − 1`, at most 2^30 trailing bytes
(see [Blocks](#blocks)), and at most `N + 2^20` blocks. The last is tighter than the
decoder's bound in [Implementation limits](#implementation-limits), which allows another
`8(b + 1)`, so that it holds whatever precedes the block count.

`deconstruct` rejects any other input. A formula holds such input as it is rather than as an
application of this module.

**Every stream in the domain has a record**, in every profile and with every rule. For
instance, with the rule `literals` (in the catalog or in full):

- one segment, and for adaptive profiles no zlib emulation;
- the blocks as in the stream: stored blocks with length code 2 and their padding, fixed and
  dynamic blocks with their own token counts;
- static profiles: the default model for every field (it uses all 72 symbols);
- every match a miss of kind 1 (or 3 for alt258), with entry 0 and a raw distance; every
  literal is a hit;
- every dynamic header coded with bit 0: candidate 0, HLIT and HDIST explicit, the
  differences from candidate 0's lengths, the run-length coding written out, and the
  explicit code-length code with an explicit HCLEN.

### Parameters and inputs

**Parameters**: none (empty).

**Inputs**: exactly two, in this order.

| slot | content | already compressed |
|---|---|---|
| 0 | the data `D` | no: whatever the stream held (text, image rows, …) |
| 1 | the [record](#the-record): the profile (u16) and the body, without the magic | mostly: the corrections are rANS or arithmetic coded, and the main and header streams pack their fields in bits; general-purpose compression gains little |

The module has no classes of its own: the maker of the formula names the inputs (e.g. `raw`
and `flate/record` in aqr-compress, slices of `pdf/pool-<class>` and `pdf/meta` in
aqr-pdf).

The module fails on any parameters, any other count of inputs, an invalid record that it
detects, or a rebuilt stream whose length is not the application's declared output size.
The specification ("Module execution") makes such a failure an invalid archive failure.

### Encoder obligations

The decoder does not check everything this document requires. Its output for an invalid
record is not specified (see [Rebuilding the stream](#rebuilding-the-stream)), so an encoder
must verify that its record rebuilds the original before using it.

Where this document leaves a choice, every option is valid and gives the same stream:

- the profile, the rule, and the rule as a catalog index or in full;
- where segments are cut, and for adaptive profiles whether a segment has zlib emulation or
  resumes it;
- for static profiles, each field's default model or its own table (which must hold every
  symbol the field codes);
- for each block, its own token count or the previous one's (when equal), and which block
  takes the rest; for each stored block, length code 0, 1 or 2 where they agree;
- for each distance, its entry, its rank or the raw distance (the decoder takes whichever
  is coded);
- for each dynamic header, the recipe's header or any candidate with the differences, any
  matching run-length coding (a candidate, or written out), and any matching code-length
  code (zlib's, package-merge or explicit), with HLIT, HDIST and HCLEN natural or explicit.

The decoder ignores the following; encoders write none of them:

- bits left over at the end of a bit stream, and nonzero bits in the padding up to its byte
  boundary;
- rANS bytes and extra bits left over at the end of a segment's corrections (static
  profiles);
- varints longer than needed (up to 10 bytes).

## Encoder choices (informative)

- **Rule.** Static profiles look the stream's fingerprint up in the catalog's `map` lines
  (one rule, or a few tried on the first 4 KiB of data), else try every rule consistent with
  the tokens on the first 4 KiB. Adaptive profiles also try the best of a few chain rules and,
  when the stream looks like zlib's (detected by emulating candidate parameters), zlib
  emulation, keeping the smallest.
- **Segments** cover about 1 MiB of data each and are cut only before tokens that no stored
  block comes before.
- **Models** (static): a field's default model unless its own table plus its coding is
  smaller.
- **Headers.** The recipe's header is checked first. Otherwise the closest of the recipe's
  candidate lengths and (when the last search found exact ones, or every 16 blocks) the first
  five candidates is coded, the recipe's run-length coding or (now and then) the first that
  matches, and the first code-length code that matches.
