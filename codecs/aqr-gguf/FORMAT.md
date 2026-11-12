# aqr-gguf format

This document specifies the module `gguf-recons` of aqr-formula entry formulas, which rebuilds
a segment of a GGUF file's tensor data, and how the encoder takes a GGUF file apart around it.
It is meant to be complete: an implementation of the module can be written from it alone.
Sections marked *(encoder)* describe the choices this implementation makes when it builds
formulas; the module does not depend on them.

GGUF itself is llama.cpp's (ggml's `docs/gguf.md`), versions 2 and 3; the layouts of the
quantised blocks are ggml's (`ggml-common.h`), named here by their ggml types.

Contents:

1. [Conventions](#1-conventions)
2. [The formula](#2-the-formula-encoder) *(encoder)*
3. [The symbols' coder](#3-the-symbols-coder)
4. [Runs](#4-runs)
5. [`gguf-recons`](#5-gguf-recons)

## 1. Conventions

- Integers are unsigned unless they are marked signed; numbers in streams are little-endian.
- **varint**: unsigned LEB128, least significant group first, at most 10 bytes. A writer
  always uses the shortest form.
- A *symbol* is a number from 0 to 15. A *group* is 1 to 32 symbols coded with one table
  (§3); a *full group* has 32.

## 2. The formula *(encoder)*

A GGUF file is taken where it starts with `GGUF` and a version of 2 or 3, its header (the
metadata, of which only `general.alignment` is read, and the tensors' infos) holds within the
file, and at least one tensor is of a type coded: F32, F16, BF16 (§4.2), or one of the
quantised types of §4.3 of a whole number of blocks. A tensor is coded where all of it is
within the file and it does not overlap the one before it (by their offsets).

The formula is the concatenation of the header up to the tensor data (the header padded to the
alignment; a slice of the stream `gguf/header`), then the tensor data to the end of the file:

- the bytes not coded between the tensors coded, where 1 MiB or more, as they are (each a
  stream `gguf/data`, the input's bytes shared);
- the rest in *segments* of about 16 MiB, each an application of `gguf-recons` whose runs
  (§4) are the tensors' parts and the bytes between them in turn: a tensor is cut between
  segments at its chunks (§4.2, §4.3). The streams of each application are `gguf/syms` and
  `gguf/raw` (compressed already) and `gguf/side`.

A tensor's part of fewer than 4096 bytes is a run of bytes as they are (§4.1), and so is one
whose coded form is estimated (by the tables' costs) at 98.5% of its bytes or more: noise, or
tensors already near their entropy. A segment of nothing coded is its bytes as they are.

## 3. The symbols' coder

The coded symbols of an application are one stream of static rANS of 32 interleaved states.
The probabilities are of `PROB_BITS = 15` bits: a table is 16 frequencies `freq[s]` adding up
to `2^15`, and `cum[s]` is the sum of those before `s` (`cum[16] = 2^15`). States are 32-bit,
at least `L = 2^16` between symbols.

The stream is the 32 initial states `x[0..32]` (4 bytes each), then 16-bit words, read in
order. A group of `n` symbols with table `t` is decoded state by state, `j` from 0 to `n − 1`:

```text
slot = x[j] mod 2^15
s    = the s for which cum[s] ≤ slot < cum[s + 1]      (the symbol, of a frequency not 0)
x[j] = freq[s] × (x[j] >> 15) + slot − cum[s]
if x[j] < 2^16:  x[j] = x[j] << 16 | the next word
```

so that the words are read in the order of the groups, and within a group of its states.
An initial state below `2^16` is an error. At the end of the application, every state must be
`2^16` and every word read, not one past the end of the stream (where words past it are read as
zeros, an error). A stream of no group is empty (no states).

*(encoder)* The encoder codes the groups from the last, each from its last state: with `x` from
`2^16`, a symbol `s` of table `t` puts out `x mod 2^16` as a word and takes `x >> 16` where
`x ≥ freq[s] × 2^17`, then `x = floor(x / freq[s]) × 2^15 + x mod freq[s] + cum[s]`; the words
are put in the stream in the reverse order of their making. A table is the counts of the
symbols it codes scaled to `2^15`, each counted one at least 1, the difference taken from or
given to the most frequent.

## 4. Runs

The parameters of an application are its runs, one after another to their end; its output is
their outputs one after another. A run is a byte `tag`, then:

| tag | run | then |
|---|---|---|
| 0 | bytes as they are (§4.1) | `varint n` |
| 1, 2, 3 | floats: BF16, F16, F32 (§4.2) | `u8 shift`, `varint n`, `u8 m`, `m` bytes `map`, a table `exp`, and a table `top` where `b > 0` |
| 0x10 + `q` | blocks of type `q` (§4.3) | `varint blocks`, a table for each part of symbols |

A table is 16 varints, its frequencies (§3), which must add up to `2^15`. Any other tag is an
error. The runs' outputs must add up to the declared length.

### 4.1 Bytes as they are

`n` bytes of `side`.

### 4.2 Floats

A float of `W` bytes (2 for BF16 and F16, 4 for F32) has a sign bit, `E` bits of exponent and
`M` bits of mantissa, highest first: BF16 1, 8, 7; F16 1, 5, 10; F32 1, 8, 23. Of a float, the
bits *kept* are its sign then its mantissa's bits above the lowest `shift` (which are zero):
`r = 1 + M − shift` bits, `shift ≤ M`. They are `k = floor(r / 8)` bytes, the lowest first, in
`raw`, and the `b = r mod 8` bits above them a *top* symbol; `b` must be at most 4.

The `n` floats are in *chunks* of 2048 (the last perhaps shorter). Of a chunk of `e` floats:

- the groups of the exponents' symbols: `ceil(e / 32)` groups of table `exp`, of 32 symbols
  each but the last (of `e mod 32` where that is not 0), the `i`-th symbol of the chunk the
  `i`-th float's;
- where `b > 0`, as many groups of table `top`, the floats' top symbols (each below `2^b`;
  `top`'s frequencies from `2^b` on must be 0);
- in `raw`, `k` bytes for each float in turn;
- in `side`, a byte for each float whose exponent symbol `s` is `m` or more (an *escape*), in
  turn: its exponent, which must be below `2^E`.

`map` (`1 ≤ m ≤ 16`) gives the exponents of the symbols below `m`, each below `2^E`. A float is

```text
e    = map[s] for s < m, else the escape's byte
kept = its k bytes | top << (8k)
float = (kept >> (M − shift)) << (E + M) | e << M | (kept mod 2^(M − shift)) << shift
```

put as `W` bytes.

*(encoder)* `shift` is the number of trailing zero bits of the OR of the run's mantissas, made
smaller where `b` would be over 4 (to the largest shift below it for which `b ≤ 4`; 0 always
is). `map` is the 16 commonest exponents of the run, or where it has more than 16, the 15
commonest, the rest escapes, the commonest first (the smaller first on a tie).

### 4.3 Blocks

A block of a type is parts, in the order of its bytes, each:

- **side** of `n` bytes: as they are, in `side`;
- **raw** of `n` bytes: as they are, in `raw`;
- **nibbles** of `n` bytes (a multiple of 16, or 4 or 8): of each 16 bytes (or of all `n` where
  fewer), the symbols the low nibbles of the bytes, then their high nibbles;
- **crumbs** of `n` bytes (a multiple of 8): each 8 bytes a group, its symbols `8i + j` the bits
  `2i` and `2i + 1` of byte `j` (each symbol below 4; the table's frequencies from 4 on must be
  0);
- **high nibbles** of `n` bytes (a multiple of 32): each 32 bytes a group of their high nibbles,
  and 16 bytes in `raw`, of which byte `i` is the low nibble of byte `i` (its low bits) and of
  byte `16 + i` (its high bits);
- **fives of 32** (20 bytes): 32 quants of 5 bits, Q5_0's and Q5_1's: 4 bytes of their fifth
  bits (bit `j` of the 32-bit number, quant `j`'s), then 16 bytes of their low 4 bits (quant
  `j < 16` the low nibble of byte `j`, quant `16 + j` its high nibble). A group of the quants'
  high 4 bits (`quant >> 1`, quant `j` symbol `j`), and 4 bytes in `raw` of their lowest bits
  (bit `j` of the 32-bit number, quant `j`'s);
- **fives of 256** (160 bytes): 256 quants of 5 bits, Q5_K's: 32 bytes `qh`, then 128 bytes
  `qs`; of quarter `k` (0 to 3) and `l` (0 to 31), quant `64k + l` is the low nibble of
  `qs[32k + l]` and bit `2k` of `qh[l]` (its fifth bit), quant `64k + 32 + l` the high nibble
  and bit `2k + 1`. 8 groups of the quants' high 4 bits, quant `j` symbol `j mod 32` of group
  `floor(j / 32)`, and 32 bytes in `raw` of their lowest bits (bit `j mod 8` of byte
  `floor(j / 8)`, quant `j`'s).
- **threes of 256** (96 bytes): 256 quants of 3 bits, Q3_K's: 32 bytes `hmask`, then 64 bytes
  `qs`; of half `h` (0 or 1), `m` (0 to 3) and `l` (0 to 31), quant `128h + 32m + l` is bits
  `2m` and `2m + 1` of `qs[32h + l]` and bit `4h + m` of `hmask[l]` (its high bit). The symbols
  the quants, quant `j` symbol `j` (each below 8);
- **IQ1_S high** (16 bytes): 8 numbers of 16 bits `qh[i]`. The symbols their bits 12–14
  (`qh[i]` symbol `i`, each below 8), and 13 bytes in `raw`: their low 12 bits as a
  little-endian number of 96 bits (`qh[i]`'s bits `12i` to `12i + 11`), then a byte of their
  top bits (bit `i` that of `qh[i]`);
- **IQ1_M scales** (8 bytes): 4 numbers of 16 bits `sc[i]`. The symbols their fields of 3 bits
  (field `k` of `sc[i]`, bits `3k` to `3k + 2`, `k` from 0 to 3, symbol `4i + k`, each below 8),
  and 2 bytes in `raw` of their top nibbles (byte `j` of `sc[2j] >> 12` and
  `(sc[2j + 1] >> 12) << 4`);
- **top nibbles** of `n` bytes (a multiple of 8): `n / 4` numbers of 32 bits `w[i]`. The
  symbols their top nibbles (`w[i] >> 28` symbol `i`), and of each two `w[2j]`, `w[2j + 1]`,
  7 bytes in `raw`: their low 28 bits as a little-endian number of 56 bits, `w[2j]`'s the low;
- **IQ2_XXS words** (64 bytes): 16 numbers of 32 bits `w[i]`. The symbols the top nibbles of
  the odd ones (`w[2i + 1] >> 28` symbol `i`), and 60 bytes in `raw`: the 8 even ones as they
  are (4 bytes each, in order), then the odd ones' low 28 bits as a part of top nibbles has
  them (28 bytes).

Numbers in a block are little-endian. Where a part's symbols are each below 4 or 8, its
table's frequencies from 4 or 8 on must be 0.

| `q` | ggml type | bytes | parts | weights |
|---|---|---|---|---|
| 0 | Q4_0 | 18 | side 2, nibbles 16 | 32 |
| 1 | Q4_1 | 20 | side 4, nibbles 16 | 32 |
| 2 | Q8_0 | 34 | side 2, high nibbles 32 | 32 |
| 3 | Q4_K | 144 | side 16, nibbles 128 | 256 |
| 4 | Q6_K | 210 | nibbles 128, crumbs 64, side 18 | 256 |
| 5 | IQ4_NL | 18 | side 2, nibbles 16 | 32 |
| 6 | IQ4_XS | 136 | side 8, nibbles 128 | 256 |
| 7 | MXFP4 | 17 | side 1, nibbles 16 | 32 |
| 8 | Q5_0 | 22 | side 2, fives of 32 | 32 |
| 9 | Q5_1 | 24 | side 4, fives of 32 | 32 |
| 10 | Q5_K | 176 | side 16, fives of 256 | 256 |
| 11 | Q2_K | 84 | nibbles 16, crumbs 64, side 4 | 256 |
| 12 | Q3_K | 110 | threes of 256, side 14 | 256 |
| 13 | IQ2_XXS | 66 | side 2, IQ2_XXS words | 256 |
| 14 | IQ2_XS | 74 | side 2, raw 64, nibbles 8 | 256 |
| 15 | IQ3_XXS | 98 | side 2, raw 64, top nibbles 32 | 256 |
| 16 | IQ1_S | 50 | side 2, raw 32, IQ1_S high | 256 |
| 17 | IQ3_S | 110 | side 2, raw 104, nibbles 4 | 256 |
| 18 | IQ2_S | 82 | side 2, raw 72, nibbles 8 | 256 |
| 19 | IQ1_M | 56 | raw 48, IQ1_M scales | 256 |

*(encoder)* Of the types of codebooks (IQ1 to IQ3), the grids' indices and the signs are raw:
they are near their entropy order 0 (an index of 8 to 11 bits within 0.15–0.37 bits of it, the
signs at it), and their alphabets are past the coder's 16 symbols. Their scales are the
symbols, and so are Q2_K's and Q3_K's quants.

The parts with symbols are the *parts of symbols*, each with its table, in order. The blocks
are in *chunks* of 8192 weights (32 blocks of 256 weights, 256 of 32; the last chunk perhaps
fewer). Of a chunk, for each part of symbols in turn, the symbols of that part of every block of
the chunk, the blocks in order, in groups of 32 (the last perhaps fewer; a block of the types
0–10 has a multiple of 32 of each part, so its groups are its own); for each part with raw bytes
in turn, its raw bytes of every block, the blocks in order; and for each side part in turn, its
bytes of every block, the blocks in order.

## 5. `gguf-recons`

- **Module**: name `gguf-recons`, version 1. A part module: its inputs are read and its output
  written in order, through windows of 16 pages each.
- **Params**: the runs (§4), at most 2^20 bytes.
- **Resources**: none.
- **Inputs**: `syms` (the coded symbols, §3), `raw` and `side`, each read in order, used up
  exactly by the runs.
- **Output**: the runs' outputs, of the declared length.

*(encoder)* The memory plan is constant beyond the params: the params, a buffer of 128 KiB of
the coded symbols, pieces of 64 KiB of `raw`, `side` and the output, and a chunk's symbols, raw
and side bytes (16 KiB, 6 KiB, 2 KiB).
