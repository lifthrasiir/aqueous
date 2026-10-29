# aqr-infer-recs format

This document specifies the modules `recs-fields` and `recs-entries` of aqr-formula entry
formulas, which rebuild records from the byte planes of their fields and entries from their
heads and tails, and how the encoder takes a file of unknown format apart around them. It is
meant to be complete: an implementation of the modules can be written from it alone. Sections
marked *(encoder)* describe the choices this implementation makes when it builds formulas; the
modules do not depend on them.

`recs-fields` is a transpose of any records of fixed size; it does not depend on how they were
found, and other recompressors may apply it to records of their formats.

Contents:

1. [Conventions](#1-conventions)
2. [The formula](#2-the-formula-encoder) *(encoder)*
3. [`recs-fields`](#3-recs-fields)
4. [`recs-entries`](#4-recs-entries)

## 1. Conventions

- Integers are unsigned. Arithmetic is exact unless marked *wrapping*.
- **varint**: unsigned LEB128, least significant group first, at most 10 bytes. A writer
  always uses the shortest form.
- A *field* of `w` bytes (1, 2, 4 or 8) is a number: its bytes in order are its most
  significant first where it is big-endian, its least significant first where it is
  little-endian.
- `align_up(x, a)`: the least multiple of `a` not less than `x`.

## 2. The formula *(encoder)*

A file of 16 KiB or more that no recompressor takes, and in which a scan finds no file of a
format, is looked into for *regions*: tables of records of a fixed size, and chains of entries
each holding its length. The search is the inference of the crate (README); it is run only
where a quick estimate says the regions may save at least 5% of the file's size under zstd at
level 3 (48 windows of 8 KiB over the file, each transposed by the record size its bytes repeat
at most and split into fields, against the windows as they are). A region is taken where it
makes the file smaller under zstd at level 3, judged on its first MiB and 64 KiB around it.

The formula is the concatenation of the bytes before the first region, the first region's
application, the bytes up to the next region, and so on, and the bytes after the last: the
bytes between the regions are slices of the stream `recs/raw`, all of them one after another.

- A table of records is an application of `recs-fields` (§3) to a stream `recs/planes`.
- A chain of entries is an application of `recs-entries` (§4) to the heads, an application of
  `recs-fields` to a stream `recs/planes`, and the tails, a stream `recs/tails`.

The fields are chosen by dynamic programming over the record, on a sample of the records
(about 64 KiB of them, from four places), each field's cost the order-0 code length of its
planes under adaptive models; a record may instead be stored as its bytes transposed (each
byte a field of width 1, as it is), where that compresses smaller.

## 3. `recs-fields`

- **Module**: name `recs-fields`, version 1.
- **Params**: `varint N`, the records; `varint K`, the bytes of a record; `varint F`, the
  fields; then for each field, in order, `u8 c` and, where `c >> 3` is 3 or 4, `varint R`.
  `c & 3` is `log2 w` for the field's bytes `w`; `c & 4` tells it big-endian; `c >> 3` is how
  it is coded (below). The fields lie one after another from offset 0 and cover the record:
  their widths sum to `K`. `R + w` is at most the field's offset (`R` refers to bytes of
  earlier fields). `F` is at most `K`, and `K` is at least 1 where `N` is.
- **Resources**: none.
- **Inputs**: `planes`.
- **Output**: the records, `N × K` bytes (the declared length).

`planes` is, for each field in order, for each byte of its coded values from the least
significant, for each record in order, that byte: `N × K` bytes in all. The coded value `t` of
a field whose value is `v` in a record is, by `c >> 3`:

| `c >> 3` | `t` |
|---|---|
| 0 | `v` |
| 1 | `v − p` wrapping in `w` bytes, `p` the field's value in the record before (0 before the first) |
| 2 | `v xor p` |
| 3 | `v − u` wrapping in `w` bytes, `u` the `w` bytes at offset `R` of the same record read as a number of the field's endianness |
| 4 | `v xor u` |

A decoder rebuilds the fields in order, so that the bytes a field refers to are known.

## 4. `recs-entries`

- **Module**: name `recs-entries`, version 1.
- **Params**: `varint N`, the entries; `varint H`, the bytes of a head; `varint kind`, `varint
  d`, `varint b`, `varint a`: the entries' lengths (below). `kind` is below 9, `a` from 1 to
  4096, and `d + w` at most `H` for the width `w` of `kind`.
- **Resources**: none.
- **Inputs**: `heads`, `tails`.
- **Output**: the entries, `N × H` bytes and the tails' (the declared length).

`heads` is `N × H` bytes, the entries' first bytes in order. An entry's length is
`align_up(b + v, a)`, where `v` is the field at offset `d` of its head of kind `kind`: a
number of the width and endianness below, its value masked:

| `kind` | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 |
|---|---|---|---|---|---|---|---|---|---|
| width | 1 | 2 | 2 | 2 | 2 | 2 | 2 | 4 | 4 |
| endianness | | little | little | little | big | big | big | little | big |
| mask | `FF` | `FFFF` | `7FFF` | `0FFF` | `FFFF` | `7FFF` | `0FFF` | `FFFFFFFF` | `FFFFFFFF` |

Each entry is its head followed by the next `length − H` bytes of `tails` (taken in order);
the length is at least `H`. `tails` is used up exactly.
