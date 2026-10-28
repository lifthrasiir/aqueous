# aqr-clip format

aqr-clip rebuilds a Clip Studio Paint file (`.clip`), the *original*, bit for bit from a
smaller form: it is the module `clip-recons` of aqr-formula. This document specifies that
module completely enough to reimplement both directions.

A `.clip` file holds its layers as 256×256 tiles, each a zlib stream. The tiles that zlib 1.3
at level 1, memLevel 9 makes again exactly are taken out and coded by a model of whole pixels
with adaptive binary arithmetic coding; the decoder makes their zlib streams again. The SQLite
database the file holds is coded by its own recompressor. The rest of the file, the
*skeleton*, is kept as it is.

Sections 1–8 are normative for decoders. Section 9 describes the reference encoder: what it
chooses is free within the constraints of sections 1–8, and a decoder relies only on those.

Other formats this one uses are specified elsewhere:

- zlib (RFC 1950) and DEFLATE (RFC 1951); zlib 1.3.1's compressor (section 8).
- The module of the database's formula: `sqlite-recons` in aqr-sqlite's `FORMAT.md`.

## Contents

1. [Conventions](#1-conventions)
2. [Module](#2-module)
3. [Decoding](#3-decoding)
4. [The walk](#4-the-walk)
5. [Tiles](#5-tiles)
6. [Arithmetic coding](#6-arithmetic-coding)
7. [The tile model](#7-the-tile-model)
8. [zlib streams](#8-zlib-streams)
9. [Encoder](#9-encoder)

## 1. Conventions

- Integers are unsigned unless stated. Fields of the `.clip` file are big endian (BE) unless
  stated.
- **varint**: unsigned LEB128. 7 bits per byte, least significant group first, bit 7 set on
  every byte but the last. Readers accept at most 10 bytes and drop bits beyond bit 63.
  Writers emit the shortest form.
- **string** (in the file): a u32 BE count `n` of UTF-16BE code units, then the `2n` bytes.
  A string *is* an ASCII text `s` when `n` is the length of `s` and every unit is a byte of
  `s` (high byte 0).
- `a[i .. j]` is the elements `i` to `j − 1`. `x div y` and `x mod y` are integer division
  and remainder. `bitlen(x)` is the number of significant bits of `x` (`bitlen(0) = 0`).
- Arithmetic on samples and residuals of the model is mod 256 unless stated. `sx(v)` reads a
  byte `v` as a signed 8-bit value (−128 .. 127).
- *Invalid* means that decoding fails. A decoder must not output anything for an invalid
  input.

## 2. Module

The module is `clip-recons`, version 1, implemented by `module::decode`. Its output is the
`.clip` file (the original) of `total` bytes, the length the application gives.

**Parameters**: a varint `batch`, the tiles a batch holds, `1 ≤ batch ≤ 65536`; nothing may
follow it.

**Inputs**: exactly four (3.1). Any other count is invalid.

There is no checksum to test.

## 3. Decoding

### 3.1 Inputs

| slot | class | content |
|---|---|---|
| 0 | `clip/skeleton` | the original without the modelled tiles' zlib streams and without the database's bytes |
| 1 | `clip/index` | the runs of modelled blocks, then the byte length of each batch's stream (3.2) |
| 2 | `clip/tiles` | the batches' streams, one after another (sections 6, 7) |
| 3 | `clip/database` (or the database's own formula) | the body of the `SQLi` chunk, or nothing where there is none |

### 3.2 Index

varints: `nruns`, then `nruns` run lengths `r[0 .. nruns]`, then one length per batch (3.4).
The runs cover the *eligible* blocks (4.3) in file order: `r[0]` modelled, then `r[1]` not
modelled, and so on alternately; an eligible block past the runs is not modelled. `nruns`
greater than the bytes of the index is invalid, and so is anything after the batch lengths.

### 3.3 Steps

1. Walk the skeleton (section 4) with the runs: the walk gives the sequence of *pieces*:
   `Keep(n)` (bytes of the skeleton), `Tile(layout, n)` (a modelled tile whose zlib stream has
   `n` bytes) and `Database(n)`.
2. Form the batches (3.4) of the `Tile` pieces, read the batch lengths, and split `clip/tiles`
   by them: their sum must be its length.
3. Decode each batch (section 7) and make each of its tiles' zlib streams (section 8). Batches
   are independent and may be decoded at once.
4. Output the pieces in order: for `Keep(n)` the next `n` bytes of the skeleton, for
   `Tile(l, n)` the tile's zlib stream, which must be `n` bytes, for `Database(n)` the
   database input, which must be `n` bytes. All of the skeleton must be used, and the output
   must be `total` bytes.

### 3.4 Batches

Number the modelled tiles in file order. For each layout in the order colour, one plane, two
planes (5.1): its tiles in file order, `batch` at a time (the last batch of a layout may hold
fewer). The batches come in that order, and so do their lengths in the index and their streams
in `clip/tiles`.

## 4. The walk

The walk reads the skeleton (or, for the encoder, the original) from the start, by the sizes
the file keeps. It yields the pieces of 3.3, consecutive `Keep`s merged. Where it reads the
original, the bytes of `Tile` and `Database` pieces are there; where it reads the skeleton, they
are not, and the walk continues after them as if they were. Any read past the end, and any
check below that fails, is invalid.

### 4.1 Container

- The file starts with `CSFCHUNK`, then two u64: the file's size (not checked) and `first`,
  the offset of the first chunk, `24 ≤ first ≤` the length read. `Keep(first)`.
- Then chunks up to the end: `CHNK`, a 4-byte kind, a u64 body size `n`, the body.
  `Keep(16)` for the chunk header, then:
  - kind `SQLi`: `Database(n)`; a second one is invalid.
  - kind `Exta`: external data (4.2).
  - any other kind: `Keep(n)`.

### 4.2 External data

The body is a u64 `idl`, `idl` bytes of ID, a u64 `size`, then `size` bytes of data;
`16 + idl + size` must be `n`. If the data does not start with a u32 then a string that is
`BlockDataBeginChunk`, the body is `Keep(n)`. Otherwise `Keep(16 + idl)` and the data is block
data: records until `size` bytes of data are covered exactly (more is invalid).

- A record starting with a u32 `rec` then the string `BlockDataBeginChunk` is a block: five
  u32 `index`, `len`, `width`, `height`, `has`. Where `has ≠ 0`: a u32 BE `zl + 4` and a u32
  little endian `zl` (the two must agree), then the `zl` bytes of a zlib stream. Then a string
  that is `BlockDataEndChunk`. The record's bytes in the original, the stream included, must
  be `rec`.
- Any other record is a table: a string (its name), u32 `hs`, u32 `count`, u32 `es`, then from
  the u32 `hs` on, `hs + count × es` bytes after the string. `Keep` of all of it.

### 4.3 Blocks

A block with data is *eligible* when `len` is a tile size of 5.1 and `width = height = 256`.
An eligible block is modelled as the runs (3.2) say, in order. For a modelled block: `Keep` up
to and including the two length fields, `Tile(layout, zl)`, then `Keep` of the end string. Any
other block is `Keep` of all of its record.

## 5. Tiles

### 5.1 Layouts

A tile inflates to `len` bytes, 256×256 pixels:

| `len` | layout | bytes | samples of a pixel `i` (`0 ≤ i < 65536`) |
|---|---|---|---|
| 327680 | colour | a plane of 65536, then 4 bytes a pixel | `t[i]`, then `t[65536 + 4i .. 65536 + 4i + 4]` |
| 65536 | one plane | a plane | `t[i]` |
| 131072 | two planes | two planes | `t[i]`, `t[65536 + i]` |

A *pixel* is its samples in that order, sample `s` the byte `s` of the pixel (5 for colour,
1 or 2). Sample 0 of colour is the alpha; samples 1, 2, 3 are B, G, R; sample 4 a fourth byte.
Pixels are in raster order, `i = 256y + x`.

## 6. Arithmetic coding

### 6.1 Adaptive bits

An adaptive bit holds `p` (16 bits, the probability of a 1 in 1/65536) and a count `c`, at
first `p = 32768`, `c = 0`. After coding bit `b`:
`p ← clamp(p + ((t − p) × RATE[c]) >> 16, 32, 65504)` with `t = 65535` for 1 and `0` for 0,
in signed 32-bit arithmetic (`>>` arithmetic, rounding down), and `RATE[c] = 131072 div
(2c + 3)`; then `c ← c + 1` while `c < 30`.

### 6.2 Coder

Carry-less binary arithmetic coding over 32 bits.

- **Decoder**: `x1 = 0`, `x2 = 2^32 − 1`, and `x` the first 4 bytes of the stream (BE). To
  decode a bit of probability `p` (of a 1): `xmid = x1 + ((x2 − x1) × p) >> 16` (64-bit
  product); the bit is 1 when `x ≤ xmid`, then `x2 = xmid`, else `x1 = xmid + 1`. Then while
  the top bytes of `x1` and `x2` are equal: `x1 ← x1 << 8`, `x2 ← (x2 << 8) | 255`,
  `x ← (x << 8) | the next byte` (all mod 2^32). Bytes past the end of the stream read as 0.
  Having read more than 4 bytes past the end, at the end of a batch, is invalid.
- **Encoder** (for reference): the same interval; on renormalisation it writes the top byte
  of `x2`; at the end it writes `(x1 >> 24) + 1`.

### 6.3 Codes of adaptive bits

- **gamma** of a value `v ≥ 0` with a set of bits `unary[0 .. 12]` and `low[k][j]`: with
  `x = v + 1` and `e = bitlen(x) − 1`, the bits `unary[k]` for `k = 0, 1, …` are 0 until
  `k = e`, which is 1; at `k = 11` no bit is coded and `k` stays 11. Then the `k` bits of `x`
  below its leading one, from the highest, bit `j` with `low[k][j]`. The value is
  `2^k + those bits − 1`.

## 7. The tile model

A batch's stream codes its tiles in order with one *state*, made anew for each batch:

- `kind[0 .. 2]`, `full[0 .. 8]`: adaptive bits; `length[0 .. 8]`, `place`: gamma sets;
- `copy[k][ctx]` for `k < 4`, `ctx < 128`, and `listed[0 .. 5]`: adaptive bits;
- `residual[t]` for `t < 320`: gamma sets;
- `last_full` (false) and the *list*, at first empty: up to 256 pixels, most recent first.

### 7.1 A tile

1. A bit with `kind[0]`: 1 means every byte of the tile is 0; the tile is done.
2. A bit with `kind[1]`: 1 means every pixel is one pixel `v`, coded as new (7.5) with no
   neighbours; the tile is done.
3. Otherwise the pixels in raster order, from `i = 0`, with `run = 0` and `broke = false`:
   - Where `x > 0` and not `broke`, a run may start (7.2).
   - Otherwise the pixel is coded (7.3), and `i ← i + 1`.

### 7.2 Runs

Let `w` be the pixel at `i − 1` and `left = 256 − x`. In row 0, `above = left` where `x > 1`
and the pixel at `i − 2` is `w`, else 0. In another row, `above` is the number of pixels from
`i − 256` on, up to `left` of them, equal to `w` before the first that is not. A run starts
where `above ≥ min(4, left)` and `above > 0`. Then, with
`t = 4 × (y > 0) + 2 × (above = left) + last_full`:

- a bit with `full[t]`: 1 means the run is `above` pixels;
- else its length `len` is gamma with `length[t]`, taken as `min(len, left)`.

`last_full ←` that bit. The run's pixels are `w`; `i` moves past them; `broke ←` (the length
is less than `left`); `run ← 0`. Where no run starts, the pixel is coded as in 7.3.

### 7.3 A pixel

Neighbours: `n` the pixel above (none in row 0); `w` the pixel to the left, or `n` where
`x = 0`; `nw` above left (where `x > 0`, `y > 0`); `ne` above right (where `y > 0`,
`x < 255`). Two neighbours are *equal* when both exist and are the same pixel. With `run`
taken in buckets `r` (0; 1–2; 3–15; 16 and more → 0, 1, 2, 3):

`ctx = (16 × broke + eq(w, n) + 2 eq(n, ne) + 4 eq(w, nw) + 8 eq(nw, n)) × 4 + r`.

Candidates: where `broke`, the pixel at `i − 1` counts as tried, and `broke ← false`. Then for
`k = 0, 1, 2, 3` the neighbours `w`, `n`, `ne`, `nw`: one that does not exist or equals one
tried is skipped; otherwise it is tried, a bit with `copy[k][ctx]`, 1 meaning the pixel is that
neighbour. On a 1: `run ← run + 1` where `k = 0`, else `run ← 0`; the pixel is done.

If no candidate is taken, `run ← 0`, and with `m` the number tried (0 to 4):

- If the list is not empty, a bit with `listed[m]`: 1 means the pixel is in the list at a
  place `j`, gamma with `place`, taken as `min(j, size − 1)`. The pixel is taken out of the
  list there.
- Otherwise (or on 0) the pixel is new (7.5), with the neighbours `w`, `n`, `nw`, `ne`; if it
  is in the list, it is taken out of it.

The pixel then goes to the front of the list, which keeps its first 256.

### 7.4 Contexts of samples

For a new pixel the neighbours stand in for each other: `a = w` (else `n`), `b = n` (else
`w`), `c = nw` (else `n`, else `w`), `d = ne` (else `n`, else `w`), and a missing one has
samples 0. The samples are coded in the order 0, 2, 1, 3, 4 for colour (alpha, G, B, R, the
fourth), 0 for one plane, 0, 1 for two planes; `slot` is the place in that order.

### 7.5 A new pixel

`transparent` is false and `g` none at first. For each sample `s` in order, from the samples
`a, b, c, d` of the neighbours' sample `s`:

- `pred = MED(a, b, c)`: `min(a, b)` where `c ≥ max(a, b)`, `max(a, b)` where
  `c ≤ min(a, b)`, else `a + b − c`.
- For colour samples (`s > 0` of colour) `s = 1` or `3` (B, R) with `g` known:
  `pred ← clamp(pred + g, 0, 255)` (not mod 256).
- `act = |a − c| + |b − c| + |d − b|` (as integers), and `cross = min(bitlen(|g|), 3)` for
  `s = 1, 3, 4` where `g` is known, else 0.
- `t = ((2 × slot + (colour sample and transparent)) × 8 + min(bitlen(act), 7)) × 4 + cross`.
- A value `z` is gamma with `residual[t]`, taken as `min(z, 255)`; the residual is
  `(z >> 1) XOR −(z AND 1)` as a signed 8-bit value `e` (the inverse of zigzag), and the sample
  is `pred + e` mod 256.
- After sample 0 of colour, `transparent ← (the sample is 0)`; after sample 2 of colour,
  `g ← e` (signed).

## 8. zlib streams

The zlib stream of a modelled tile is what zlib 1.3.1's `deflate` makes of the tile's bytes
(5.1) in one call with `Z_FINISH`, after `deflateInit2(level 1, Z_DEFLATED, windowBits 15,
memLevel 9, Z_DEFAULT_STRATEGY)`: the header `78 01`, the DEFLATE blocks (`deflate_fast`, its
blocks flushed when `lit_bufsize − 1` = 32767 symbols are buffered, each block stored, fixed or
dynamic as `_tr_flush_block` decides), and the Adler-32 of the bytes. aqr-flate's emulation of
that compressor (`aqr_flate::zlib`, used by `aqr_clip::zlib`) makes the same bytes.

## 9. Encoder

The reference encoder (`formula::encode`):

- Walks the original (section 4), every eligible block a candidate. A candidate is modelled
  where it inflates to `len` bytes and section 8 makes its zlib stream again exactly; the
  runs record which.
- `batch` is 64: batches are decoded as tasks of their own; the size costs next to nothing.
- Codes each batch with the model, the encoder's choices being:
  - a run's full bit is 1 where the pixels equal to `w` from `i` on, up to `left`, are exactly
    `above`; else it codes their number;
  - a candidate's bit is 1 for the first equal to the pixel;
  - a pixel in the list is coded by its place where the bits of `listed[m]` (1) and of the
    place cost no more, by the models' current probabilities, than those of `listed[m]` (0) and
    of the samples; else as new.
- The database: aqr-sqlite's formula embedded where it takes it, else its bytes as content of
  the format `sqlite`. The skeleton is content (aqr-compress looks into it for files).
- A file that is not a `.clip` file, or whose walk fails, has no formula.
