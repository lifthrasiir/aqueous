# The `dex-recons` module

This document specifies the module `dex-recons` of aqr-formula entry formulas. The module
rebuilds an Android DEX file from streams: its header and map, its strings front-coded, its
tables of ids and of class definitions by column, the data of its classes with the offsets of
their code as ranks, and its other sections as they are, each kind in a stream of its own.
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

- **Module**: name `dex-recons`, version 1.
- **Params**: none (zero bytes).
- **Resources**: none.
- **Inputs**: exactly 33, the streams below in order, each of class `dex/NAME`.
- **Output**: the file, `len` bytes long (the `len` the application declares).

| # | name | contents |
|---|---|---|
| 0 | `rest` | the bytes of the file not otherwise written (§3.4), in order |
| 1 | `head` | the header, the map list and how each of its items is rebuilt (§3.1) |
| 2 | `code` | sections of `code_item`s kept as they are (§3.2) |
| 3 | `debug` | sections of `debug_info_item`s kept as they are |
| 4 | `type-list` | sections of `type_list`s kept as they are |
| 5 | `annotation` | sections of `annotation_item`s kept as they are |
| 6 | `annotation-sets` | sections of `annotation_set_item`s, `annotation_set_ref_list`s and `annotations_directory_item`s kept as they are |
| 7 | `array` | sections of `encoded_array_item`s kept as they are |
| 8 | `raw` | any other section kept as it is |
| 9, 10 | `str-prefix`, `str-suffix` | the strings (§4.1) |
| 11 | `type` | the type ids (§4.2) |
| 12–14 | `proto-ret`, `proto-params`, `proto-shorty` | the prototype ids (§4.3) |
| 15–17 | `field-class`, `field-name`, `field-type` | the field ids (§4.4) |
| 18–20 | `method-class`, `method-name`, `method-proto` | the method ids (§4.4) |
| 21–26 | `cdef-class`, `cdef-flags`, `cdef-super`, `cdef-ifaces`, `cdef-src`, `cdef-offs` | the class definitions (§4.5) |
| 27–32 | `cdata-sizes`, `cdata-fdiff`, `cdata-fflags`, `cdata-mdiff`, `cdata-mflags`, `cdata-code` | the classes' data (§4.6) |

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
   `0x0005` (method ids), `0x0006` (class definitions), `0x2000` (class data) and `0x2002`
   (string data), for one item of each type at most. Any other mode is an error.

`head` must end there.

### 3.2 Sections kept

For each item of mode 1, in the map's order: its section starts at its offset and ends at the
least offset of the map's items that is above its own, or at `len` where there is none. Copy
that many bytes from the stream of its type (`0x2001` `code`; `0x2003` `debug`; `0x1001`
`type-list`; `0x2004` `annotation`; `0x1003`, `0x1002`, `0x2006` `annotation-sets`; `0x2005`
`array`; any other `raw`) and write them there. Streams 2 to 8 must then be at their ends.

Two lists are read from what was written, for §4:

- **lists**: where an item of type `0x1001` is of mode 1, the offsets of the `size` type
  lists of the first such item: from `p` its offset, `size` times: `p = align4(p)`, an offset `p`, then `p`
  past `u32 n` at `p` and `n` `u16`s. Else empty.
- **code**: where an item of type `0x2001` is of mode 1, the offsets of the `size` code
  items of the first such item: from `p` its offset, `size` times: `p = align4(p)`, an offset `p`; with
  `tries` the `u16` at `p + 6` and `insns` the `u32` at `p + 12`, `q = p + 16 + 2 × insns`;
  where `tries > 0`, `q` goes past `2 × (insns & 1) + 8 × tries` bytes and then the handler
  list: a uleb `n`, then `n` handlers, each a sleb `s`, `|s|` pairs of ulebs, and one uleb
  more where `s ≤ 0`; `p = q`. Each must lie within `out`. Else empty.

### 3.3 Sections taken

The items of mode 2 are rebuilt in this order (§4): strings, type ids, prototype ids, field
ids, method ids, class definitions with their data. The tables (types `0x0001` to `0x0006`)
are `size` entries of 4, 4, 12, 8, 8 and 32 bytes from their offset. All streams 9 to 32 must
then be at their ends.

### 3.4 The rest

The ranges written must not overlap. Copy from `rest` the bytes between them, before the first
and after the last, in order; `rest` must then be at its end.

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
  where it is there, else by its distance from the cursor.

The checksum and the signature are derived where they are right, else kept in the header. The
streams are decoded again, and must give the file: else every section is kept (mode 1), and
where that too fails the file is not taken.

Why this layout (measured on the 21 DEX files of 7 F-Droid APKs, 132.0 MB; `aqr-dex-check`,
zstd -9, each stream class compressed as one): the files 49.23 MB, the streams 38.38 MB
(−22.0%). The strings front-coded 4.57 MB (string data and ids 9.02 MB under zstd); the
id tables 3.30 MB (5.49 MB); the class definitions and their data 1.92 MB (4.58 MB). The
code (24.98 MB) is as it is: the module rebuilds no instruction yet.
