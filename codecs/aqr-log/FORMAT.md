# The `log-recons` module

This document specifies the module `log-recons` of aqr-formula entry formulas. The module
rebuilds lines of text (logs, tables of numbers between spaces) from a *template* for each line
and the *fields* the template leaves out, stored column by column: words as they are, and
numbers as integers (with how they were printed, as [aqr-num](../../crates/aqr-num/README.md)
specifies, or as runs of digits), and timestamps as integers by a pattern. The document is meant to be complete, with aqr-num for the
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
- The marks of templates: **digits** 0x01, **word** 0x02, **number** 0x03, **time** 0x04, **ip** 0x05.

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
  of a token, **number** for the whole of a token that is a number, **ip** for an IPv4 address,
  and a time as the mark time, its pattern and the mark again. A template's *slots* are its
  marks, in order. Template number i has the slots of its marks, and each slot is a
  *column*; the columns are numbered from 0, those of the first template first. The columns of
  all the templates are C, T the templates and M the most slots of the marks other than word
  (digits, number, time, ip) in one template.
- **layout**: of each line, in order, a varint: 0 for a line kept as it is, else its template's
  number (at most T).
- **raw**: the lines kept as they are, each followed by a LF.
- **columns**: of each column, in order, by its mark:

  ```text
  word:            varint  the bytes of its part of `text`
  digits, number, time, ip (each of its columns):
                   u8      pred      0: its integers as they are, 1: as changes from the
                                     column's previous one, 2: as changes from the line before,
                                     3 (ip only): as indexes into the recent addresses (below)
                   varint  unit      what the stored integers are multiplied by: 1 to 2^63 - 1
                   varint  nums      the bytes of its part of `nums`
  number only:     varint  desc      the bytes of its part of `desc`
  ```

  `columns` has nothing after the last. A part of a stream is the next bytes of it after the
  parts of the columns before: all of a stream is the parts of the columns, in order, and no
  byte is left over.
- **text**: of each word column, its words, each followed by a LF (the column's part is the
  words one after another; a word holds no LF).
- **nums**: of each digits, number or time column, the svarint of each of its values, in order;
  r is the integer it holds times the column's `unit` (an error if it does not fit 64 bits;
  the unit of the predictor 3 is 1, and its integers are varints, not svarints).
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

The value of the k-th number (a column of a digits, number or time slot) of a line is the integer v from its
column's next r (the stored integer times `unit`), by the column's `pred`: v = r (0); v = p + r (1) with p the column's previous
value (0 for the first); v = q + r (2) with q the value of the k-th number of the last line
that was not kept as it is (0 if it had fewer, or there was none). k counts from 0 over the
columns of the marks digits, number and time of the line's template. The value is the column's
previous value from then on, whatever its pred.

The k-th number of an ip column is an address: a value from 0 to 2^32 - 1 (an error otherwise).
By the predictor 3 its integer is a varint s, taken as an index into the *recent addresses* (the
address at s, an error if there are not so many) where s < 64, else the address s - 64 (an error
over 2^32 - 1). The recent addresses are a list of at most 64 (`RECENT`), shared by all the ip
columns of the segment and starting empty, whatever their predictors: each address of a line, in
the order of the slots, goes to the front of the list (from where it was in it, else the last
one dropping out of a full list). The value is taken as the column's previous and the line's
k-th as for any number, at scale 0.

A number has a *scale*: that of its descriptor (for a number slot, whose descriptor is
read before the value), 0 for a digit run. p and q are taken at the scale of the number
they predict: p as `rescale(p, a, b)` with a the scale of the column's previous number
(0 before the first) and b that of this one, and q as `rescale(q, c, b)` with c the scale
q had in its line. `rescale(x, a, b)` is aqr-num's `fixed::rescale`: x × 10^min(b − a, 18),
wrapping in 64 bits, where b ≥ a, else x divided by 10^min(a − b, 18), truncated toward zero.
(The scale is that of a descriptor of padded fixed or of `%g`, which vary from number to
number, so that a change of scale does not spoil the prediction.)

## 4. Rebuilding the file

Each line of `layout`, in order, is rebuilt and followed by a LF, except the last if `flags` bit
0 is set (an error if there are no lines then). A line kept as it is is the bytes of `raw` up
to its LF. A line of template i is the template's bytes with each mark replaced:

- **word**: the next word of the column's `text`, up to its LF;
- **digits**: the value v (0 to 10^18 − 1, else an error) in decimal; with the byte w of
  `widths` nonzero, left-padded with zeros to w digits (an error unless the digits of v are
  fewer than w, and w at most 18);
- **time**: the integer v (the first column's, the second's f for a pattern with a fraction of n
  digits: an error unless 0 ≤ f < 10^n, then v × 10^n + f, an error if it does not fit 64 bits)
  printed as the pattern says (aqr-num `time::put`; an error if it cannot print v). A time's
  columns have a scale of 0.
- **ip**: the address v as four decimal octets (v's bytes, most significant first, without
  leading zeros) between points.
- **number**: the integer v at the scale s of the column's descriptor (mode, f) as
  `types` says (§3), printed as aqr-num specifies (its README, "Specification"; the modes
  include its decimal comma, padded fixed and digit groups).

Every stream must be read to its end, every column's part too; a line that would make the file
longer than declared is an error, and so is a file shorter.

## 5. Encoding *(encoder)*

- **Taken.** A file of at least 16 KiB (`formula::MIN_LEN`) of at least 16 lines in its first
  64 KiB, of which at most 1 in 10 hold a mark byte (0x01 to 0x04); and, once made, where lines
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
- **Lines.** A line holding a mark byte (0x01 to 0x04), or one that would give a template past
  the limits (T 65,536, C 262,144, the templates 64 MiB), is kept as it is. A line is searched
  for *timestamps* (**Times**, below), each a slot of its own: the time mark, its pattern and the
  mark. What is left is cut into tokens at the *separators*, which stay in the template as
  they are (so empty tokens exist; a CR before the LF is not: the line is cut without it, and the template ends with the byte, so the last token of a CRLF line is a clean one; a line of the other ending has a template of its own); a timestamp is
  not cut where it holds a separator, and a token round it is cut into the parts before and
  after (each a token of its own).
- **Separators.** Either a space only, or *wide*: a space, `;` and TAB, and `,` unless the
  segment's numbers have a decimal comma. The segment takes the wide ones where the streams of
  its first MiB (cut at a line's end), made both ways and compressed by zstd -3, are no larger
  with them than without (text of words and code, whose commas and semicolons are the
  punctuation of lines of every shape, comes out smaller with spaces only). The decimal comma is
  told from the first 256 KiB of the segment by tokens, cut at spaces, `;` and TABs only: a
  token is a *point number* if it is an optional sign, digits, a point and at least a digit,
  then an optional exponent (`e` or `E` and at least one more byte); a *comma number* is the
  same with a comma. A comma number of exactly three digits after the comma and not more than
  three digits before it, the first of them not a zero, with no exponent (`1,234`), may be a
  thousands grouping: it is counted for that; the other comma numbers (`0,418`, `18,5`,
  `1234,567`, `1,5e3`) are counted for the decimal comma. The numbers have a decimal comma if
  the comma numbers (not those) outnumber the point numbers and are at least as many as the
  groupings. A token that is a comma number with groups of three digits between points before
  the comma (`1.187,25`, `-3.456.789,0`: one to three digits, then groups of three after a
  point) counts for the decimal comma too, where the first count above would have skipped it.
  (In a comma-separated table the tokens are lines or fields with several commas,
  none of them a comma number; in a `;`-separated table of a European locale, `4,216` is one.)
  A token with a point is coded as a number also where the locale has a comma, and one with
  both is a number where its groups are those aqr-num prints (`1.187,25`; an encoder's choice
  of the column's style: aqr-num's README, "Encoder conventions"), else not.
- **Times.** A timestamp starts at a digit or a capital letter that follows no letter, digit,
  `.`, `:`, `_` or `+`, and ends before no letter or digit (but a `Z` that ends a word), `_`
  or `:`, nor before the digits of a fraction after a point or comma. It is one of these
  (the pattern letters of aqr-num; first the longest that prints back the same bytes by
  `time::put`, `time::parse` having read it):
  - dates `Y-m-d`, `Y/m/d`, `Y.m.d` (4 digits and a separator: ISO 8601), `d/m/Y`, `d.m.Y`,
    `d-m-Y` (2 digits and a separator, `m/d/Y` and so on where the segment's `dd/dd/yyyy`
    dates have a second field over 12 and none a first), `d/b/Y`, `d-b-Y`, `d.b.Y` (a month
    name), followed by nothing, or a space, `T` (the pattern's `\T`) or `:` and a time of day;
  - a time of day `H:M:S` with the fraction the segment writes (below), `H:M:S` with the
    digits of the fraction in the text (fixed), `H:M:S`, or `H:M`; alone, with an hour of
    one digit (`h`) too;
  - syslog's `b e H:M:S` (and `b D H:M:S`) and ctime's `a b d H:M:S Y` (and with `e`), and RFC
    5322's `a, d b Y H:M:S` (a weekday and a date: the date must have it).
  A zone after a time (` -0400`, `-0400`, `+02:00`, `Z`: a sign and an offset `hh:mm` or `hhmm`
  of at most 14 hours and a multiple of 15 minutes, with a space before the sign or not, or `Z`, at
  the end of a token) is part of the timestamp, as literal bytes at the end of its pattern
  (`d/b/Y:H:M:S -0400`, `Y-m-d\TH:M:S\Z`: aqr-num's README, "Zones"): the time stays local, as
  printed, and an offset that differs is a pattern of its own; where the conditions do not hold (a comma and a digit after it, say) the zone is template text as before. The fraction a segment writes is told from the first 256 KiB: of the `H:M:S` followed
  by a point or a comma and 1 to 9 digits, those digits' counts: one count, always that many
  digits (`f`); several, a trimmed fraction of the most (`t`); none, no fraction; the point is
  the likelier of `.` and `,`. A time of another fraction than the segment's is of its own
  fixed fraction.
- **Tokens.** The *form* of a token is the token with each run of digits replaced by one
  mark. In the order:
  1. a token that is an IPv4 address (below) is an **ip** slot;
  2. a token that is a whole number (below) is a **number** slot;
  3. a token with a run of more than 18 digits, or whose form occurs fewer than
     R = max(64, L / 500) times in the segment (L its lines not kept as they are; the counts
     include numbers' tokens), is a **word** slot;
  4. else the token's bytes are in the template, each run of digits a **digits** slot, and each
     IPv4 address in it an **ip** slot.

  The *form* of a token has each IPv4 address in it as one mark of its own, apart from the
  digit runs'. An *IPv4 address* in a token starts at a digit that follows no digit or point,
  and is four octets between points, each of one to three digits (no zero first, but the digit
  itself), at most 255, and ends before no digit, nor a point and a digit (so `1.2.3.4.5`,
  `01.2.3.4` and `1.2.3` are none, `(1.2.3.4)`, `ip=1.2.3.4` and `1.2.3.4.` are, and a version
  `1.2.3.4` is).
- **Whole numbers.** A token of at most 40 bytes, starting with a digit, a sign or a point,
  that aqr-num's `fixed::parse` takes (with its comma as a point where the token has one, and
  no point: fixed in the comma mode), or, if it holds only digits, `.`, `,`, `e`, `E`, `+` and
  `-` and at least a digit, that a style from `style::choose_with` on that token alone codes such that
  `aqr_num::put` prints the same bytes. A column's style is chosen by `style::choose_with` (padded fixed allowed) from its
  first 512 numbers' tokens (of the first 1024 where padded fixed is chosen, which is kept
  only if they, coded as the column's numbers, with their descriptors, compress by zstd -1 to
  fewer bytes than coded in the style without it), in the comma modes where its tokens have commas; each token is coded in it (`Style::encode`, a zero taking the
  scale of the column's previous number) where that prints it back, else as it is a number
  alone. A change of descriptor is a 2 of `types`.
- **Predictors.** Of a column, the one whose `nums` part zstd -1 compresses smallest (ties
  to the lower; by length for a column of 8 values or fewer), as it is (`unit` 1) or, where the
  predictor's integers have a greatest common divisor `g` of at least 2 and fitting 63 bits,
  divided by it (`unit` g; not where the quotients are all the same byte, which cost nothing
  as they are, nor, for 8 values or fewer, where the unit's bytes outweigh theirs; ties to the
  larger unit). A column whose integers are all multiples of one (a measure in quarters,
  `16.195,25` of the SMARD tables, a multiple of 25 at its scale; times of a quarter of an
  hour) stores the quotients, so the part holds what varies. An ip column has the predictor 3 too, its indexes
  into the recent addresses, where the addresses of a log recur across its templates (80% of those
  of OpenSSH's log are the last one seen, 99% one of the last four); it is taken like the others,
  but not divided by a unit. A time with a fraction puts its
  integer without the fraction (in the unit of the pattern: seconds) into its first column and
  the fraction into its second, so that the changes of the first are small and the second, whose
  digits are random where a clock reads microseconds, is not a part of them.
- **Order.** Templates are numbered in the order of their first lines. The streams are left to
  the archive to compress; `raw`, `templates` and `text` are of `Kind::Text`.
