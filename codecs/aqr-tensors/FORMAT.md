# aqr-tensors format

This document specifies the module `tensors-recons` of aqr-formula entry formulas, which rebuilds
a segment of the tensor data of a file of tensors, and how the encoder takes such a file apart
around it: GGUF, PyTorch's of `torch.save` and safetensors. It is meant to be complete: an
implementation of the module can be written from it alone.
Sections marked *(encoder)* describe the choices this implementation makes when it builds
formulas; the module does not depend on them.

GGUF itself is llama.cpp's (ggml's `docs/gguf.md`), versions 2 and 3; the layouts of the
quantised blocks are ggml's (`ggml-common.h`), named here by their ggml types. The files of
`torch.save` are those of its ZIP format (PyTorch 1.6 on; `torch/serialization.py`), safetensors
Hugging Face's (`safetensors`' README), of its dtypes as of 0.6.

Contents:

1. [Conventions](#1-conventions)
2. [The formula](#2-the-formula-encoder) *(encoder)*
3. [The symbols' coder](#3-the-symbols-coder)
4. [Runs](#4-runs)
5. [`tensors-recons`](#5-tensors-recons)

## 1. Conventions

- Integers are unsigned unless they are marked signed; numbers in streams are little-endian.
- **varint**: unsigned LEB128, least significant group first, at most 10 bytes. A writer
  always uses the shortest form.
- A *symbol* is a number from 0 to 15. A *group* is 1 to 32 symbols coded with one table
  (§3); a *full group* has 32.

## 2. The formula *(encoder)*

A file is taken where its container's structure holds as below, and at least one tensor is of a
type coded. A tensor is coded where all of it is within the file and it does not overlap the one
before it (by their places); the bytes before the first one coded are the file's *header*. Its
*row* is the elements of its last dimension (GGUF's first), where the container tells it and it
divides the tensor's elements (0 otherwise): of floats, its tiles' rows (§4.5).

- **GGUF**: a file starting with `GGUF` and a version of 2 or 3, its header (the metadata, of
  which only `general.alignment` is read, and the tensors' infos) within the file. Its tensors
  of F32, F16, BF16 and F64 (§4.2), I8, I16, I32 and I64 (lanes, §4.4), or of one of the
  quantised types of §4.3 of a whole number of blocks, are coded; the header is up to the tensor
  data (padded to the alignment), a slice of the stream `gguf/header`.
- **`torch.save`**: a ZIP archive whose first entry, stored, is named `data.pkl` or ends with
  `/data.pkl` (its prefix `P`), its central directory (ZIP64's too) within the file. Of the
  pickle `P/data.pkl`, only the persistent ids are read: tuples `('storage', torch.T, KEY,
  LOCATION, NUMEL)`, a storage's data the entry `P/data/KEY`. The pickle is run by a machine of
  the opcodes of protocols 2 to 5 that keeps strings, integers, globals and tuples (and the memo,
  of which the ids' parts are got again); an opcode of protocol 0 or 1 (text), or one that does
  not hold, and the file is not taken. Storages whose entry is stored and of NUMEL elements'
  bytes are coded, each once: of `FloatStorage`, `HalfStorage`, `BFloat16Storage`,
  `DoubleStorage`, `Float8_e4m3fnStorage` (and `…fnuz…`), `Float8_e5m2Storage` (and `…fnuz…`)
  as floats (`ComplexFloatStorage` and `ComplexDoubleStorage` as two F32 or F64 each), of
  `LongStorage`, `IntStorage`, `ShortStorage`, `CharStorage`, `ByteStorage`, `BoolStorage` and
  `UInt16/32/64Storage` as lanes. Where an entry `P/byteorder` is there and is not `little`,
  the file is not taken. The header (the pickle and the entries' headers before the first
  storage coded) is a slice of `pth/header`.
- **safetensors**: a file of a u64 `N` (little-endian, from 2 to 10^8), then `N` bytes of a JSON
  object (its first byte `{`, its last not a space `}`) within the file, then the data. A
  member of the object (other than `__metadata__`) of `data_offsets` `[a, b]` of a whole number
  of elements of its `dtype` is coded, from byte `8 + N + a`: `F64`, `F32`, `F16`, `BF16`,
  `F8_E4M3`, `F8_E5M2` (and their `…FNUZ`) as floats (`C64`, `C128` as two F32 or F64 each),
  `I64`, `U64`, `I32`, `U32`, `I16`, `U16`, `I8`, `U8`, `BOOL`, `F8_E8M0` as lanes of their
  width, and the packed `F4`, `F6_E2M3`, `F6_E3M2` as lanes of a byte (of no row). The header
  (`8 + N` bytes) is a slice of `safetensors/header`.

The formula is the concatenation of the header, then the tensor data to the end of the file:

- the bytes not coded between the tensors coded, where 1 MiB or more, as they are (each a
  stream `tensors/data`, the input's bytes shared);
- the rest in *segments* of about 16 MiB, each an application of `tensors-recons` whose runs
  (§4) are the tensors' parts and the bytes between them in turn: a tensor is cut between
  segments at its chunks (§4.2 to §4.5). The streams of each application are `tensors/syms` and
  `tensors/raw` (compressed already) and `tensors/side`.

A tensor's part of fewer than 4096 bytes is a run of bytes as they are (§4.1), and so is one
whose coded form is estimated (by the tables' costs) at 98.5% of its bytes or more: noise, or
tensors already near their entropy. A segment of nothing coded is its bytes as they are.

How a tensor of floats is coded is decided once, on a sample of it (8 windows of 1024 floats,
spread over it):

- **A grid** (§4.5) is looked for only where the sample shows the structure of one: blocks of 16
  or 128 floats along the rows, or of 16 down the columns, of fewer distinct magnitudes than
  0.85 times as many as blocks of the sample's floats drawn at random would have; or mantissas
  of an entropy below their bits (but those always zero; at most the bits a sample of its
  size can tell, less 2), less 1; or mantissas of 3 bits or fewer. Then each grid (§4.5's table) on
  each shape of tiles (1 row by 16, 32, 64, 128, 256 or a row; 16, 32, 64 or 128 rows by 1; 128
  by 128) is fitted to about 2048 floats of the sample, the three of the fewest bits (the
  indices' entropy, the scales', the escaped tiles' floats) again to about 32768, and the best
  taken where it comes to less than 0.95 times the floats' coding of the sample. Weights
  converted from a lower precision (NVFP4's, MXFP4's, FP8's of blocks, NF4's, GPTQ's and AWQ's,
  INT8's of rows, to BF16, F16 or F32) are such grids; a run of a grid of which more than a fifth
  of the elements are escaped is coded as floats.
- Otherwise, the **mantissa's top bits** (§4.2) are symbols where, on the sample, their entropy
  given the classes of the exponents (as the coder would make them) is 1.5% of a float's bits
  less than as they are: of a lower precision converted, or of FP8. Of weights trained, they are
  near uniform (BF16's top 4 bits 3.93 given the exponent), and raw.

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
| 1 to 6 | floats: BF16, F16, F32, F64, FP8 E4M3, FP8 E5M2 (§4.2) | `u8 shift`, `varint n`, `u8 m` (and the flag `whole`), `m` varints `map`, a table `exp`, `u8 t`, and where `t > 0` classes and their tables |
| 8 | lanes (§4.4) | `u8 w`, `varint n`, `u8 nib`, two tables for each lane of nibbles |
| 9 | a grid (§4.5) | `u8 type`, `varint n`, `start`, `row`, `br`, `bc`, `u8 mode`, `varint k` and `k` values, tables |
| 0x10 + `q` | blocks of type `q` (§4.3) | `varint blocks`, a table for each part of symbols |

A table is 16 varints, its frequencies (§3), which must add up to `2^15`; a table of symbols
each below `a` must have the frequencies from `a` on 0. *Classes* of the 16 symbols of a
table `T` are a byte `c` (1 to 4), then where `c > 1` 8 bytes, symbol `2i`'s class the low
nibble of byte `i`, `2i + 1`'s its high nibble (each below `c`; all 0 where `c = 1`), then `c`
tables, class `j`'s the `j`-th: of each symbol `x` of `T`, the symbol that goes with it is of the
table of `x`'s class. Any other tag is an error. The runs' outputs must add up to the declared
length.

In a chunk, the symbols of a table `T` and those of its classes go: the groups of `T`'s, then of
each class `j` in turn the symbols that go with `T`'s symbols of class `j`, in the order of
those, in groups of 32 (the last perhaps fewer) of class `j`'s table.

### 4.1 Bytes as they are

`n` bytes of `side`.

### 4.2 Floats

A float of `W` bytes has a sign bit, `E` bits of exponent and `M` bits of mantissa, highest
first:

| tag | float | `W` | `E` | `M` |
|---|---|---|---|---|
| 1 | BF16 | 2 | 8 | 7 |
| 2 | F16 | 2 | 5 | 10 |
| 3 | F32 | 4 | 8 | 23 |
| 4 | F64 | 8 | 11 | 52 |
| 5 | FP8 E4M3 | 1 | 4 | 3 |
| 6 | FP8 E5M2 | 1 | 5 | 2 |

Of a float, the bits *kept* are its sign then its mantissa's bits above the lowest `shift`
(`shift ≤ M`): `r = 1 + M − shift` bits. Their top `t` bits (`t ≤ 4`, `t ≤ r`) are a symbol, of
the table of the class of the float's exponent symbol (§4); the `r − t` below them are *raw
bits*. The byte `m` is `m' | whole << 7`, `1 ≤ m' ≤ 16`.

The `n` floats are in *chunks* of 2048 (the last perhaps shorter). Of a chunk of `e` floats:

- the groups of the exponents' symbols, the `i`-th symbol of the chunk the `i`-th float's (of
  table `exp`), and where `t > 0` the top symbols that go with them by their classes (§4);
- in `raw`, `ceil(e (r − t) / 8)` bytes: the floats' raw bits, float `i`'s at bit `i (r − t)`
  of the chunk's bytes (bit `j` of byte `k` bit `8k + j`, the lowest of a float's first), the
  bits past the last float 0;
- in `side`, for each float whose exponent symbol `s` is `m'` or more (an *escape*), in turn:
  where `whole`, the float's `W` bytes; else its exponent, `ceil(E / 8)` bytes (below `2^E`).

`map` gives the exponents of the symbols below `m'`, each below `2^E`. A float is

```text
e     = map[s] for s < m', else the escape's exponent
kept  = top << (r − t) | its raw bits
float = (kept >> (M − shift)) << (E + M) | e << M | (kept mod 2^(M − shift)) << shift
```

put as `W` bytes, or where it is an escape and `whole`, its `W` bytes of `side` instead.

*(encoder)* Of the counts of the trailing zero bits of the mantissas, `shift` is either that of
all floats (the escapes their exponents) or the one saving most bits with the floats of fewer
zeros whole (`whole`), whichever comes to less. `map` is the 16 commonest exponents of the run,
or where it has more than 16, the 15 commonest, the rest escapes, the commonest first (the
smaller first on a tie). `t` is 0 but where the tensor's sample says the top bits pay (§2); then
of 1 to 4, the one of the fewest bits, the classes of the exponent symbols merged greedily
from one for each (the pair costing least more merged, at most 4 kept, a table counted 300
bits), where it saves 1% of the run's bits.

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

### 4.4 Lanes

`n` elements of `w` bytes (1, 2, 4 or 8): integers, bytes of packed FP4 or FP6, scales of E8M0,
and the like. Byte `j` of each element is its *lane* `j`; a lane of `nib`'s bit `j` set
(`nib >> w` must be 0) is of nibbles, its bytes' low and high nibbles symbols, each of a table
of its own: the tables are, of each lane of nibbles in turn, that of its low nibbles then that of
its high ones.

The elements are in chunks of `8192 / w` (the last perhaps shorter). Of a chunk of `e`:

- of each lane of nibbles in turn, the groups of its `e` low nibbles, then of its `e` high ones
  (the `i`-th symbol the `i`-th element's);
- in `raw`, of each element in turn, its bytes of the other lanes in order.

*(encoder)* A lane is of nibbles where its two tables come to less than 97% of its bytes. Of
packed FP4 (NVFP4's and MXFP4's quants) or INT4 (GPTQ's in I32), the nibbles are the values;
of E8M0's scales, the high nibble is all but constant.

### 4.5 Grids

`n` elements of a float of `type` (1, 2 or 3: BF16, F16 or F32, §4.2), each the value of its
*index* `q` in a *grid* of `k` values (`1 ≤ k ≤ 256`, each an F32, little-endian), times the
*scale* of its tile, less the tile's *zero point* first where `mode` is 1 (`mode` 0 or 1):

```text
x = (g[q] − z) × s     where mode is 1
x = g[q] × s           where mode is 0
```

each operation of F32, rounded to nearest even (IEEE 754's, no fused multiply-add), then `x`
rounded to `type`, to nearest even (of F16, subnormals made and beyond its range infinity); a
NaN is the quiet NaN of `type`, positive (`7FC0`, `7E00`, `7FC00000`), whatever its bits.

The elements are those from element `start` of a tensor of rows of `row` elements (`row ≥ 1`):
element `i` of the run is at row `(start + i) / row` and column `(start + i) mod row`. A
*tile* is `br` rows by `bc` columns (`br ≥ 1`, `1 ≤ bc ≤ row`, `ceil(row / bc) ≤ 16384`): the
element of row `y` and column `x` is of the tile of row `y / br` and column `x / bc`. `k` 0
takes the grid of the run of a grid before it in the application (an error where there is
none).

Where `k ≤ 16`, the index is a symbol, of the one table (its frequencies from `k` on 0); else
its high nibble is a symbol of the first table (from `ceil(k / 16)` on 0) and its low nibble
the symbol that goes with it, by classes (§4), the index `hi × 16 + lo` (an error where it is
`k` or more).

The elements are in chunks of 2048 (the last perhaps shorter). Of a chunk, the groups of its
symbols, then of each element in turn, in `side`:

- where it is the first of its tile in the run (the first element of the run, of each tile it
  is over, at whichever row and column it starts): the tile's scale, 4 bytes of the bits of an
  F32, then where `mode` is 1 and the scale is not `FFFFFFFF`, its zero point (an F32);
- where its tile's scale is `FFFFFFFF` (the tile *escaped*), the element's own bytes.

*(encoder)* The grids are these (index `2c + sign` that of code `c` of a minifloat's magnitude):

| name | values | `k` | mode |
|---|---|---|---|
| e2m1 | FP4 E2M1 by its codes (`-0` as code 8) | 16 | 0 |
| nf4 | bitsandbytes' NF4 | 16 | 0 |
| int4 | −8 to 7 by their nibbles | 16 | 0 |
| uint4 | 0 to 15 | 16 | 1 |
| e4m3 | FP8 E4M3 but its NaN, `2c + sign` | 254 | 0 |
| e5m2 | FP8 E5M2 but its infinities and NaNs | 248 | 0 |
| e2m3, e3m2 | FP6's | 64 | 0 |
| int8 | −128 to 127, zigzag (`2v`, `−2v − 1`) | 256 | 0 |

A tile is fitted whole (all of its elements, of the tensor: the run's own and not), the same
however it is cut between runs. The scales tried first are those of the tile above it and of
the one left of it; then of each of the grid's 8 largest magnitudes, the scale the tile's
largest element would be at, the indices nearest the elements at it (each within 3 ulps), and
of the range of scales that give every element by its index exactly (those of the elements of
an index as near another's as an ulp or two, of a grid finer there than the type, left out
where any are not), the F32 of the fewest bits of mantissa in it, then two further in. A grid of
a zero point (uint4) takes the tile's least and most as its ends (`z` an integer, tried also
1 off). The scale taken is the first giving every element exactly; a tile with none is escaped.
Of the indices of more than 16 values, the classes of the high nibbles are made as a float's
(§4.2).

## 5. `tensors-recons`

- **Module**: name `tensors-recons`, version 2. A part module: its inputs are read and its output
  written in order, through windows of 16 pages each.
- **Params**: the runs (§4), at most 2^20 bytes.
- **Resources**: none.
- **Inputs**: `syms` (the coded symbols, §3), `raw` and `side`, each read in order, used up
  exactly by the runs.
- **Output**: the runs' outputs, of the declared length.

*(encoder)* The memory plan is constant beyond the params: the params, a buffer of 128 KiB of
the coded symbols, pieces of 64 KiB of `raw`, `side` and the output, a chunk's symbols, raw
and side bytes (16 KiB each), and of a grid, of each column of tiles its scale, zero point and
row of tiles (16384 × 16 bytes), and its values (1 KiB).
