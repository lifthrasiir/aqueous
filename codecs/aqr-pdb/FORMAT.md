# The aqr-pdb modules

This document specifies the modules of aqr-formula entry formulas that rebuild PDB files (the
MSF container of Microsoft's debugging information) from streams:
[`pdb-msf`](#2-the-pdb-msf-module), the container from its streams' data, its layout and the
rest of its blocks, and [`pdb-symbols`](#3-the-pdb-symbols-module), the CodeView symbol records
of the modules' streams from their fields, and [`pdb-types`](#4-the-pdb-types-module), the TPI
and IPI streams (the type records) with their hash streams. The document is meant to be complete: an
implementation of the modules can be written from it alone. Sections marked *(encoder)*
describe the choices this implementation makes when it builds formulas; the modules do not
depend on them.

Contents:

1. [Conventions](#1-conventions)
2. [The `pdb-msf` module](#2-the-pdb-msf-module)
3. [The `pdb-symbols` module](#3-the-pdb-symbols-module)
4. [The `pdb-types` module](#4-the-pdb-types-module)

## 1. Conventions

- Integers are unsigned and little-endian: `u16`, `u32`. **varint**: unsigned LEB128, least
  significant group first, at most 10 bytes, read as a 64-bit value. `unzigzag(z) = (z >> 1)
  XOR −(z & 1)`.
- **Packed** streams: an input holding `n` streams is the varints of the lengths of the first
  `n − 1`, then the streams one after another, the last taking what is left.
- Any read past the end of a stream, any value out of its range, a stream not read to its end
  and an output not of the length the application declares make the module fail.

## 2. The `pdb-msf` module

An MSF file is a sequence of blocks of a size, a power of two. Block 0 holds the superblock:

```text
u8[32]  magic        "Microsoft C/C++ MSF 7.00\r\n\x1aDS\0\0\0"
u32     block_size
u32     fpm          the block of the free page map (1 or 2)
u32     blocks       the blocks of the file
u32     dir_bytes    the bytes of the directory
u32     unknown
u32     map          the block holding the list of the directory's blocks
```

The directory is `u32` the number of streams `n`, `n` × `u32` their sizes (`0xffffffff` for a
stream of none, of no blocks), then each stream's blocks (`⌈size / block_size⌉` × `u32`). A
stream's data is its blocks' bytes, one block after another, cut to its size.

- **Module**: name `pdb-msf`, version 1.
- **Params**: none (zero bytes).
- **Resources**: none.
- **Inputs**: exactly 3: `streams` (the streams' data, in the order below), `layout` and
  `rest`.
- **Output**: the file.

The **order** of the streams in `streams`: by their first blocks, streams of no blocks last,
by index where alike.

Decoding:

1. Of `layout`, varints: `log` (9 to 16; `block_size = 2^log`), `fpm`, `blocks`, `unknown`,
   `map` (each below 2^32), `n` (at most the bytes left of `layout`), then each stream's
   size plus 1 modulo 2^32 (so that 0 is `0xffffffff`). `blocks × block_size` must be the
   output's length. The directory's length is `4 + 4n + 4 × (the streams' blocks in all)`;
   its blocks, `⌈dir_bytes / block_size⌉` of them, must take at most `block_size` bytes as
   `u32`s; with the streams' blocks they must be at most the bytes left of `layout`.
2. Then the blocks, the directory's then each stream's, each a varint of `layout`
   unzigzagged and added to the previous block plus 1 (the first's previous is `map`), each
   below 2^32. `layout` must be at its end.
3. Of each block, the bytes at its start that are **covered**: block 0, 56 (the magic and the
   fields); block `map`, 4 for each of the directory's blocks; each of the directory's blocks
   and each stream's, of the directory's or the stream's bytes, those in it (a block's worth,
   but of the last). No block may be covered twice, nor by 0 bytes, and each must be below
   `blocks`.
4. The output is the superblock (`dir_bytes` the directory's length), the list of the
   directory's blocks at the start of block `map`, the directory in its blocks, each stream's
   data in its blocks (the streams' data taken in order from `streams`, which must then be at
   its end), and of each block in order, the bytes after those covered from `rest` (which must
   then be at its end).

### Encoding *(encoder)*

A file is taken where its length is its blocks', its directory as its superblock tells
(its blocks listed in one block), no block used twice, and where the file decoded from the
inputs made is the same. Formulas take PDB files only where their modules' symbols are 512 KiB
or more (§3).

## 3. The `pdb-symbols` module

A module's stream starts with its symbols: a `u32` signature, then CodeView symbol records,
each a `u16` length (of what follows it), a `u16` kind and its fields. Some records open a
scope that a later one closes; those that open one hold the offsets (from the start of the
module's stream) of the record of the scope they are in (0 at the top) and of the record that
closes them. The symbols are followed by the module's C11 lines, its C13 lines (subsections,
each a `u32` kind, a `u32` length, its body and zeros to 4 bytes) and the rest (its global
references). The module rebuilds the streams of many modules, one after another.

- **Module**: name `pdb-symbols`, version 2.
- **Params**: a varint, the scopes open at once at most (§3.4), no more than the records (half
  the bytes of `kind`, below); nothing after it.
- **Resources**: none.
- **Inputs**: exactly 6 of packed streams, each of class `pdb-symbols/NAME`:

  | # | input | its streams |
  |---|---|---|
  | 0 | `head` | `kind`, `len`, `scope` |
  | 1 | `fields` | `item`, `type`, `code`, `tail`, `raw` |
  | 2 | `text` | `name`, `ann` |
  | 3 | `ann` | `annop`, `annoff`, `annlen`, `annline`, `annother` |
  | 4 | `c13` | `c13`, `c13line`, `c13off`, `c13flag`, `c13inl`, `c13inlline`, `c13inlid` |
  | 5 | `lines` | `lines` |

- **Output**: the modules' streams, one after another, `len` bytes in all.

### 3.1 Layouts

Each kind has a layout, its fields in order (the record's other bytes, to its end, follow
them), and opens a scope, closes one or neither:

| kind | | fields |
|---|---|---|
| `0x114d` (`S_INLINESITE`) | opens | parent, end, item |
| `0x115d` (`S_INLINESITE2`) | opens | parent, end, item, u32 |
| `0x1110`, `0x110f`, `0x1155`, `0x1156` (procedures) | opens | parent, end, u32, code, code, code, type, code, u16, u8, name |
| `0x1147`, `0x1146` (procedures of ids) | opens | parent, end, u32, code, code, code, item, code, u16, u8, name |
| `0x1103` (`S_BLOCK32`), `0x1104` (`S_WITH32`) | opens | parent, end, code, code, u16, name |
| `0x1102` (`S_THUNK32`) | opens | parent, end, u32, code, u16, u16, u8, name |
| `0x1132` (`S_SEPCODE`) | opens | parent, end |
| `0x0006`, `0x114f`, `0x114e` (ends) | closes | |
| `0x113e` (`S_LOCAL`) | | type, u16, name |
| `0x1111` (`S_REGREL32`) | | code, type, u16, name |
| `0x110b` (`S_BPREL32`) | | code, type, name |
| `0x110c`, `0x110d`, `0x1112`, `0x1113` (data) | | type, code, u16, name |
| `0x1105` (`S_LABEL32`) | | code, u16, u8, name |
| `0x1108` (`S_UDT`) | | type, name |
| `0x1168` (`S_INLINEES`) | | items |
| `0x114c` (`S_BUILDINFO`) | | item |
| `0x1101` (`S_OBJNAME`) | | u32, name |
| any other | | |

The other bytes of `S_INLINESITE` and `S_INLINESITE2` are their **annotations**; of the
others, their **rest**.

### 3.2 State

Kept all through: `prev[s] = 0` for each slot `s` below 1024 (the slot of field `k` of a
record of kind `t` is `(31t + k) AND 1023`); `item_prev = type_prev = 0`; `item_cache` and
`type_cache` empty lists; of the lines (§3.5), `block_off`, `block_file`, `line`, `inl_file`
and `inl_line` 0, and of each inlinee its last file and line (none at first). A value **cached** is read from a varint `y`: `y < 64`, entry `y` of
the list (which must exist); else the previous moved by `unzigzag(y − 64)` (within `u32`); then
the previous is set to the value and the value goes to the front of the list (leaving its place
there; the list keeps its first 64).

### 3.3 Modules

Until `len` is at its end, a module: its symbols, a varint `size` of `len`: a size below 4,
`size` bytes of `raw`; else a `u32` signature of `tail`, then records (§3.4) until a varint of
`len` is 2, then a varint `n` of `len` and `n` bytes of `raw`; the symbols must then be `size`
bytes, and no scope open (below) may have an end to derive. Then a varint `n` of `len` and `n`
bytes of `lines` (the C11 lines); a varint `n` of `len` and, where not 0, C13 lines of `n`
bytes (§3.5); a varint `n` of `len` and `n` bytes of `lines` (the rest). Offsets of records are
from the start of the module's stream.

### 3.4 Records

A record's varint `v` of `len` (not 2): its length is `v >> 1` (2 to 65535; the record, with
its `u16` length, may not end past the module's `size`); `v & 1` set: the record is **kept**.
Its kind is a `u16` of `kind`. Its offset `q` is from the module's start. Written: the length,
the kind, then, of a record kept, `length − 2` bytes of `raw`; else its layout's fields in
order:

- **parent**: a varint of `scope`: 0, the offset of the innermost scope open (0 where none);
  else that less 1 (below 2^32), as a `u32`.
- **end**: a varint of `scope`: 0, to derive: 4 zero bytes, set when the scope closes to the
  offset of the record closing it; else that less 1, as a `u32`.
- **item**, **type**: a `u32` cached (§3.2) from a varint of `item` (`item_cache`,
  `item_prev`) or `type` (`type_cache`, `type_prev`).
- **items**: a count, a varint of `code` (at most a quarter of the record's length), as a
  `u32`, then that many items.
- **code**: `prev[slot]` moved by a varint of `code` unzigzagged (within `u32`), as a `u32`.
- **u32**, **u16**, **u8**: as many bytes of `tail`.
- **name**: bytes of `name` up to and with a NUL.

Then the record's other bytes, `r` of them (to its end, at least 0): of its rest, `r` bytes of
`tail`; of its annotations, by a byte of `annop`: `0xff`, kept: that byte, then `r` bytes of
`ann`; else operations, each a byte `op` of `annop` until one is 0 (which ends them): `op`
must be 1 to 13; written as a byte, then its operands, each a varint (below 2^29) of its
stream written as a compressed integer: below 2^7 a byte, below 2^14 two bytes (`0x80 | v >>
8`, `v`), else four bytes (`0xc0000000 | v`, big-endian). Opcode 3: one of `annoff`; 12: two
of `annlen`; 1, 2, 4: one of `annlen`; 6, 7, 11: one of `annline`; others one of `annother`.
The record may not grow past its end, and zeros fill it to its end.

After a record that opens a scope, it is open (innermost; no more may be open at once than the
params tell); after one that closes one, the innermost open closes (if any), its end, where to
derive, set to the closing record's offset `q`. The record must have ended at its length.

### 3.5 C13 lines

A byte of `c13`: 1, the lines are `n` bytes of `lines`; 0, a varint count of `c13` of
subsections, each: a `u32` kind and a length `L`, varints of `c13` (the subsection may not end
past `n`), written as `u32`s, then a byte of `c13`: 1, the body is `L` bytes of `lines`; 0, the
body by its kind (others fail):

- `0xf2` (lines): `block_off` moved by a varint of `c13` (unzigzagged, within `u32`), 4 bytes
  of `c13` (a `u16` segment and `u16` flags), a varint of `c13` (the code's size, a `u32`);
  then blocks until the body is `L` bytes: `block_file` moved by a varint of `c13`, a count
  `k` (a varint of `c13`), written as `u32`s with the block's size `12 + 8k` (`12 + 12k` where
  the flags' bit 0 is set; the block may not end past `L`); then `k` lines, each an offset
  moved from the block's previous (0 for the first) by a varint of `c13off`, `line` moved by a
  varint of `c13line` (below 2^24), and a byte of `c13flag`, written as the offset and `line |
  flag << 24`; then, of columns, `4k` bytes of `lines`.
- `0xf6` (inlinee lines): `L − 4` a multiple of 12; a `u32` 0; then `(L − 4) / 12` entries:
  the inlinee, 4 bytes of `c13inlid`; its file, moved by a varint of `c13inl` from the
  inlinee's last file (else `inl_file`); its line, moved by a varint of `c13inlline` from its
  last line (else `inl_line`); written as three `u32`s; `inl_file`, `inl_line` and the
  inlinee's last file and line are set to them.

The body must then be `L` bytes, then zeros to 4 bytes from the lines' start. The lines must
then be `n` bytes.

### 3.6 Encoding *(encoder)*

C13 lines are kept where their subsections are not padded with zeros; a subsection of lines or
inlinee lines, where its blocks' sizes are not as above or its signature is not 0. A record is
kept where its fields do not fit its length (a name without its NUL); an inline
site's annotations are kept where they hold an operation not of 1 to 13, an integer not in its
fewest bytes, or bytes but zeros after an operation 0. Parents and ends are derived where they
are those of the scopes as they nest. The encoder decodes what it made and takes the symbols
only where they come back.

## 4. The `pdb-types` module

The TPI stream (stream 2) holds the type records, the IPI stream (stream 4) the id records
(functions, strings, build information, links of a type to its line); both are a header of 56
bytes and records, each a `u16` length (of what follows it), a `u16` kind and its fields. A
stream's header names a hash stream (a stream of the file): the hash value of each record (`u32`,
modulo the header's number of buckets), then an index (pairs of `u32`: a record's index and its
offset in the records, for some of them) and the adjusters (empty). The module makes the two
streams and their hash streams from the fields of the records; the hash stream is made in two
outputs, which the formula puts one after the other.

- **Module**: name `pdb-types`, version 1. A **part module** (spec, "Module execution").
- **Params**: a byte for the TPI stream and one for the IPI stream, `mode | rule << 2`, then six
  varints, the sizes of the outputs.
- **Resources**: none.
- **Inputs**: exactly 13, each one stream of the names below, read in order (windows of one
  page).
- **Outputs**: exactly 6, of the sizes in the params: the TPI stream, its hash values, its index
  (and adjusters, none), then the IPI stream's three.

`mode` is 0 (the stream is absent: its three sizes are 0), 1 (**records**: the stream, its size
at least 56; the other two sizes are 0) or 2 (**full**: also the hash values, a multiple of 4
bytes, and the index, a multiple of 8). The records of the TPI stream are decoded first, then
those of the IPI stream, from the same inputs. The bits of `rule`: bit 0 the rule of the index
(§4.4), bit 1 that the header names the offset of the adjusters as the index's, not as its end
(lld); in mode 1 no rule is set.

| # | input | holds |
|---|---|---|
| 0 | `kind` | a byte for each record: its kind's index in the list below, or 255 (kept) |
| 1 | `sub` | for a field list or a method list, a byte for each member and 0 at the end |
| 2 | `ti` | indices coded as the distance back, of records but field lists and member functions |
| 3 | `fti` | indices of the other stream's space (§4.2) |
| 4 | `num` | counts, and numeric leaves (§4.3) |
| 5 | `attr` | small fields, of records but field lists, as varints |
| 6 | `name` | names, each with its NUL |
| 7 | `raw` | bytes as they are |
| 8 | `hdr` | the header (§4.4) |
| 9 | `tif` | indices of field lists, as the distance back |
| 10 | `tim` | indices of member functions (kind `0x1009`) as the distance back, but the class by `Prev` |
| 11 | `attrf` | small fields of field lists |
| 12 | `tia` | indices coded as `Abs` |

The kinds with layouts, in the order of their indices: `0x000a`, `0x1001`, `0x1002`, `0x1008`,
`0x1009`, `0x1201`, `0x1203`, `0x1205`, `0x1206`, `0x1503`, `0x1504`, `0x1505`, `0x1506`,
`0x1507`, `0x1601` to `0x1607`. The members of a field list, with the codes 1 and up in `sub`:
`0x1400`, `0x1401`, `0x1402`, `0x1404`, `0x1409`, `0x1502`, `0x150d`, `0x150e`, `0x150f`,
`0x1510`, `0x1511`, `0x1512`. In a method list the byte of a member is 255.

### 4.1 Layouts

A record is made of its kind (a `u16`) and its fields, the **padding** and nothing else. The
padding is the bytes that make the record's length (with the `u16` before it) a multiple of 4,
`0xf3 0xf2 0xf1` for 3, `0xf2 0xf1` for 2, `0xf1` for 1, none for 0; a member of a field list is
padded in the same way, counting from the start of the record. Fields (`ti` an index of the type
records, `id` of the id records; both coded by §4.2; `u8`, `u16`, `u32` to `attr` or `attrf`
as a varint; `count` a `u16` or `u32` to `num`; `numeric` §4.3; `name` to `name`, with its NUL;
`tail` below):

| kind | fields |
|---|---|
| `0x000a` | `u16` n, ⌈n / 2⌉ bytes of `raw` (no padding but the usual) |
| `0x1001` | ti, u16 |
| `0x1002` | ti, u32 attr; if bits 5 to 7 of attr are 2 or 3: ti, u16 |
| `0x1008` | ti, u8, u8, u16, ti |
| `0x1009` | ti, ti (`Prev`), ti, u8, u8, u16, ti (`Abs`), u32 |
| `0x1201` | count (u32) n, n × ti |
| `0x1203` | members until the end |
| `0x1205` | ti, u8, u8 |
| `0x1206` | members until the end: u16 attr, u16, ti, and a u32 if bits 2 to 4 of attr are 4 or 6 |
| `0x1503` | ti, ti, numeric, name |
| `0x1504`, `0x1505` | count (u16), u16 prop, ti, ti, ti, numeric, name, and a name if prop & 0x200 |
| `0x1506` | count (u16), u16 prop, ti, numeric, name, and a name if prop & 0x200 |
| `0x1507` | count (u16), u16 prop, ti, ti, name, and a name if prop & 0x200 |
| `0x1601` | id, ti, name, tail |
| `0x1602` | ti, ti, name, tail |
| `0x1603` | count (u16) n, n × id |
| `0x1604` | count (u32) n, n × id |
| `0x1605` | id, name |
| `0x1606` | ti, id, u32 |
| `0x1607` | ti, id, u32, u16 (no padding) |

| member | fields |
|---|---|
| `0x1400` | u16, ti, numeric |
| `0x1401`, `0x1402` | u16, ti, ti, numeric, numeric |
| `0x1404`, `0x1409` | u16, ti |
| `0x1502` | u16, numeric, name |
| `0x150d` | u16, ti, numeric, name |
| `0x150e` | u16, ti (`Abs`), name |
| `0x150f` | count (u16), ti, name |
| `0x1510`, `0x1512` | u16, ti, name |
| `0x1511` | u16 attr, ti, a u32 if bits 2 to 4 of attr are 4 or 6, name |

**tail**: a byte of the attributes' stream, 0 or 1; if 1, 8 bytes of `raw` (what some compilers
write after the name: a hash).

A record **kept** has the code 255 in `kind`; `raw` holds a varint kind, a varint `n` (2 to
65535) and `n − 2` bytes, the record's after its kind. Any record of a kind with no layout is
kept, as is one that the layout does not give exactly (§4.5).

### 4.2 Indices

A record has an index, `begin` (the header's first index) plus its position. An index of the
stream's own space (types in the TPI stream, ids in the IPI stream) is, by how it is coded:

- `Back`: the varint `v`. Even: the record's index less 1 less `v / 2`, which must be `begin` or
  more. Odd, `u = v >> 1`: `u` if below `begin` (a simple type), else the record's index plus
  `u − begin` (a record after it).
- `Abs`: the varint, as it is.
- `Prev`: the previous index coded so (0 at the start of each stream) moved by the varint
  unzigzagged.

An index of the other space (types in the IPI stream) is the previous one in `fti` (0 at the
start of each stream) moved by the varint unzigzagged. All results must fit a `u32`.

### 4.3 Numeric leaves

A leaf is a `u16`, a value if below 0x8000, else a code (`0x8000` to `0x8010`) and the bytes
that follow. The varint of `num` is `z`. Unless it is 2⁶⁴ − 1, the value is `unzigzag(z)`, in the
shortest of: inline (0 to 0x7fff); `0x8002` (to 0xffff); `0x8004` (to 0xffffffff); `0x800a`
(above); for negative numbers `0x8000` (to −128), `0x8001` (to −32768), `0x8003`
(to −2³¹), `0x8009`. For 2⁶⁴ − 1: a varint `n` and `n` bytes of `raw`, the leaf as it is.

### 4.4 Header, hashes and index

In mode 1 the header is 56 bytes of `hdr`, of which `begin` is the `u32` at 8. In mode 2, the
varints of `hdr`: the version, `begin`, the hash stream's index (a `u16`), the auxiliary index
(`u16`), the size of a key, the number of buckets (not 0), and the number of exceptions `e`;
then `e` pairs, a varint (the position of a record less the last exception's, from 0) and the
hash value of the record. The header is the `u32`s: the version, 56, `begin`, `begin` plus the
records (the size of the hash values over 4), the bytes of the records, the hash stream's index
and the auxiliary's as a `u32` (`hash | aux << 16`), the key, the buckets, 0, the size of the
hash values, the same, the size of the index, the offset of the adjusters (the size of the hash
values plus that of the index, or without it with bit 1 of `rule`), 0.

The hash value of a record, modulo the buckets (the record's bytes include its length):

- kinds `0x1606` and `0x1607`: the string hash of the 4 bytes after the kind;
- kinds `0x1504` to `0x1507` that are not forward declarations (bit 7 of prop clear) and whose
  name, after the last colon, is not `<unnamed-tag>` or `__unnamed`: the string hash of the
  unique name if bit 8 of prop (scoped) is set and there is one, else of the name;
- others: the CRC-32 (reflected, polynomial 0xedb88320, the register 0 at the start and the
  result) of the record.

The string hash: `r` the XOR of the `u32`s of the string, the `u16` of the remainder if
its bit 1 is set and its last byte if bit 0 is; `r |= 0x20202020`, `r ^= r >> 11`, `r ^ (r >> 16)`.
A record whose hash is another is an exception, which `hdr` names.

The index has an entry (the record's index and its offset from the first record, two `u32`s) for
the first record, and with rule 0 for each record at an offset of the next multiple of 8192
or more after the previous entry's offset (the entry's, rounded down to a multiple of 8192, plus
8192), with rule 1 for each record that ends in a later 8192 bytes than it starts in.

### 4.5 Encoding *(encoder)*

A stream is taken where it has a header of the version 20040203 and 56 bytes, a `begin` other
than 0 and records to its end. Mode 2 is used where its hash stream is another stream of the
file, of the size the header says, with an index by one of the rules and a header as above. The
encoder decodes what it made and compares it with the streams. A record is kept where its
fields do not end at its length, its padding is not the usual, a leaf has a code not in the table,
or its kind has no layout. A numeric leaf is escaped where it is not in the shortest form, not
whole or past ±2⁶². Formulas take the streams where the records are 64 KiB or more.
