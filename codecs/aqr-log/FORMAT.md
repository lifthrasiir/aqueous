# The `log-recons` module

This document specifies the module `log-recons` of aqr-formula entry formulas. The module
rebuilds lines of text (logs, tables of numbers between spaces) from a *template* for each line
and the *fields* the template leaves out, stored column by column: words as they are, and
numbers as integers (with how they were printed, as [aqr-num](../../crates/aqr-num/README.md)
specifies, or as runs of digits). The document is meant to be complete, with aqr-num for the
whole numbers: an implementation of the module can be written from the two. Sections marked
*(encoder)* describe the choices this implementation makes when it builds formulas; the module
does not depend on them.

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
  Arithmetic on integers wraps in 64 bits.
- `LF` is the byte 0x0A. A *line* is the bytes up to a LF (not holding it); the last line of a
  file has no LF where the file does not end with one.
- The marks of templates: **digits** 0x01, **word** 0x02, **number** 0x03.

## 2. The module

- **Module**: name `log-recons`, version 1.
- **Params**:

  ```text
  u8      flags     0 or 1: bit 0 the last line has no LF
  varint  T         the templates, 0 to 65,536
  varint  C         the columns, 0 to 262,144
  varint  M         the most numbers of a template, at most C
  ```

  Nothing else may follow. T, C and M only lay the module's memory out; the module checks
  that they are what the streams hold (§3).
- **Resources**: none.
- **Inputs**: nine, in order: `layout`, `raw`, `templates`, `columns`, `text`, `nums`,
  `widths`, `types` and `desc` (§3).
- **Output**: the file of the declared length (§4). Any inconsistency (a stream ending early or
  with bytes left, a value out of its range, a length other than the declared one) is an error.

## 3. Streams

The file is a sequence of lines. A line is *kept as it is*, or has a *template*.

- **templates**: the templates, in order, each followed by a LF (so a template holds no LF); at
  most 64 MiB in all. The templates are numbered from 1. A template is the bytes of a line
  with its *fields* replaced by marks: **digits** for a run of digits, **word** for the whole
  of a token, **number** for the whole of a token that is a number. A template's *slots* are
  its marks, in order. Template number i has the slots of its marks, and each slot is a
  *column*; the columns are numbered from 0, those of the first template first. The columns of
  all the templates are C, T the templates and M the most slots of the marks digits or number
  in one template.
- **layout**: of each line, in order, a varint: 0 for a line kept as it is, else its template's
  number (at most T).
- **raw**: the lines kept as they are, each followed by a LF.
- **columns**: of each column, in order, by its mark:

  ```text
  word:            varint  the bytes of its part of `text`
  digits, number:  u8      pred      0: its integers as they are, 1: as changes from the
                                     column's previous one, 2: as changes from the line before
                   varint  nums      the bytes of its part of `nums`
  number only:     varint  desc      the bytes of its part of `desc`
  ```

  `columns` has nothing after the last. A part of a stream is the next bytes of it after the
  parts of the columns before: all of a stream is the parts of the columns, in order, and no
  byte is left over.
- **text**: of each word column, its words, each followed by a LF (the column's part is the
  words one after another; a word holds no LF).
- **nums**: of each digits or number column, the svarint r of each of its values, in order.
  The part must end where a varint does (its last byte under 0x80). Let n be the varints in
  the part; the column's parts of `widths` or `types` are n bytes.
- **widths**: of each digits column, a byte for each value: 0, or the number of digits it is
  printed in, padded with zeros (§4).
- **types**: of each number column, a byte for each value: 1 for a number of the descriptor of
  the column's previous number (an error for the first), 2 for a number of a descriptor given
  in `desc`.
- **desc**: of each descriptor of a column given (a 2 of its `types`), in order:

  ```text
  u8       mode
  u8       f        flags
  svarint  s        the scale, -60 to 60
  ```

  (mode, f) must be valid (aqr-num's README, "Descriptor"). It is the column's descriptor from
  then on.

The value of the k-th number (digits or number slot) of a line is the integer v from its
column's next r, by the column's `pred`: v = r (0); v = p + r (1) with p the column's previous
value (0 for the first); v = q + r (2) with q the value of the k-th number of the last line
that was not kept as it is (0 if it had fewer, or there was none). k counts from 0 over the
slots of the marks digits and number of the line's template. The value is the column's
previous value from then on, whatever its pred.

## 4. Rebuilding the file

Each line of `layout`, in order, is rebuilt and followed by a LF, except the last if `flags` bit
0 is set (an error if there are no lines then). A line kept as it is is the bytes of `raw` up
to its LF. A line of template i is the template's bytes with each mark replaced:

- **word**: the next word of the column's `text`, up to its LF;
- **digits**: the value v (0 to 10^18 − 1, else an error) in decimal; with the byte w of
  `widths` nonzero, left-padded with zeros to w digits (an error unless the digits of v are
  fewer than w, and w at most 18);
- **number**: the integer v at the scale s of the column's descriptor (mode, f) as
  `types` says (§3), printed as aqr-num specifies (its README, "Specification").

Every stream must be read to its end, every column's part too; a line that would make the file
longer than declared is an error, and so is a file shorter.

## 5. Encoding *(encoder)*

- **Taken.** A file of at least 16 KiB (`formula::MIN_LEN`) of at least 16 lines in its first
  64 KiB, of which at most 1 in 10 hold a mark byte (0x01 to 0x03); and, once made, where lines
  kept as they are are less than a tenth of its bytes.
- **Segments.** A file whose lines start past 64 MiB (`formula::SEGMENT`) from the first of a
  segment is cut there, at a line's start. Each segment is an application of the module, with
  params of its own (the last one has `flags` bit 0 where the file does not end with a LF);
  the nine streams are one each, the segments' parts one after another and each application's
  inputs slices of them, and the formula is a Concat of the applications. A file in one segment
  is one application over whole streams. Each segment is made on its own: the templates, the
  counts they depend on, the columns and their predictors start afresh, as the module does.
  The encoder goes over a segment's lines three times (counts, templates and samples, streams)
  so that its heap holds the segment's counts and the streams.
- **Lines.** A line holding a mark byte, or one that would give a template past the limits
  (T 65,536, C 262,144, the templates 64 MiB), is kept as it is. A line is cut into tokens at
  each single space (so empty tokens exist; a CR before the LF is in the last token).
- **Tokens.** The *form* of a token is the token with each run of digits replaced by one
  mark. In the order:
  1. a token that is a whole number (below) is a **number** slot;
  2. a token with a run of more than 18 digits, or whose form occurs fewer than
     R = max(64, L / 500) times in the segment (L its lines not kept as they are; the counts
     include numbers' tokens), is a **word** slot;
  3. else the token's bytes are in the template, each run of digits a **digits** slot.
- **Whole numbers.** A token of at most 40 bytes, starting with a digit, a sign or a point,
  that aqr-num's `fixed::parse` takes, or, if it holds only digits, `.`, `e`, `E`, `+` and `-`
  and at least a digit, that a style from `style::choose` on that token alone codes such that
  `aqr_num::put` prints the same bytes. A column's style is chosen by `style::choose` from its
  first 512 numbers' tokens; each token is coded in it (`Style::encode`, a zero taking the
  scale of the column's previous number) where that prints it back, else as it is a number
  alone. A change of descriptor is a 2 of `types`.
- **Predictors.** Of a column, the one whose `nums` part zstd -1 compresses smallest (ties
  to the lower; by length for a column of 8 values or fewer).
- **Order.** Templates are numbered in the order of their first lines. The streams are left to
  the archive to compress; `raw`, `templates` and `text` are of `Kind::Text`.
