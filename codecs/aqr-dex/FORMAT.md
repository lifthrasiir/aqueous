# The `dex-recons` module

This document specifies the module `dex-recons` of aqr-formula entry formulas. The module
rebuilds an Android DEX file from streams: its header and map, its strings front-coded, its
tables of ids and of class definitions by column, the data of its classes with the offsets of
their code as ranks, its code items with their instructions split by field and their indices
through caches, and its other sections as they are.
The document is meant to be complete: an implementation of the module can be written from it
alone. Sections marked *(encoder)* describe the choices this implementation makes when it
builds formulas; the module does not depend on them.

The DEX format is that of Android's "Dalvik Executable format" page (source.android.com); its
names (`string_id_item`, `map_list`, `class_data_item`, ...) are used here. Nothing of it is
needed to decode beyond what this document says.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [Decoding](#3-decoding)
4. [Sections taken](#4-sections-taken)
5. [Encoding](#5-encoding-encoder)

## 1. Conventions

- Integers are unsigned and little-endian: `u16`, `u32`. **varint**: unsigned LEB128, least
  significant group first, at most 10 bytes, read as a 64-bit value. **uleb**: the DEX
  format's ULEB128 (at most 5 bytes, below 2^32); the module writes it in its shortest form.
  **sleb**: the DEX format's SLEB128 (at most 5 bytes).
- `unzigzag(z) = (z >> 1) XOR −(z & 1)`. A value **moved** from `prev` by `z` is `prev +
  unzigzag(z)`, which must lie within the range the field's width allows (`u16` or `u32`).
- **align4(p)** is the least multiple of 4 not below `p`.
- Any read past the end of a stream, any value out of its range, a stream not read to its end
  and any write outside the output make the module fail.

## 2. The module

- **Module**: name `dex-recons`, version 2.
- **Params**: none (zero bytes).
- **Resources**: none.
- **Inputs**: exactly 14, each of class `dex/NAME`, which hold the streams below: input `k`
  is the varints of the lengths of all its streams but the last, then its streams one after
  another, the last taking what is left.
- **Output**: the file, `len` bytes long (the `len` the application declares).

| # | input | its streams |
|---|---|---|
| 0 | `sections` | `rest`, `head`, `code`, `type-list`, `raw` |
| 1 | `debug` | `debug` |
| 2 | `annotation` | `annotation`, `annotation-sets`, `array` |
| 3 | `strings` | `str-suffix` |
| 4 | `ids` | `str-prefix`, `type`, `proto-ret`, `proto-params`, `proto-shorty`, `field-class`, `field-name`, `field-type`, `method-class`, `method-name`, `method-proto` |
| 5 | `classes` | `cdef-class`, `cdef-flags`, `cdef-super`, `cdef-ifaces`, `cdef-src`, `cdef-offs`, `cdata-sizes`, `cdata-fdiff`, `cdata-fflags`, `cdata-mdiff`, `cdata-mflags`, `cdata-code` |
| 6 | `code` | `code-head`, `code-len`, `code-tries`, `code-debug`, `code-try`, `ins-zero`, `ins-argn` |
| 7 | `op` | `ins-op` |
| 8 | `reg4` | `ins-reg4` |
| 9 | `reg8` | `ins-reg8`, `ins-reg16` |
| 10 | `args` | `ins-args` |
| 11 | `lit` | `ins-lit`, `ins-branch`, `ins-payload` |
| 12 | `hits` | `idx-hit`, `hit-string`, `hit-type`, `hit-field`, `hit-method` |
| 13 | `misses` | `idx-string`, `idx-type`, `idx-field`, `idx-method`, `idx-other` |

The streams:

| stream | contents |
|---|---|
| `rest` | the bytes of the file not otherwise written (§3.4), in order |
| `head` | the header, the map list and how each of its items is rebuilt (§3.1) |
| `code`, `debug`, `type-list`, `annotation`, `annotation-sets`, `array`, `raw` | sections kept as they are (§3.2) |
| `str-prefix`, `str-suffix` | the strings (§4.1) |
| `type` | the type ids (§4.2) |
| `proto-*` | the prototype ids (§4.3) |
| `field-*`, `method-*` | the field and method ids (§4.4) |
| `cdef-*` | the class definitions (§4.5) |
| `cdata-*` | the classes' data (§4.6) |
| `code-*` | the code items but their instructions (§4.7) |
| `ins-*` | the fields of instructions (§4.8) |
| `idx-hit`, `hit-*`, `idx-*` | the indices of instructions (§4.9) |

## 3. Decoding

`out` is `len` zero bytes. Parts of it are **written**, each a range of `out`; §3.4 checks
that they do not overlap.

### 3.1 The head

Read from `head`:

1. 112 bytes, the header: write them at 0. `map_off` is `u32` at 52 of them.
2. A byte `checks`, below 4: bit 0, the checksum is derived; bit 1, the signature is (§3.5).
3. `u32 count`, then `12 × count` bytes, the entries of the map list: write the 4 bytes of
   `count` at `map_off`, and the entries after them. Each entry is `u16 type`, 2 bytes not
   read, `u32 size`, `u32 offset`: item `i` of the map.
4. `count` bytes, the **mode** of each item: 0, its section is among the rest; 1, it is kept
   as it is (§3.2); 2, it is taken (§4). Mode 2 is allowed only for the types `0x0001`
   (string ids), `0x0002` (type ids), `0x0003` (prototype ids), `0x0004` (field ids),
   `0x0005` (method ids), `0x0006` (class definitions), `0x2000` (class data), `0x2001` (code
   items) and `0x2002` (string data), for one item of each type at most. Any other mode is an
   error.

### 3.2 Sections kept

For each item of mode 1, in the map's order: its section starts at its offset and ends at the
least offset of the map's items that is above its own, or at `len` where there is none. Copy
that many bytes from the stream of its type (`0x2001` `code`; `0x2003` `debug`; `0x1001`
`type-list`; `0x2004` `annotation`; `0x1003`, `0x1002`, `0x2006` `annotation-sets`; `0x2005`
`array`; any other `raw`) and write them there.

Lists are read from what was written, for §4:

- **lists**: where an item of type `0x1001` is of mode 1, the offsets of the `size` type
  lists of the first such item: from `p` its offset, `size` times: `p = align4(p)`, an offset `p`, then `p`
  past `u32 n` at `p` and `n` `u16`s. Else empty.
- **code**: where an item of type `0x2001` is of mode 1, the offsets of the `size` code
  items of the first such item: from `p` its offset, `size` times: `p = align4(p)`, an offset `p`; with
  `tries` the `u16` at `p + 6` and `insns` the `u32` at `p + 12`, `q = p + 16 + 2 × insns`;
  where `tries > 0`, `q` goes past `2 × (insns & 1) + 8 × tries` bytes and then the handler
  list: a uleb `n`, then `n` handlers, each a sleb `s`, `|s|` pairs of ulebs, and one uleb
  more where `s ≤ 0`; `p = q`. Each must lie within `out`. Else, where the item of type
  `0x2001` is of mode 2, the offsets of its code items as §4.7 writes them; else empty.
- **debug**: where an item of type `0x2003` is of mode 1, the offsets of the `size` debug info
  items of the first such item: from `p` its offset, `size` times: an offset `p`, then `p`
  past a uleb, a uleb `n` and `n` ulebs, then bytes `b` until one is 0: after `b = 1`, `5`,
  `6` or `9` a uleb, after `2` a sleb, after `3` three ulebs, after `4` four ulebs. Else empty.

### 3.3 Sections taken

The items of mode 2 are rebuilt in this order (§4): strings, type ids, prototype ids, field
ids, method ids, code items, class definitions with their data. The tables (types `0x0001` to
`0x0006`) are `size` entries of 4, 4, 12, 8, 8 and 32 bytes from their offset.

### 3.4 The rest

The ranges written must not overlap. Copy from `rest` the bytes between them, before the first
and after the last, in order. Every stream must then be at its end.

### 3.5 Checks

Where `checks` has bit 1: write the SHA-1 of `out[32..]` at 12. Then, where it has bit 0:
write the Adler-32 of `out[12..]` at 8, as `u32`.

## 4. Sections taken

### 4.1 Strings

The items of type `0x0001` and `0x2002` are both of mode 2 or neither; their sizes must be
the same, `n`. The string data is written from the offset `d` of item `0x2002`, one string
after another: for each `i < n`, read a varint `k` from `str-prefix`, at most the length of
string `i − 1` (0 for the first), and from `str-suffix` the bytes up to the next zero byte,
past it. String `i` is the first `k` bytes of string `i − 1` followed by those bytes. Its
`string_data_item` is written at `p` (`d` for the first, else where the last ended): the uleb
of its UTF-16 length (its bytes `b` with `b & 0xC0 ≠ 0x80`), its bytes, a zero byte. Entry `i`
of the string ids is `u32 p`.

### 4.2 Type ids

Each entry is a `u32` moved from the last (0 before the first) by a varint of `type`.

### 4.3 Prototype ids

Each entry is `u32 shorty_idx`, `u32 return_type_idx`, `u32 parameters_off`, read as:

- `return_type_idx`: moved from the last (0 before the first) by a varint of `proto-ret`.
- `parameters_off`: a varint `k` of `proto-params`: 0 for 0, else entry `k − 1` of lists.
- `shorty_idx`: a varint `k` of `proto-shorty`: `k − 1` where `k > 0`, below 2^32. Where
  `k = 0`, it is **derived**: the strings must be taken (§4.1), and the **type ids** are
  those of the first item of type `0x0002` of mode 1, else of the one of mode 2, which must
  exist. The descriptor of type `t` is the string whose index is the `u32` at entry
  `t` of those type ids (written already, §3.2 or §4.2), `t` below their size. The shorty is the
  first byte of the descriptor of the return type, then that of each parameter type (the
  `u16`s of the type list at `parameters_off`, where it is not 0), each `[` made `L`; the
  index is that of the first string equal to it, which must exist.

### 4.4 Field and method ids

Each entry is `u16 class_idx`, 2 bytes, `u32 name_idx`: `class_idx` moved from the last's (0
before the first) by a varint of `field-class` (or `method-class`); a varint `z` of
`field-name` (`method-name`) gives `name_idx` moved from the last's (0 before the first) where
`class_idx` is the last's, else `z` itself, below 2^32; the 2 bytes are copied from
`field-type` (`method-proto`).

### 4.5 Class definitions

Each entry is 8 `u32`s, read as:

- `class_idx`: moved from the last (0 before the first) by a varint of `cdef-class`.
- `access_flags`: a varint of `cdef-flags`, below 2^32.
- `superclass_idx`, `source_file_idx`: a varint of `cdef-super`, `cdef-src`, below 2^32,
  minus 1 modulo 2^32.
- `interfaces_off`: a varint `k` of `cdef-ifaces`: 0 for 0, else entry `k − 1` of lists.
- `annotations_off`, `class_data_off`, `static_values_off`: a varint `z` each of `cdef-offs`,
  in this order. 0 is 0. For `class_data_off` where the item of type `0x2000` is of mode 2,
  any other `z` makes the class's data (§4.6) be read now, its **rank** `r` moved from the
  number of classes' data read before by `z − 1`; its offset is set below. Else each column
  keeps its last value other than 0 (0 at first), and the value is moved from it by `z − 1`
  and becomes it.

Where the item of type `0x2000` is of mode 2 (which needs that of `0x0006`): the data of `m`
classes was read, `m` its size; their ranks must be `0` to `m − 1`, each once. In order of
rank, the data is written from the item's offset one after another, and each class's
`class_data_off` is where its data starts.

### 4.6 Classes' data

The code items, from **code** (§3.2), are each used or not, at first none; the **cursor** is
0. A class's `class_data_item` is:

- four ulebs from varints of `cdata-sizes`: `s0` static fields, `s1` instance fields, `d`
  direct methods, `v` virtual methods;
- for each of `s0 + s1` fields, a uleb from a varint of `cdata-fdiff`, a uleb from one of
  `cdata-fflags`;
- for each of `d + v` methods, a uleb from a varint of `cdata-mdiff`, a uleb from one of
  `cdata-mflags`, and the uleb `code_off`.

The **window** of the class is the code items not used from the cursor up to `d + v` after
it (not past the last), in order, made before its methods. For each method, read a varint `k`
of `cdata-code`: 0, `code_off` is 0; `1 ≤ k ≤ |window|`, the item `k − 1` of the window,
which leaves it; else the item `cursor + unzigzag(k − |window| − 1)`, which must exist. The
item is then used, and `code_off` is its offset. After the class's methods, the cursor goes
past the items used.

### 4.7 Code items

The code items are written from the item's offset: for each of its `size`, zero bytes until
the offset is a multiple of 4, then a `code_item`, whose offset is the next of **code**:

1. `registers_size` and `ins_size`, each a varint of `code-head` below 2^16;
2. `outs_size`, set after the instructions (6.);
3. `tries_size`, a varint of `code-tries` below 2^16;
4. `debug_info_off`, from a varint `k` of `code-debug` (below);
5. `insns_size` `n`, a varint of `code-len` below 2^32;
6. the instructions, `n` code units (§4.8); then `outs_size` is moved from the most words of
   arguments that its invocations pass (§4.8; 0 where none, at most 2^16 − 1) by a varint of
   `code-head`;
7. where `tries_size > 0`, bytes copied from `code-try`: `2 × (n & 1) + 8 × tries_size`, then
   a handler list as §3.2 reads it.

The debug info items, of **debug**, are used or not, at first none; the **next** is the
first, and the **recent** list, the items used, the most recent first, is empty. `k = 0`:
`debug_info_off` is 0; `k = 1`: the item is the next; `2 ≤ k < 2 + |recent|`: entry `k − 2`
of recent; else item `k − 2 − |recent|`, which must exist. The item is then used: it goes to
the front of recent (leaving its place there; recent keeps its first 128), and where it is the
next, the next is the one after it.

### 4.8 Instructions

The **words** of a method `m` are those of the parameters of its prototype (two for a type
whose descriptor starts with `J` or `D`, one for any other, and for one whose descriptor is
not a string taken (§4.1); at most 255), where the method ids, the prototype ids and the type
ids are there (the first items of their types of mode 1, else of mode 2): `m` below their
size, its prototype (`u16` at 2 of its entry) below theirs, its parameters (`u32` at 8 of the
prototype's entry) 0 or a `u32 n` and `n` `u16` type indices within `out`, each below the
type ids' size. Else 0.

Instructions are read until `n` code units are written; one that would end past them makes
the module fail. Each starts with its opcode `op`, a byte of `ins-op`:

- `op = 0`: a byte `h` of `ins-zero`. Where `h` is 1, 2 or 3, the instruction is a payload
  whose first unit is `h << 8`: then units (`u16`) of `ins-payload`: the next 1 (3 where `h =
  3`) tell its length, `4 + 2 × u0` units for `h = 1`, `2 + 4 × u0` for `h = 2`, `4 +
  ceil(u0 × (u1 | u2 << 16) / 2)` for `h = 3`, all of it; the rest of its units follow from
  `ins-payload`. Else the unit is `h << 8`.
- Else the units of the format of `op` (below), the first `op` in its low byte, and its fields
  in the order listed, each set in its bits:

| opcodes | units | fields (unit:bits class) |
|---|---|---|
| `01 04 07 12 21 7B`–`8F` `B0`–`CF` | 1 | 0:8–15 R4 |
| `0A`–`0D` `0F`–`11` `1D 1E 27` | 1 | 0:8–15 R8 |
| `28` | 1 | 0:8–15 B |
| `29` | 2 | 0:8–15 Z, 1 B |
| `02 05 08` | 2 | 0:8–15 R8, 1 R16 |
| `38`–`3D` | 2 | 0:8–15 R8, 1 B |
| `13 15 16 19` | 2 | 0:8–15 R8, 1 L |
| `1A`ˢ `1C 1F 22`ᵗ `60`–`6D`ᶠ `FE`ʰ `FF`ᵖ | 2 | 0:8–15 R8, 1 I |
| `2D`–`31` `44`–`51` `90`–`AF` | 2 | 0:8–15 R8, 1:0–7 R8, 1:8–15 R8 |
| `D8`–`E2` | 2 | 0:8–15 R8, 1:0–7 R8, 1:8–15 L |
| `32`–`37` | 2 | 0:8–15 R4, 1 B |
| `D0`–`D7` | 2 | 0:8–15 R4, 1 L |
| `20 23`ᵗ `52`–`5F`ᶠ | 2 | 0:8–15 R4, 1 I |
| `2A` | 3 | 0:8–15 Z, 1–2 B |
| `03 06 09` | 3 | 0:8–15 Z, 1 R16, 2 R16 |
| `14 17` | 3 | 0:8–15 R8, 1–2 L |
| `26 2B 2C` | 3 | 0:8–15 R8, 1–2 B |
| `1B`ˢ | 3 | 0:8–15 R8, 1–2 I |
| `24`ᵗ `6E`–`72`ᵐ `FC`ᶜ | 3 | 1 I, 0:8–15 N, 2 A |
| `25`ᵗ `74`–`78`ᵐ `FD`ᶜ | 3 | 1 I, 0:8–15 N, 2 R16 |
| `FA`ᵐ | 4 | 1 I, 0:8–15 N, 2 A, 3 Iᵖ |
| `FB`ᵐ | 4 | 1 I, 0:8–15 N, 2 R16, 3 Iᵖ |
| `18` | 5 | 0:8–15 R8, 1–4 L |
| any other | 1 | 0:8–15 Z |

A field of a whole unit is 16 bits; of units `a`–`b`, 32 or 64 bits over them, the low unit
first. Classes: Z a byte of `ins-zero`; R4 a byte of `ins-reg4`; R8 a byte of `ins-reg8`; R16
two bytes of `ins-reg16`; A two bytes of `ins-args`; L and B the field's bytes (1, 2, 4 or 8,
little-endian) of `ins-lit` and `ins-branch`; N a byte of `ins-argn` (below); I an index
(§4.9) of the kind its opcode's mark says: ˢ string, ᵗ type, ᶠ field, ᵐ method, ᶜ call site,
ʰ method handle, ᵖ prototype.

N: for `op` `6E`–`72` the byte read XOR `w << 4` where `w < 16`, for `74`–`78` XOR `w & 255`,
with `w` the words of the method the instruction's index names, plus 1 but for `71` and `77`;
else as read. The **words of arguments** an instruction passes: N's high 4 bits for `6E`–`72`,
`FA`, `FC`; N for `74`–`78`, `FB`, `FD`; none for others.

### 4.9 Indices

Each kind has a cache, the indices of the kind read, the most recent first (at most 128),
and a **last**, 0 at first; both start empty at the first code item and go on through all.
An index is a varint `k` of the kind's stream of hits (`hit-string`, `hit-type`, `hit-field`,
`hit-method`; `idx-hit` for prototypes, call sites and method handles): `k > 0`, entry `k − 1`
of the cache, which must exist and leaves its place; `k = 0`, the index moved from last by a
varint of the kind's stream of misses (`idx-string`, `idx-type`, `idx-field`, `idx-method`;
`idx-other` for the others), below 2^16 (2^32 for `1B`), which becomes last. The index then
goes to the front of the cache.

## 5. Encoding *(encoder)*

`aqr_dex::encode::split` takes files that start with `dex\n`, three digits below `041` (041
is a container of several files) and a zero byte, whose header gives `file_size` as their
length, `header_size` 112 and `endian_tag` `0x12345678`, and whose map list lies within them.

The items of type `0x0000` (header) and `0x1000` (map list) are of mode 0; one item of each
type of §3.1 that is the only one of its type is taken where it is laid out as §4 rebuilds
it, else kept (mode 1), as is every other item:

- the strings, where every string id points at the end of the last item (the first at the
  data's offset), each item's length is the shortest uleb of its UTF-16 length, and the
  bytes end with a zero byte;
- the prototypes, where every `parameters_off` is 0 or a type list's, the strings are taken
  and the type ids are there; a shorty that is not derived (§4.3) is given by index;
- the class definitions, where every `interfaces_off` is 0 or a type list's;
- the classes' data, where the class definitions are taken, the data of the classes is one
  after the other from the section's offset in some order, every uleb is in its shortest form
  and every `code_off` is 0 or a code item's. A code item is given by its place in the window
  where it is there, else by its distance from the cursor;
- the code items, where they are one after the other with zero bytes between them, every
  instruction ends within its array and every `debug_info_off` is 0 or a debug info item's
  (of a section kept). A debug info item is the next where it is, else given by its place in
  recent where it is there, else by its index.

The checksum and the signature are derived where they are right, else kept in the header. The
streams are decoded again, and must give the file: else every section is kept (mode 1), and
where that too fails the file is not taken.

Why this layout (measured on the 21 DEX files of 7 F-Droid APKs, 132.0 MB; `aqr-dex-check`,
zstd -9, each stream's bytes of all files compressed as one): the files 49.23 MB, the streams
31.05 MB (−36.9%), the 14 inputs 31.32 MB (−36.4%; the inputs as one stream each, 58, cost
more to allocate than the 0.27 MB they save). The strings front-coded 4.57 MB (string data
and ids 9.02 MB under zstd); the id tables 3.30 MB (5.49 MB); the class definitions and their
data 1.92 MB (4.58 MB); the code items 17.65 MB (24.98 MB). In the code items:

- the indices through caches of 128 (8: +0.7 MB, 64: +0.1 MB, 255: the same) with their
  misses as deltas from the last miss (from the last index: +0.16 MB), the hits in a stream
  for each kind (one for all: +0.18 MB);
- the debug info items by first use and recency, 0.41 MB (their offsets in deltas 0.79 MB);
- the count of arguments of invocations predicted from the methods' prototypes, 0.10 MB
  (0.69 MB), and `outs_size` from the instructions;
- streams put together in inputs by interleaving them (as fields are read) instead: 17 inputs
  cost 1.3 MB.
