# The `xsv-recons` module

This document specifies the module `xsv-recons` of aqr-formula entry formulas. The module
rebuilds a delimited text table (CSV, TSV, the `;` and `|` variants, USV) from its fields, stored
column by column: text as it is, and numbers as integers with how they were printed. The
document is meant to be complete, with [aqr-num](../../crates/aqr-num/README.md) for the numbers: an
implementation of the module can be written from the two. Sections marked *(encoder)* describe
the choices this implementation makes when it builds formulas; the module does not depend on
them.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [Streams](#3-streams)
4. [Rebuilding the file](#4-rebuilding-the-file)
5. [Encoding](#5-encoding-encoder)

## 1. Conventions

- Integers are unsigned unless said otherwise. **varint**: unsigned LEB128, least significant
  group first, at most 10 bytes, of at most 64 bits. **svarint**: a varint of the zigzag form of
  a signed 64-bit integer (`(v << 1) ^ (v >> 63)`). A writer always uses the shortest form.
- `LF` is the byte 0x0A, `CR` 0x0D.

## 2. The module

- **Module**: name `xsv-recons`, version 1.
- **Params**:

  ```text
  u8      n        1 to 4
  n bytes delim    between two fields of a record
  u8      quote    the byte quoting fields, or 0 for none
  u8      m        0 to 4
  m bytes term     the end of a record; none (m = 0) for the end of a line
  varint  C        the columns, 1 to 4096
  ```

  Nothing else may follow.
- **Resources**: none.
- **Inputs**: seven, in order: `layout`, `raw`, `columns`, `types`, `text`, `nums` and `desc`
  (§3).
- **Output**: the file of the declared length (§4). Any inconsistency (a stream ending early or
  with bytes left, a value out of its range, a length other than the declared one) is an error.

## 3. Streams

The file is a sequence of *records*. A record is kept as it is, or is one or more *fields* with
an *end*. Field j of a record (from 0) is in column min(j, C − 1); a column's fields are those of
the records in order.

- **layout**: of each record, in order, a varint r:
  - r = 0: a record kept as it is, a varint `len` following: the next `len` bytes of `raw`.
  - else, a record of r >> 2 fields (at least one), and its end e = r & 3: 0 the terminator
    (`term`, or LF where there is none), 1 CR LF (only where there is no `term`), 2 nothing.
    e = 3, and e = 1 with a `term`, are errors.
- **raw**: the bytes of the records kept as they are.
- **columns**: of each of the C columns, in order:

  ```text
  u8      pred     0: its numbers as they are, 1: as differences
  varint  types    its bytes of `types`
  varint  text     its bytes of `text`
  varint  nums     its bytes of `nums`
  varint  desc     its bytes of `desc`
  ```

  Each of `types`, `text`, `nums` and `desc` is the parts of the columns one after the other, in
  the order of the columns, of the lengths given; they add up to the stream's length. `columns`
  has nothing after the last.
- **types**: of each field of a column, a byte t: bit 0 whether the field is quoted (an error
  where `quote` is 0), and its kind t >> 1: 0 text, 1 a number of the column's descriptor, 2 a
  number of a new descriptor. Other kinds are errors.
- **text**: of each text field of a column, its bytes (unquoted: within the quotes, a doubled
  quote made single), then a zero byte.
- **desc**: of each number of a new descriptor, the descriptor and scale:

  ```text
  u8       mode
  u8       f        flags
  svarint  s        the scale, -60 to 60
  ```

  (mode, f) must be valid (aqr-num's README, "Descriptor"). It is the column's descriptor from
  then on; a number of the column's descriptor before the first is an error.
- **nums**: of each number of a column, an svarint: its integer v where `pred` is 0, else v − p
  (wrapping, in 64 bits), p being the integer of the column's previous number (0 for the first).

## 4. Rebuilding the file

For each record of `layout`, in order: a record kept as it is is its bytes of `raw`. A record of
fields is its fields with `delim` between them, then its end: `term` (or LF where there is
none), CR LF, or nothing. A field is, of its column, the next byte of `types` and:

- if quoted, `quote` first and last;
- text: the next bytes of `text` up to a zero byte (which is not part of the field), every
  `quote` byte doubled where the field is quoted;
- a number: the integer v (from `nums`) at the scale s of the descriptor (mode, f), printed as
  aqr-num specifies (its README, "Specification").

The file is the records, one after the other. Every stream must be read to its end, every
column's parts too; a record that would make the file longer than declared is an error.

## 5. Encoding *(encoder)*

- **Dialect.** The first 64 KiB of the file are cut into records (up to 1,000; the last one,
  cut by the sample's end, left out) with each candidate: `,`, TAB, `;` and `|` between fields,
  each with `"` quoting and with none, records ending at a line's end; and, where the sample has
  them, USV (U+241F between fields, U+241E ending records, or U+241E and LF where 9 in 10 of
  them are followed by LF) and their ASCII forms (0x1F, 0x1E), without quoting. A candidate is
  kept where at least 9 in 10 of its records have the same number of fields, at least two, at
  most 1 in 50 of them are kept as they are, and there are at least 8 records (fewer only in a
  file of fewer). Of those, the one whose records most often have that number, then of the
  most fields, is taken.
- **Records.** A field starting with `quote` is quoted: it ends at a `quote` not followed by
  another, and must be followed by `delim`, the record's end, or the file's end. Any other field
  ends at the first `delim` or the record's end. Where there is no `term`, a record ends at LF
  (CR LF where a CR is before it). A record with a quoted field not closed or followed by other
  bytes, or a field holding a zero byte, is kept as it is, up to its line's end (its
  terminator's) or the file's end.
- **Files taken.** A file of at least 16 KiB whose dialect is found, of which records kept as
  they are make less than a tenth. C is the most fields of a record (at most 4096).
- **Numbers.** Fields that start with a digit, a sign or a point, of at most 40 bytes, are coded
  in the style aqr-num's `style::choose` picks for the column from its first 512 such fields
  (with fallbacks as `Style::encode` has them), where they print back to the same bytes; zero
  takes the scale of the column's previous number. Others are text.
- **Predictors.** Of a column, the one whose `nums` part zstd -1 compresses smaller.
- The streams are left to the archive to compress; `text` and `raw` are of `Kind::Text`.
