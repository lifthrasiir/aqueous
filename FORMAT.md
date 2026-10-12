# flate-recons format

flate-recons splits a raw DEFLATE stream into its decompressed **data** and a **record** of
everything else the encoder chose. From the data and the record the stream is rebuilt bit for
bit. This document describes the record, and the `.flate.aqr` file that stores a record with
its data.

Where this document says an algorithm is defined by the source (the rules' tables, zlib's
emulation, the Huffman tree builders), the source in this repository is normative.

## Conventions

- Integers in byte fields are little endian.
- **varint**: unsigned LEB128 (7 bits per byte, least significant group first, high bit set
  on every byte but the last).
- **Bit streams** are packed least significant bit first, as DEFLATE does. An `n`-bit field
  holding `v` is written least significant bit first. A bit stream ends with zero bits up to
  a byte boundary.
- **gamma(v)** (`v ≥ 0`) is Elias gamma of `v + 1` in a bit stream: with
  `k = floor(log2(v + 1))`, it is `k` one bits, a zero bit, then the low `k` bits of `v + 1`
  as a `k`-bit field.
- **pad(p)** is a padding value of 0 to 127: one bit, 0 for zero, else 1 followed by `p` in 7
  bits.

## The `.flate.aqr` file

| bytes | field |
|---|---|
| 6 | magic `69 04 5f 95 ab 77` (`aQRflat3` in base64) |
| 2 | profile (u16) |
| 4 | length of the record body (u32) |
| … | the record body |
| … | the data, as zstd frames |

The profile and the body together are the record. The data is the decompressed content of
the DEFLATE stream. `flate-recons compress` writes it with zstd at level 19 by default.

## The record

A record is a profile (u16) followed by its body. Formats that embed flate-recons (such as
pdf-recons) store the record without the magic, with their own length framing.
`flate_recons::deconstruct_with` returns a record, and `reconstruct` takes one.

### Profiles

The profile sets the rule catalog that the body indexes rules in, and how corrections are
coded.

| profile | corrections | rule catalog |
|---|---|---|
| 0 – 9 | static codes | `model/rules-lλ.txt` with λ = 200, 100, 50, 30, 20, 10, 5, 2, 1, 0 |
| 10 | adaptive, with lookahead and zlib emulation; priors `model/priors-10.txt` | λ = 0 |
| 11 – 65535 | reserved; a record with one is invalid | |

λ is the speed penalty each catalog was fitted with (bits per kilobyte of data, for every
ns/B a rule takes). Profile 0 is the fastest, and profile 9 the smallest of the static ones.
Profile 6 (λ = 5) is the library's default. Further adaptive profiles (11, 12, …) will be new
sets of priors, possibly with a different context layout.

The catalogs and priors are compiled into the library. A catalog file lists one rule per
line (in the syntax of `Rule`'s `Display`, such as `key=3,ways=8,rank,window=32767`), plus
`map` lines that the encoder uses to choose a rule. A rule's index in the catalog is the
index of its line among the rule lines, counting from 0.

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

### Rule

One bit. If it is 1, gamma(index) follows: the rule at that index in the profile's catalog.
If it is 0, the rule follows in full: its kind in 2 bits (0 search, 1 literals, 2 runs,
3 chain). A chain rule has only its search depth and nice length, as indices into
1, 2, 4, …, 32768 (4 bits) and 8, 16, 24, 32, 64, 128, 192, 258 (3 bits). The other kinds
continue:

| field | bits | value |
|---|---|---|
| key | 1 | key length − 3 |
| ways | 3 | ways − 1 (1, 2, 4 or 8) |
| long | 4 | key length of the second table, 0 for none (else 5 to 8) |
| rank | 1 | whether distances are ranked |
| inner | gamma | positions inserted inside a match up to this length (259: every position) |
| lazy | gamma | lazy evaluation below this length (0: greedy) |
| margin | gamma | lazy margin |
| min_len | gamma | shortest match − 3 |
| far_len | gamma | TOO_FAR length limit (0: none) |
| far_dist | gamma | TOO_FAR distance |
| window | gamma | farthest distance − 1 |

`Rule::check` (`src/tokens.rs`) lists the valid ranges. The catalogs hold no chain
rules; the encoder of an adaptive profile writes one in full when it chooses it.

### Blocks

gamma(number of blocks), then for each block its type in 2 bits (0 stored, 1 fixed, 2
dynamic). The last block is the final one.

- **Stored**: pad(the padding bits before its length fields), then a 2-bit length code: 0
  for the previous stored block's length, 1 for 65535, 2 for a 16-bit length that follows.
- **Fixed or dynamic**: one bit, 1 if the block has as many tokens as the previous fixed or
  dynamic block. Otherwise, as long as no earlier block took the rest, one more bit follows:
  1 means the block holds all the tokens not given to other blocks. If that bit is 0 (or was
  not read), gamma(token count) follows. The encoder gives the rest to the last fixed or
  dynamic block.

Then come pad(the padding after the final block), gamma(number of trailing bytes after the
stream), and each trailing byte in 8 bits.

The total token count is the sum of the segments' counts (below).

### Coding parameters

**Static profiles**: one model for each of the nine correction fields (see
[Corrections](#corrections)), in field order. Each model starts with one bit. If it is 1,
the field uses its default model: the field's line of `model/models.txt` (one line per
field in order, comments aside, giving the 72 symbols' `q`). If it is
0, a table follows:

- one bit, 0 if the field has no symbols at all (no further bits);
- otherwise, the last symbol used in 7 bits, then for every symbol up to it one bit (used or
  not) and, if used, `q − 1` in 6 bits (`q` in 1 to 64).

**Adaptive profiles**: one bit, whether zlib emulation is used. If it is 1, the parameters
follow: level (4 bits, 1 to 9), strategy (3 bits: 0 default, 1 filtered, 2 Huffman only,
3 RLE, 4 fixed), windowBits − 9 (3 bits, up to 15) and memLevel (4 bits, 1 to 9).

### Segments

The tokens are cut into segments that are coded independently.

gamma(number of segments), then for each segment:

- gamma(token count);
- static profiles only: gamma(misses of class 0) and gamma(misses of class 1);
- for every segment but the first: gamma(position), the data position of the segment's first
  token, before any stored block that comes before that token. Positions must not decrease.
- gamma(length of its corrections in bytes);
- adaptive profiles with zlib emulation, for every segment but the first: one bit, whether
  the segment resumes the emulation, then if so its checkpoint (see
  [zlib emulation](#zlib-emulation)).

The main stream then pads to a byte boundary.

## Header stream

The header stream holds, for each dynamic block in order, its Huffman header, coded against
the headers that known encoders would build for the block's tokens. Decoding a header needs
the block's symbol frequencies: 288 literal/length counts (the end-of-block symbol counted
once, length 258 written as symbol 284 counted as 284) and 32 distance counts. So headers are
read after the tokens.

A *recipe* carries over from one dynamic block to the next. It holds a candidate index for
the code lengths, the run-length coding and the code-length code, all 0 at the start
(zlib's).

1. One bit. If it is 1, the header is exactly what the recipe builds. HLIT and HDIST cut the
   lengths after their last nonzero entry (at least 257 and 1), and HCLEN cuts the
   code-length code after its last nonzero entry in DEFLATE's permuted order (at least 4).
   Nothing else follows.
2. Otherwise:
   - gamma(k) chooses candidate code lengths `k`, which become the recipe's. Then
     HLIT − 257 and HDIST − 1 follow, each as one bit 1 if equal to the candidate's natural
     cut, else 0 and the value in 5 bits. Then gamma(number of lengths that differ from the
     candidate's), and for each in order the gamma of the gap since the previous one (or the
     start) and the length in 4 bits.
   - The run-length coding: one bit 1 and gamma(k) for candidate coding `k` (now the
     recipe's), or one bit 0 and the coding written out. Written out, it is gamma(number of
     ops), then per op 2 bits: 0 for a literal length, 1 for symbol 16, 2 for 17, 3 for 18.
     For 16, 17 and 18, one bit follows, 1 if the run is as long as it can be (the run of
     the repeated value at that point, capped at 6, 10 or 138). Otherwise the symbol's extra
     bits follow (2, 3 or 7).
   - The code-length code, in 2 bits: 0 for zlib's, 1 for package-merge (either becomes the
     recipe's), 2 for explicit. HCLEN − 4 follows as one bit 1 if natural (for candidate 0 when
     explicit), else 0 and 4 bits. For explicit, the lengths follow in 3 bits each in
     DEFLATE's order, up to HCLEN.

Candidate code lengths are indexed in this order:
0. zlib's trees;
1. package-merge of zopfli-smoothed counts, limit 15, with zopfli's distance patch;
2. the same with brotli's smoothing (ECT);
3. the same on the counts as they are;
4. package-merge without smoothing or patch;
5. and on: package-merge with the patch at limits 14 down to 9, on the counts as they are,
   then zopfli-smoothed, then brotli-smoothed.

The run-length codings are, in order: zlib's per tree, zlib's over both trees, libdeflate's,
then zopfli's variants. `src/header.rs` and `src/trees.rs` define them.

## Tokens and their prediction

A segment's tokens are predicted one by one, from the data alone. Stored blocks interrupt the
tokens, and their bytes are skipped over. A token is either a hit (exactly the prediction) or
a miss, for which a *correction* is coded.

### The rule

A search rule keeps tables of recent positions (`src/table.rs`). It predicts the
longest match among a bucket's entries, greedily or one step lazily. A rule's table for a
segment starts fresh: with every position inserted, it starts with the window before the
segment's first position inserted.

A **chain** rule (`src/matchfind.rs`) keeps exact chains of every earlier position with the same first three bytes, within 32768
bytes. It predicts the closest longest match among the first `depth` positions of the
chain, and stops at the first match of `nice` bytes or more. It offers no bucket entries,
so a distance's entry field is always 0. Its ranks walk the chain, at most 8192 positions.

Each prediction has a **class**:

- **0**: the table offered no candidate, so a literal is predicted;
- **1**: it offered one (whether a match or a literal is predicted);
- **2**: zlib emulation made the prediction (adaptive profiles only).

### Corrections

A miss is coded as these fields, in this order:

1. **kind** (field 2 for class 0, field 3 for classes 1 and 2): 0 for a literal where a match
   was predicted; 1 for a fresh match where a literal was predicted; 2 for a match whose
   length differs from the predicted match; 3 for length 258 written as symbol 284 (in which
   case no length field follows).
2. The length: for kind 1, **fresh length** (field 4), the length − 3. For kind 2, **length
   delta** (field 5), the zigzag of the actual minus the predicted length (0, −1, 1, −2, …
   map to 0, 1, 2, 3, …).
3. For a match, the distance:
   - **entry** (field 6): 1 + its index among the bucket entries of the position (first table
     most recent first, then the second's not already listed), or 0 if it is not there.
   - If 0 and the rule ranks distances, **rank** (field 8): 1 + its rank among the earlier
     positions matching at least as long (walking a hash chain, at most 1024 steps), or 0.
   - If still 0, **raw distance** (field 7): the distance − 1.

Fields 0 and 1 are the gaps of the static coding (below).

Field values are coded as **bucket symbols** (72 of them) with extra bits. A value `v < 16`
is symbol `v` with no extra bits. Otherwise, let `n` be the bit length of `v` (5 to 32): the
symbol is `16 + 2(n − 5) + b`, where `b` is the bit below the leading one, and the extra bits
are the low `n − 2` bits of `v`.

### Static coding (profiles 0 – 9)

Each segment's corrections are a sequence of field values, coded with the field's model. The
first gap of each class comes first (class 0 then class 1, only for classes with misses).
Then, for each miss in token order, its fields follow, then the gap to the next miss of the
same class (if there is one). A gap is the number of hits of that class before the miss.

Model `q` values give frequencies. Weights are `w(q) = (q odd ? 181 : 128) << (q div 2)`,
taken at `q − 1`. A symbol's frequency is `max(1, floor(w · 2^15 / Σw))` for the symbols in
use. The difference to 2^15 is added to the most frequent one (the lowest of equals).

The corrections of a segment are empty if it has no fields. Otherwise they are a varint
length of the rANS bytes, the rANS bytes, then the extra bits as a bit stream. rANS has a
32-bit state, L = 2^23 and 8-bit renormalization (after ryg's `rans_byte`). The decoder
loads the state from the first 4 bytes, big endian. Then, per symbol:

- `slot = x mod 2^15`, giving symbol `s` with `cum[s] ≤ slot < cum[s+1]`;
- `x = freq[s] · (x >> 15) + slot − cum[s]`;
- while `x < 2^23`, `x = (x << 8) | next byte`;
- the symbol's extra bits are read from the bit stream.

### Adaptive coding (profiles 10 and up)

Each segment's corrections are one binary arithmetic coded stream. Every token codes a hit
flag, and a miss codes its fields down binary trees. Every bit has a context, with an
adaptive probability that starts from the profile's priors.

**Arithmetic coder** (carry-less, after lpaq). The state is two 32-bit bounds `x1 = 0` and
`x2 = 2^32 − 1`. A bit with probability `p` of being 1 (in units of 2^−16) splits the range
at `xm = x1 + ((x2 − x1) · p >> 16)`: a 1 keeps `[x1, xm]`, a 0 keeps `[xm + 1, x2]`. While
the top bytes of `x1` and `x2` agree, that byte is output and both shift left by 8 (`x2`
filling with ones). The stream ends with the single byte `(x1 >> 24) + 1`, or is empty if no
bit was coded. The decoder starts from its first 4 bytes, big endian, and reads zeros past
the end.

**Probabilities.** Context `c` has a probability `p` (16 bits) and a count `n`, both from the
priors file. Each `ctx ID P N` line sets context ID. Unlisted contexts start at `p = 32768`,
`n = 0`. A bit is coded at `p` clamped to [16, 65520]. Then `p += ((t − p) · r(n)) >> 16`,
with `t` = 65535 for a 1 and 0 for a 0, the shift arithmetic, and
`r(n) = floor(131072 / (2n + 3))`. Then `n` grows by one, up to 40. Extra bits are coded at
`p = 32768` with no context.

**Per token**, at position `p` with prediction `pred` (length `pl`, 0 for a literal) of class
`c`:

- `lb(l)` is the length bucket: 0 for `l < 3`, then 1 for 3, 2 for 4, 3 for 5, 4 for 6–8,
  5 for 9–16, 6 for 17–32, 7 for 33–128, 8 for 129–257 and 9 for 258.
- `cmp` is 0, except when the rule predicts a match (class 1). Then the longest match at
  `p + 1` is found: for a search rule, among the entries its table holds at that point,
  inserting nothing; for a chain rule, as its prediction at `p + 1` would be. `cmp` is 1 if
  that match is one longer than `pl`, and 2 if longer still.
- `h[c]` is the class's last two outcomes (bit 0 the latest, 1 for a miss), 0 at the start
  of a segment.
- The flag (1 for a miss) has context `((c · 10 + lb(pl)) · 3 + cmp) · 4 + h[c]`.
  Afterwards `h[c] = (h[c] · 2 + flag) mod 4`.

**A miss** codes the fields of [Corrections](#corrections). The payload is that of class 1
for class 2. Each field is coded down a tree: a value's bits go most significant first, and
node `j` (starting at 1, then `2j` + bit) has context `base + j`.

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

With zlib emulation (adaptive profiles), zlib 1.3.1's `deflate` with the given parameters is
emulated alongside (`src/zlib.rs`: `deflate_fast`, `deflate_slow`, `deflate_rle` and
`deflate_huff`, with zlib's window, hash chains and lazy evaluation). The emulator is driven
by the actual tokens. Where its next token starts at the current position, that token is the
prediction (class 2). The rule's table still takes every token, for the corrections' fields
and for when emulation ends.

The emulation ends for the rest of a segment in three cases: its next token does not start
at the current position (or there is none); it cannot follow the actual token; or, over the
bytes of a stored block, zlib run on its own cannot reach the block's end.

The first segment starts the emulation afresh. Any other segment starts it from its
checkpoint, if it has one. Otherwise that segment has no emulation. A checkpoint is the
emulator's state at the segment's first token, coded relative to that token's position `s`
and the window size `w = 2^windowBits`:

| field | | value |
|---|---|---|
| match_available | 1 bit | deflate_slow's pending literal; `strstart = s + match_available` |
| base | gamma | `strstart div w − base div w` (base: the window's start, a multiple of `w`) |
| read | gamma | `base + 2w − read` (read: the input read into the window) |
| high_water | gamma | `2w − high_water` |
| match_length | gamma | `match_length − 2` |
| match_start | gamma | `strstart − match_start`, only if `match_length > 2` |

The emulator is rebuilt from the data and the checkpoint. The window holds the input, and,
for deflate_slow, every position of the last window before `strstart` is inserted again into
the hash chains. A checkpoint that cannot come from the parameters and the data is invalid.
deflate_fast (levels 1 to 3, other than Huffman-only and RLE) cannot be resumed, because its
hash chains depend on the tokens. When the encoder uses its emulation, it codes the whole
stream as one segment.

## Rebuilding the stream

1. Read the main stream. Resolve the blocks' token counts against the segments' total, and
   note where the stored blocks fall between tokens.
2. Decode the segments (in parallel if wanted), each from its position and with its own
   table, models or contexts, and emulator. A segment must end where the next one starts,
   and the last one where the data ends (past any trailing stored blocks).
3. Read the header of each dynamic block from the header stream, against its tokens.
4. Write the blocks: the stored ones from the data, with their padding and lengths; then the
   final padding and the trailing bytes.
