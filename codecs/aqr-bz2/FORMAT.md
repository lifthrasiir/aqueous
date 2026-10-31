# The `bz2-recons` module

This document specifies the module `bz2-recons` of aqr-formula entry formulas. The module
rebuilds bzip2 streams, one after another, from the data they hold: its output is what
libbzip2 (1.0.3 and later, the default work factor) gives for that data. The document is meant
to be complete: an implementation of the module can be written from it alone. Sections marked
*(encoder)* describe the choices this implementation makes when it builds formulas; the module
does not depend on them.

The module is a compressor, so the algorithm itself is normative. The output must match
libbzip2 to the bit.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [Streams](#3-streams)
4. [Blocks](#4-blocks)
5. [Huffman codes](#5-huffman-codes)
6. [Encoding](#6-encoding-encoder) *(encoder)*
7. [Validation](#7-validation)

## 1. Conventions

- Integers are unsigned. **varint**: unsigned LEB128, least significant group first; a writer
  uses the shortest form.
- Bits are written MSB first: `bits(v, n)` writes the `n`-bit value `v`, its most significant
  bit first, and bytes fill from their most significant bit. Writing continues across blocks
  without alignment; a stream ends padded with zero bits to a byte.
- **crc(d)**: CRC-32 with the polynomial `0x04C11DB7`, MSB first (not reflected), initial value
  `0xFFFFFFFF`, the result inverted (CRC-32/BZIP2; `crc("123456789") = 0xFC891918`).
- `rotl(x)` rotates a 32-bit `x` left by one.
- The bzip2 format itself (what a decoder reads) is not restated beyond what fixes the
  encoding.

## 2. The module

- **Module**: name `bz2-recons`, version 1.
- **Params**:

  ```text
  varint  S              streams, 1 or more
  S times:
    u8      level        1..9
    varint  n            bytes of data of the stream
    varint  K            blocks whose rank is given
    K times:
      varint  g          the block's index is the previous given one's + 1 + g (the first: g)
      varint  rank       the block's origPtr, below 2^24
  ```

  Nothing else may follow.
- **Resources**: none.
- **Inputs**: exactly one, the data `D`. It is cut into the streams' data in order, `n` bytes
  each; the `n` must add up to `|D|`.
- **Output**: the streams `stream(Dᵢ, levelᵢ)` (§3), one after another, each block given a
  rank taking that rank instead of its own (§4.2). Its length must be the `len` the
  application declares.

## 3. Streams

`stream(D, L)` is `BZh`, then the digit of `L` (`0x30 + L`), then the blocks of §3.1, each
compressed by §4, then `bits(0x177245385090, 48)`, `bits(C, 32)` and the padding. `C` starts at
0 and becomes `rotl(C) ^ crc(B)` for each block's data `B`, in order. Empty data has no
blocks.

### 3.1 Where blocks are cut

Let `M = 100000 × L − 19`. Cut `D` into **runs**: from the start, each run is the longest
stretch of one byte value starting there, of at most 255 bytes. A run of `r` bytes makes
`sym(r)` symbols of RLE1 (§4.1): `r` if `r < 4`, else 5.

The first block starts at 0. Go through the runs in order, keeping a count `c`, 0 at a block's
start. When a run starts at position `p > 0`, the run before it adds `sym` of its length to
`c`; then, if `c ≥ M` and `p + 1 < |D|`, a block starts at `p` and `c = 0`. The last block
ends at `|D|`.

*Note.* This is how libbzip2 fills blocks: it checks for a full block before each byte it
takes, and puts a run into the block once the run has ended. A run under way when the block
fills goes into the next block, whole.

## 4. Blocks

A block of data `B` (not empty) is compressed as follows.

### 4.1 RLE1, BWT and MTF

1. **RLE1**: each run of `B` (as in §3.1) of `r < 4` bytes is itself; a run of `r ≥ 4` bytes
   of value `x` is `x x x x` and the byte `r − 4`. This gives the block `T`, of `N` bytes.
2. **Symbols in use**: the byte values occurring in `T`, `u` of them, numbered in increasing
   order of value: value `v` has the number `seq(v)`.
3. **BWT**: sort the `N` rotations of `T` (rotation `i` is `T[i..N] T[0..i]`) as byte strings.
   `last` is the sequence of the last byte of each rotation in sorted order, and `rank`
   (`origPtr`) is the position of rotation 0 in that order. Equal rotations (where `T` is
   periodic) may come in any order: `last` does not depend on it, and `rank` is the position
   of the last of them in descending order of start — libbzip2 orders equal rotations
   otherwise, which the params give where it matters.
4. **MTF**: with a list of the numbers `0..u` in order, for each byte `x` of `last`: let `j`
   be the position of `seq(x)` in the list; move it to the front. A run of `z` zeros (`j = 0`)
   in a row, ended by a nonzero `j` or the end of `last`, is written as the digits of `z + 1`
   in base 2 without its leading 1, least significant first, each 0 as RUNA (symbol 0) and
   each 1 as RUNB (symbol 1). A nonzero `j` is the symbol `j + 1`. After `last`, the symbol
   EOB `= u + 1`. This gives the symbols `m`, `n_m` of them, of an alphabet of `A = u + 2`;
   `freq(s)` is how often `s` occurs in `m`.

### 4.2 The header

`bits(0x314159265359, 48)`, `bits(crc(B), 32)`, `bits(0, 1)`, `bits(rank, 24)`, the `rank`
of §4.1 unless the params give the block another.

### 4.3 The tables and their selectors

The number of tables `G` is 2 if `n_m < 200`, 3 if `n_m < 600`, 4 if `n_m < 1200`, 5 if
`n_m < 2400`, else 6. Each table `t` holds a length `len[t][s]` for each symbol `s < A`.

**At first**, the tables are made for ranges of symbols, from the last table down. With
`left = n_m` and `start = 0`, for `k = G, G−1, …, 1` (the table `k − 1`):

1. `share = ⌊left / k⌋`. Take symbols from `start` on, adding their `freq` to `got` (from 0),
   until `got ≥ share` or the symbol `A − 1` has been taken. Let `end` be the last taken
   (`end = start − 1` if none was).
2. If `end > start`, `k ≠ G`, `k ≠ 1` and `G − k` is odd, give the symbol `end` back:
   `got −= freq(end)`, `end −= 1`.
3. `len[k−1][s] = 0` for `start ≤ s ≤ end`, 15 for every other `s`.
4. `start = end + 1`, `left −= got`.

**Then four passes.** Each pass:

1. Cut `m` into groups of 50 symbols (the last may be shorter). For each group, the cost of a
   table is the sum of its lengths of the group's symbols; the group's **selector** is the
   table of least cost, the first of them if several are equal. The table's frequencies
   (zero at the start of the pass) count the group's symbols.
2. Each table's lengths become the code lengths of its frequencies by §5.1, with a limit of
   17.

The selectors of the last pass are written, with the lengths after it.

### 4.4 Writing the block

After the header:

1. **The symbols in use**: 16 bits, bit `i` (first written first) set if any byte value
   `16i..16i+16` is in use; then for each `i` set, 16 bits, bit `j` set if `16i + j` is.
2. `bits(G, 3)`, `bits(number of selectors, 15)`.
3. **The selectors**, MTF coded: with a list of `0..G` in order, for each selector, its
   position `j` in the list as `j` one bits and a zero bit; the selector moves to the front.
4. **The lengths** of each table in turn: `bits(len[t][0], 5)`, then for each symbol `s` in
   order, from the current length `c` (at first `len[t][0]`): `bits(2, 2)` and `c += 1` while
   `c < len[t][s]`, `bits(3, 2)` and `c −= 1` while `c > len[t][s]`, then `bits(0, 1)`.
5. **The symbols** `m`, each group with its selector's table: `bits(code[t][s], len[t][s])`,
   with the codes of §5.2.

## 5. Huffman codes

### 5.1 Lengths

The code lengths for frequencies `f[0..A]` with a limit `X`, as libbzip2 makes them:

1. The weight of a leaf `s` is `max(f[s], 1) × 256`. A weight's low 8 bits are a depth, its
   high bits a frequency; a leaf's depth is 0.
2. Nodes are numbered: the leaves `1..A` (leaf `s` is node `s + 1`), new nodes from `A + 1`
   on. A binary min-heap `h[1..]` of nodes is kept by weight, with `h[0]` a node of weight 0
   that every other weight exceeds.
   - **sift up** from `i`: while the weight of `h[i]` is less than that of `h[⌊i/2⌋]`, swap
     them, `i = ⌊i/2⌋`.
   - **sift down** from 1, in a heap of `n`: let `x = h[1]`, `i = 1`; loop: `c = 2i`; stop if
     `c > n`; if `c < n` and the weight of `h[c+1]` is less than that of `h[c]`, `c += 1`; stop
     if the weight of `x` is less than that of `h[c]`; else `h[i] = h[c]`, `i = c`. Then
     `h[i] = x`.
3. Add the leaves `1, 2, …, A` in order, each at `h[n+1]` (`n` the size before) and sifted up.
4. While the heap holds more than one node: take `a = h[1]`, move `h[n]` to `h[1]`, `n −= 1`,
   sift down; the same again for `b`. The new node's weight is
   `((w(a) & ~255) + (w(b) & ~255)) | (1 + max(w(a) & 255, w(b) & 255))`, and it is the parent
   of `a` and `b`; add it to the heap as in step 3.
5. A symbol's length is the depth of its leaf (the number of parents above it).
6. If any length exceeds `X`, every leaf's weight `w` becomes `(1 + ⌊(w / 256) / 2⌋) × 256`,
   and the tree is built again from step 2.

### 5.2 Codes

Canonical: by increasing length, and among equal lengths by increasing symbol, each symbol
takes the next code; the code is shifted left by one bit as the length grows by one.

## 6. Encoding *(encoder)*

`aqr_bz2::formula::encode` takes the bzip2 streams at the start of a file, one after another
(pbzip2 writes one per piece of its input, and `bzip2` reads such concatenations). Each stream
is decoded; its blocks must be cut as §3.1 cuts its data, and compressing them must give the
stream back, its first block checked before the rest are compressed. A block whose rank
differs from the stream's gets the stream's in the params. The first stream not reproduced,
and anything else after the streams, is sliced as it is (a stream of class `bz2`); the data,
of all streams, goes in a stream of class `raw`, which aqr-compress recompresses in turn.
Data beyond 2^30 bytes is not taken.

What is not reproduced, and stays as it is: other compressors (lbzip2, 7-Zip's, Dart's and
others' own), libbzip2 before 1.0.3 where a code would be longer than 17 bits, randomised
blocks (bzip2 0.9.0), and streams cut short by flushing (`BZ_FLUSH`).

## 7. Validation

The module fails if:

- the params are malformed: no stream, a level outside 1..9, a block given a rank that the
  stream does not have, a rank not below the block's `N`, a missing byte or trailing bytes;
- there is not exactly one input, or the streams' `n` do not add up to its length;
- the output is not `len` bytes long.
