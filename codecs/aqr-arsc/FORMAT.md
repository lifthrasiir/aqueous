# The `arsc-recons` module

This document specifies the module `arsc-recons` of aqr-formula entry formulas. The module
rebuilds an Android resource table (`resources.arsc`) from streams: its chunks' headers, the
strings of its string pools without their offsets and lengths, the entries of its type chunks
by field with their offsets derived and their keys and values predicted by the
configurations before, and any other chunk as it is. The document is meant to be complete: an
implementation of the module can be written from it alone. Sections marked *(encoder)*
describe the choices this implementation makes when it builds formulas; the module does not
depend on them.

A resource table is a tree of chunks, each a header (`u16 type`, `u16 headerSize`, `u32
size`, then the rest of its header) and a body: the table (`0x0002`), holding string pools
(`0x0001`) and packages (`0x0200`), which hold string pools, type specs (`0x0202`) and types
(`0x0201`). Nothing of it is needed to decode beyond what this document says.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [Decoding](#3-decoding)
4. [String pools](#4-string-pools)
5. [Types](#5-types)
6. [Encoding](#6-encoding-encoder)

## 1. Conventions

- Integers are unsigned and little-endian: `u16`, `u32`. **varint**: unsigned LEB128, least
  significant group first, at most 10 bytes, read as a 64-bit value.
- `unzigzag(z) = (z >> 1) XOR −(z & 1)`. A value **moved** from `prev` by `z` is `prev +
  unzigzag(z)`, which must lie within `u32`.
- Any read past the end of a stream, any value out of its range, a stream not read to its end
  and any write past where it must end make the module fail.

## 2. The module

- **Module**: name `arsc-recons`, version 1.
- **Params**: none (zero bytes).
- **Resources**: none.
- **Inputs**: exactly 6, each of class `arsc/NAME`, which hold the streams below: input `k`
  is the varints of the lengths of all its streams but the last, then its streams one after
  another, the last taking what is left.
- **Output**: the file, `len` bytes long (the `len` the application declares).

| # | input | its streams |
|---|---|---|
| 0 | `head` | `head`, `raw`, `style`, `text16` |
| 1 | `text` | `text` |
| 2 | `entries` | `present`, `flags`, `key` |
| 3 | `values` | `vtype`, `vstr` |
| 4 | `data` | `vdata` |
| 5 | `maps` | `map` |

| stream | contents |
|---|---|
| `head` | the chunks' headers, each followed by its mode (§3) |
| `raw` | the bodies of chunks kept as they are |
| `text`, `text16` | the strings of UTF-8 and UTF-16 pools (§4) |
| `style` | the pools' style offsets and styles (§4) |
| `present`, `flags`, `key`, `vtype`, `vstr`, `vdata`, `map` | the types' entries (§5) |

## 3. Decoding

`out` is empty. While it is shorter than `len`, a chunk is appended to it, which must not
take it past `len`; then it must be `len` bytes long, and every stream at its end.

A **chunk**, which must end by `end` (`len` at the top, its container's end below): from
`head`, 8 bytes, its `type`, `headerSize` (at least 8) and `size` (at least `headerSize`,
its start plus `size` not past `end`), then `headerSize − 8` bytes; append them. Then a byte
of `head`, its mode:

- 0: `size − headerSize` bytes of `raw`;
- 1: chunks, one after another, until its start plus `size`;
- 2, where `type` is `0x0001`: a string pool's body (§4);
- 3, where `type` is `0x0201`: a type's body (§5);
- any other: the module fails.

The chunk must then end at its start plus `size`.

## 4. String pools

The header, of `headerSize` at least 28, has at 8 `u32` the count `n` of strings (above 0),
at 12 the count `m` of styles, at 16 the flags (bit `0x100`: UTF-8), at 20 the start of the
strings and at 24 that of the styles, both from the chunk's start; the strings must start at
`headerSize + 4 × (n + m)`, and where `m > 0`, the styles not before them.

The body: `n` offsets (`u32`), set below; `4 × m` bytes of `style`; then the strings, string
`i`'s offset (from the strings' start) where it starts:

- UTF-8: the bytes of `text` up to its next zero byte, past it; written as its length in
  UTF-16 units (its bytes `b` with `b & 0xC0 ≠ 0x80`, and one more for each `b ≥ 0xF0`), its
  length in bytes, each one byte below `0x80` else two (`0x80 | n >> 8`, `n & 0xFF`; below
  `0x8000`), the bytes, a zero byte;
- UTF-16: the units (`u16`) of `text16` up to its next zero unit, past it; written as their
  count, one unit below `0x8000` else two (`0x8000 | n >> 16`, `n & 0xFFFF`), the units, a
  zero unit.

Then zero bytes up to the styles' start where `m > 0`, else up to the chunk's end; then
bytes of `style` up to the chunk's end.

## 5. Types

What earlier entries predict: for each type id `t` and index `i`, a **key**, a **value type**
and a **value**, all 0 at first.

The header, of `headerSize` at least 20, has at 8 the type id `t` and at 9 its flags, which
must be 0, at 12 `u32` the count `n` of entries and at 16 the start of the entries, which
must be `headerSize + 4 × n`. The body: `n` offsets (`u32`), then the entries. For each
`i < n`, a byte of `present`: 0, offset `0xFFFFFFFF`, no entry; 1, the offset is where the
entry starts (from the entries' start), and the entry:

- its flags, a varint of `flags` below 2^16; complex where bit 0 is set;
- its key: a varint `k` of `key`: 0, the predicted key; else `k − 1`, below 2^32;
- written: `u16` 16 where complex, else 8; the flags (`u16`); the key (`u32`);
- complex: 4 bytes of `map` (the parent); a varint `c` of `flags`, below 2^32, written as
  `u32`; `12 × c` bytes of `map`. The predicted key becomes the key;
- else, its value type `vt`, a byte of `vtype`; its data: for `vt = 0x03` (a string's index),
  moved from the last string index (0 at first, over all types) by a varint of `vstr`, and
  then the last; else a varint `k` of `vdata`: 0, the predicted value, where the predicted
  value type is `vt`; else `k − 1`, below 2^32. Written: `8, 0, 0, vt`, the data (`u32`). The
  key, `vt` and the data become the predicted ones.

## 6. Encoding *(encoder)*

`aqr_arsc::encode::split` takes files that start with a table's header (`0x0002`, header of
12 bytes) whose size is their length. Each chunk is taken where it is laid out as §3 to §5
rebuild it, else kept (mode 0): the table and packages where their bodies are chunks; string
pools where their strings follow each other from the first offset, with the lengths in their
shortest form and no zero byte (unit) within, the padding is zeros; types where the offsets
follow the entries one after another, no entry is compact (flags bit 3) and every entry and
value has its usual size. The streams are decoded again, and must give the file.

Why this layout (measured on the `resources.arsc` of 7 F-Droid APKs, 44.7 MB;
`aqr-arsc-check`, zstd -9): the files 10.60 MB, the streams 6.76 MB (−36.2%; xz -9e takes the
files to 7.75 MB). The strings 5.53 MB, without their lengths (their data with them, 6.66
MB) or offsets (1.63 MB); the entries 0.12 MB of 3.2 MB (their offsets 0.23 MB, the entries
1.85 MB, under zstd as they are): the keys of nine entries in ten are those of the
configuration before. The string indices cost 0.94 MB: the pool is sorted by configuration
and value, not in the order the entries refer to its strings.
