# The `jsonl-recons` module

This document specifies the module `jsonl-recons` of aqr-formula entry formulas. The module
rebuilds JSON lines (one JSON value a line, or the lines of a JSON array, an element and a comma
each) from their values, stored by key path: strings as they are written, numbers as integers with
how they were printed, and the structure of the objects and arrays apart. The document is meant
to be complete, with [aqr-num](../../crates/aqr-num/README.md) for the numbers: an implementation
of the module can be written from the two. Sections marked *(encoder)* describe the choices this
implementation makes when it builds formulas; the module does not depend on them.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [Streams](#3-streams)
4. [Paths](#4-paths)
5. [Rebuilding the file](#5-rebuilding-the-file)
6. [Encoding](#6-encoding-encoder)

## 1. Conventions

- Integers are unsigned unless said otherwise. **varint**: unsigned LEB128, least significant
  group first, at most 10 bytes, of at most 64 bits. **svarint**: a varint of the zigzag form of
  a signed 64-bit integer (`(v << 1) ^ (v >> 63)`). A writer always uses the shortest form.
- `LF` is the byte 0x0A, `CR` 0x0D, `SP` 0x20.
- A **string** of a stream is its bytes up to LF; the LF is taken from the stream but is not
  part of the string. (No string of a line holds one, and the streams of strings are text.)
- **Bits** of k bits are ⌈k / 8⌉ bytes; bit i is bit i mod 8 (the least significant first) of
  byte ⌊i / 8⌋. Bits that no rule below reads may be anything.

## 2. The module

- **Module**: name `jsonl-recons`, version 1.
- **Params**: one byte, the **style**: bit 0 whether a comma is followed by SP, bit 1 whether a
  colon is. Other values are errors; nothing may follow.
- **Resources**: none.
- **Inputs**: ten, in order: `layout`, `raw`, `paths`, `keys`, `types`, `shapes`, `names`,
  `text`, `nums` and `desc` (§3).
- **Output**: the file of the declared length (§5). Any inconsistency (a stream ending early or
  with bytes left, a value out of its range, a length other than the declared one) is an error.

## 3. Streams

The file is a sequence of *lines*. A line is kept as it is, or is a JSON *value* with an end.

- **layout**: of each line, in order, a varint r:
  - r = 0: a line kept as it is, a varint `len` following: the next `len` bytes of `raw`.
  - else a value, at the root path (§4), and its end e = r − 1: bits 0–1 are LF (0), CR LF
    (1) or nothing (2; the last line of a file), and bit 2 tells a comma before it. Ends of
    bits 0–1 of 3, and e > 7, are errors.
- **raw**: the bytes of the lines kept as they are (their ends too).
- **paths**: the table of paths (§4): of each path, in order,

  ```text
  u8      flags    bit 0: a map, bit 1: its numbers as differences, bit 2: its strings shared
  varint  width    1 to 64
  varint  types    its bytes of `types`
  varint  shapes   its bytes of `shapes`
  varint  names    its bytes of `names`
  varint  text     its bytes of `text`
  varint  nums     its bytes of `nums`
  varint  desc     its bytes of `desc`
  ```

  Other flags are errors; there are at most 2^20 paths. Each of `types`, `shapes`, `names`,
  `nums` and `desc` is the parts of the paths one after the other, in the order of the table, of
  the lengths given; they add up to the stream's length. So is `text`, but for what follows the
  paths' parts: the *shared* part.
- **keys**: the keys of the key lists (§5), each a string, in the order they are defined.
- **types**: of each value of a path, a byte t: 0 `null`, 1 `false`, 2 `true`, 3 a string, 4 a
  number of the path's descriptor, 5 a number of a new descriptor, 6 a number as it is written,
  7 an object, 8 an array. Other bytes are errors.
- **shapes**: of each object and array of a path, its shape (§5): varints and bits.
- **names**: of each member of an object of a map path, its key, a string.
- **text**: of each string of a path, its bytes between the quotes, and of each number as it
  is written, its bytes: a string each, in the path's part, or where the path's flags have bit
  2, in the shared part (the strings of all such paths, in the order of the file).
- **desc**: of each number of a new descriptor, the descriptor and scale:

  ```text
  u8       mode
  u8       f        flags
  svarint  s        the scale, -60 to 60
  ```

  (mode, f) must be valid (aqr-num's README, "Descriptor"). It is the path's descriptor from
  then on; a number of the path's descriptor before the first is an error.
- **nums**: of each number of a path of a descriptor, an svarint: its integer v, or where the
  path's flags have bit 1, v − p (wrapping, in 64 bits), p being the integer of the path's
  previous such number (0 for the first).

## 4. Paths

Every value is at a *path*, and takes the next bytes of the path's parts of the streams. Paths
are numbered from 0 in the order they are *made*; the n-th path made is the n-th of the table,
whose flags and width it has. Making a path when every path of the table is made is an error;
at the end, every path of the table must be made.

- The root path, of the lines' values, is made as the first value line is rebuilt.
- A path p has *children*, each found the first time it is needed and made then:
  - of a key k (the members of key k of p's objects, p not a map): found by (p, k), where key
    lists are defined (§5);
  - the members of p's objects, p a map: one child, needed by the first member of an object;
  - element j of p's arrays (from 0): child min(j, width − 1), needed by element j of an
    array. (Children j are needed and made in order.)

## 5. Rebuilding the file

For each line of `layout`, in order: a line kept as it is is its bytes of `raw`. A value line is
its value at the root path at depth 0, then a comma where its end tells, then its end: LF, CR LF,
or nothing.

A value at path p, at depth d, is, by the next byte of p's `types`:

- `null`, `false`, `true`: those bytes;
- a string: `"`, the next string of p's `text` (or of the shared part), `"`;
- a number as it is written: the next string of p's `text` (or of the shared part);
- a number: the integer v (from p's `nums`) at the scale s of the descriptor (mode, f), printed
  as aqr-num specifies (its README, "Specification");
- an object or an array: as below, its members or elements at depth d + 1. One at depth 64 or
  more is an error.

Commas and colons are those of the style, or the *other*, where a separator's bit tells: a comma
followed by SP where the style's is not, and the reverse; the same of colons. Bits b of a shape
of n members or elements are the bits after a varint m (n = m >> 1): ⌈(2n + 2) / 8⌉ bytes of an
object's, ⌈(n + 2) / 8⌉ of an array's, where m has bit 0; else none, all bits 0. Of an object or
array:

- bit 0: SP after the `{` or `[`;
- bit 1: SP before the `}` or `]` (none where n = 0).

**Arrays.** A shape of p's `shapes`: m and bits b. The array is `[`, its elements with commas
between them, `]`; element j is a value at p's child j (§4), and the comma before it the other
where bit j + 2 of b is set.

**Objects of a map.** Where p's flags have bit 0, a shape of p's `shapes`: m and bits b. The
object is `{`, its members with commas between them, `}`; member i is `"`, its key (the next
string of p's `names`), `"`, a colon, and a value at p's child of the members. The colon is the
other where bit 2i + 2 of b is set; the comma before it, where bit 2i + 3 is.

**Objects.** Else, a *key list* of p: p keeps the key lists defined at it, the most recent first.
A varint s of p's `shapes`:

- s = 0: a key list defined: m and bits b of p's `shapes`, then n keys, the next strings of
  `keys`. The child of each key is found (or made), in order. The list becomes p's most recent;
  where p then keeps more than 1024 lists, the least recent is dropped.
- s ≥ 1: the s-th most recent list of p (an error where there is none), which becomes its most
  recent.

The object is `{`, its members with commas between them, `}`; member i is `"`, the list's key i,
`"`, a colon, and a value at the key's child, its separators by the list's bits b as those of a
map's.

The file is the lines, one after the other. Every stream must be read to its end, every path's
parts too; a line that would make the file longer than declared is an error.

## 6. Encoding *(encoder)*

- **Lines.** A line ends at LF (CR LF where a CR is before it), or at the end of the file. A line
  is a value where it is a JSON value (RFC 8259) and nothing else but a comma at its end, written
  with no whitespace but one SP after a comma, a colon, a `{` or a `[`, or before a `}` or `]`
  (its strings' escapes are not looked at); it nests less than 64 deep. Any other line is kept
  as it is.
- **Style.** The first 64 KiB of the file, or its first 8 lines where they are longer (up to
  4 MiB), are cut into lines (up to 1,000; a last line cut by the sample's end left out). The file
  is taken where at least 9 lines in 10 of them are objects or arrays, of 8 lines at least (2 in a
  file of fewer). The style's comma and colon are those most of their commas and colons are.
- **Files taken.** A file of at least 16 KiB whose style is found, of which lines kept as they are
  make less than a tenth, of at most 2^20 paths in a segment.
- **Segments.** The file is cut, at the ends of lines, into segments of 64 MiB at least (the last
  shorter, and one of a longer line longer), each an application of the module of its own: the
  formula is their concatenation, each holding its streams (of the same classes, which the archive
  puts one after the other). What the encoder and the module hold of the file at once is a
  segment; the first pass is of all of it.
- **Paths.** A first pass over the file tells, of each path:
  - a map: once it has had 32 objects, where at least 16 keys are met, used fewer than 4 times
    each on average, or more than 4,096 keys (keys that are data: ids, hashes, words). Its members
    after that are in one child.
  - its width: where 9 in 10 of its arrays (of 8 at least) are of one length from 2 to 63, that
    length (tuples, such as coordinates); else 1.
  - its numbers' style, which aqr-num's `style::choose` picks from its first 512 numbers.
  - whether its strings are shared: where strings of a record repeat each other (the URLs of
    GitHub's events) rather than those of the path. Of the first 4 MiB of strings of the file
    (64 KiB at least), the strings of each of the 32 paths of the most bytes in turn, from the
    most, and before them those of the others together, are made shared where that makes the
    strings smaller through zstd -3 (the paths' parts, then the shared part).

  It keeps of each path 4,096 key lists at most, and of the strings 4 MiB (each counted 16 bytes
  longer), so that it holds what it is told by bounds of its own. The second pass writes the
  streams of each segment with them; a path made in the second pass has those of the path it is
  of in the first (none where that was not seen: not a map, width 1).
- **Numbers.** Each in its path's style (with fallbacks as `Style::encode` has them), where it
  prints back to the same bytes; zero takes the scale of the path's previous number. Others are
  numbers as they are written.
- **Key lists.** Of an object of a path not a map, the list of its keys and separators most
  recently defined or recalled at the path if it is one of the 1,024 the path keeps, else defined.
- **Predictors.** Of a path, the one whose `nums` part zstd -1 compresses smaller.
- The streams are left to the archive to compress; `raw`, `keys`, `names` and `text` are of
  `Kind::Text` and contents (aqr-compress recompresses them in turn, as text: with one aqr-wrt
  dictionary of all the segments' text, where there are several).
