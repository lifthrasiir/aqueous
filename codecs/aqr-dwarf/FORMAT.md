# The aqr-dwarf modules

This document specifies the modules of aqr-formula entry formulas that rebuild sections of
DWARF debugging information, and of the tables beside it, from streams:
[`dwarf-apple`](#2-the-dwarf-apple-module), Apple's accelerator tables, and
[`dwarf-info`](#3-the-dwarf-info-module), `.debug_info`. The document is meant
to be complete: an implementation of the modules can be written from it alone. Sections
marked *(encoder)* describe the choices this implementation makes when it builds formulas;
the modules do not depend on them.

Contents:

1. [Conventions](#1-conventions)
2. [The `dwarf-apple` module](#2-the-dwarf-apple-module)
3. [The `dwarf-info` module](#3-the-dwarf-info-module)

## 1. Conventions

- Integers are unsigned and little-endian: `u16`, `u32`. **varint**: unsigned LEB128, least
  significant group first, at most 10 bytes, read as a 64-bit value.
- `unzigzag(z) = (z >> 1) XOR −(z & 1)`. A value **moved** from `prev` by `z` is `prev +
  unzigzag(z)`, which must lie within `u32`. In §3, values are 64-bit and moved modulo 2^64,
  unless said otherwise.
- **Packed** streams: an input holding `n` streams is the varints of the lengths of the first
  `n − 1`, then the streams one after another, the last taking what is left.
- **LEB128**: DWARF's, unsigned (ULEB128) or signed (SLEB128), always written in the fewest
  bytes.
- Any read past the end of a stream, any value out of its range, a stream not read to its end
  and an output not of the length the application declares make the module fail.
- **DJB hash** of bytes `s`: `h = 5381`, then for each byte `c` of `s` in order `h = h × 33 +
  c` modulo 2^32.

## 2. The `dwarf-apple` module

Apple's accelerator tables (the sections `__apple_names`, `__apple_types`,
`__apple_namespac` and `__apple_objc` of a Mach-O image's `__DWARF` segment, as `dsymutil`
and LLVM write them) map names to the DIEs that carry them. A table is:

```text
header        u32 magic 0x48415348 ("HASH"), u16 version 1, u16 hash function 0 (DJB),
              u32 bucket_count, u32 hashes_count, u32 header_data_len,
              header data: u32 die_offset_base, u32 atom_count, atom_count × (u16 type, u16 form)
buckets       bucket_count × u32
hashes        hashes_count × u32
offsets       hashes_count × u32
data          per hash: its entries, each u32 strp (an offset in .debug_str), u32 count and
              count records (each the atoms' values), then u32 0
```

The module derives the buckets, hashes and offsets, and the order of the entries, from the
entries and the strings they name.

- **Module**: name `dwarf-apple`, version 1.
- **Params**: the header (`20 + header_data_len` bytes, as it is in the table). Its magic,
  version and hash function must be as above, `header_data_len` must be `8 + 4 ×
  atom_count`, `atom_count` at least 1, the first atom's type 1 (`DW_ATOM_die_offset`) and
  each atom's form one of `0x0b` (1 byte), `0x05` (2 bytes), `0x06` (4 bytes) and `0x07`
  (8 bytes), the first atom's `0x06`; `bucket_count` must not be 0 unless there are no
  entries.
- **Resources**: none.
- **Inputs**: exactly 5:

  | # | input | contents |
  |---|---|---|
  | 0 | `.debug_str` | the strings the entries name |
  | 1 | `strp` | each entry's `strp`, a varint added to the previous entry's (to 0 for the first) |
  | 2 | `count` | each entry's record count, a varint |
  | 3 | `die` | each record's DIE offset (below) |
  | 4 | `atoms` | each record's other atoms, as they are in the table, one record after another |

- **Output**: the table.

Decoding:

1. The entries are read until `strp` is at its end, in this order (the order of their
   strings): `strp`, then the count `n` (at most the bytes left of `die`), then `n` DIE
   offsets: the first moved from the first of the previous entry that had any (from 0 for the
   first), each other moved from the one before it, each a varint of `die`. `count` and
   `die` must then be at their ends, and `atoms` must hold exactly `size × (the records in
   all)` bytes, `size` the bytes of the atoms after the first.
2. Each entry's hash is the DJB hash of the string at its `strp` in `.debug_str`, up to its
   NUL (which must be there), and its bucket the hash modulo `bucket_count`.
3. The entries are put in the order of their buckets, then of their hashes, entries of the
   same bucket and hash keeping the order of step 1. A run of entries of the same hash is a
   group; there must be `hashes_count` groups.
4. `4 × bucket_count` must not exceed the output's length. The table is the header, then
   bucket `b` for each `b` below `bucket_count`: the index of the first group of bucket `b`,
   or `0xffffffff` where there is none; then each group's hash; then each group's offset (from
   the table's start, of where its data starts); then the data, each group's entries in order
   (`strp`, the count, and the records, each its DIE offset as a `u32` and its other atoms as
   `atoms` holds them, taken in order) and a `u32` 0 after each group.

### Encoding *(encoder)*

The table is taken where it parses as above with its data in the order of its hashes, one
group after another, and where the table decoded from the streams made is the same: every
hash the DJB hash of its strings, the hashes in that order without repeats, the entries of a
group in the order of their strings, the buckets as derived, nothing after the data. LLVM's
tables are made so. The entries are sorted by `strp` (stably) to make the streams.

## 3. The `dwarf-info` module

`.debug_info` is a sequence of units, each a header and a tree of DIEs (debugging information
entries). Each DIE is the ULEB128 code of an abbreviation of `.debug_abbrev`, which gives its
attributes and their forms, then its attributes' values; a DIE of an abbreviation with
children is followed by them, closed by a null entry (code 0). The module parses the DIEs by
the abbreviations and codes each value by what it is (DWARF 5, §7.5).

- **Module**: name `dwarf-info`, version 2.
- **Params**: four varints: `S`, the strings of `.debug_str` (§3.2); `D`, the DIEs; `O`, the
  DIEs open at once (§3.4); `F`, the references to DIEs not decoded yet (§3.5), at most. They
  bound the module's memory: a section of more of any fails. Version 1 took no params.
- **Resources**: none.
- **Inputs**: exactly 5: `.debug_abbrev`, `.debug_str` (empty where there is none), then three
  inputs of packed streams, each of class `dwarf-info/NAME`:

  | # | input | its streams |
  |---|---|---|
  | 2 | `die` | `code`, `str`, `ref` |
  | 3 | `num` | `addr`, `line`, `call`, `file`, `sec`, `idx`, `data`, `pclen` |
  | 4 | `bytes` | `unit`, `expr`, `cstr`, `raw` |

- **Output**: `.debug_info`.

### 3.1 Abbreviations

An abbreviation table is read from an offset of `.debug_abbrev`: entries, each a ULEB128 code
(0 ends the table), a ULEB128 tag, a byte (not 0: the DIEs have children), then pairs of
ULEB128 attribute and form up to a pair of zeros, a form `0x21` (`DW_FORM_implicit_const`)
followed by an SLEB128 constant (which the DIEs do not hold). These LEB128 values may be of any
length. A table fails where a code repeats, exceeds 2^16 or exceeds `1024 + 16 × (the entries
before it)`, or where it runs past the section. The tables read are kept by offset; each costs
its highest code plus 1 plus its attributes, and the tables read in all may cost at most `4 ×
|.debug_abbrev| + 256`: a table beyond that fails.

### 3.2 State

The decoder keeps, all through the section:

- `dies`: the offset of each DIE decoded (in the output), in order; a DIE's **index** is its
  place there.
- the **strings**: the offset of each string of `.debug_str`, in order: 0, and each offset
  after a NUL that is not the section's end.
- `str_next = 0`; `str_prev[s] = 0` and `ref_prev[s] = 0` and `prev[s] = 0` for each slot `s`
  below 1024; `str_cache` and `ref_cache` empty lists; `prev_addr`, `prev_strx`, `prev_addrx`
  and `prev_abbrev` 0. The **slot** of a number `a` is `(a XOR (a >> 10)) AND 1023`.

### 3.3 Units

Until `unit` is at its end, a byte of `unit`:

- **1**: a unit kept as it is: a varint `n` of `unit`, then `n` bytes of `raw` copied.
- **0**: a unit rebuilt: a `u16` version of `unit` (2 to 5); `prev_abbrev` moved by a varint
  of `unit` (64-bit, then at most `2^32 − 1`): the abbreviations' offset. Version 5: a unit
  type byte and an address size byte of `unit`; the header written is a placeholder `u32` for
  the length, the version, the unit type, the address size, the abbreviations' offset (`u32`),
  then 0 bytes (types 1 and 3), 8 (types 4 and 5) or 12 (types 2 and 6) copied from `unit`
  (other types fail). Versions 2 to 4: an address size byte of `unit`; the header written is
  the placeholder, the version, the offset and the address size. The address size must be 4
  or 8. The unit's **start** is where its header starts. Then its DIEs (§3.4), and the
  placeholder is set to the bytes after it to the end of the unit, which must be below
  `0xfffffff0`.
- any other byte fails.

### 3.4 DIEs

The table at the unit's abbreviations' offset is read (§3.1). A stack of **open** DIEs is
empty. Then, for each varint `c` of `code`:

- `c = 0`: the unit ends.
- `c = 1`: a null entry: a 0 byte written; an open DIE is taken from the stack, if any, and
  where it has a sibling to derive, that is set (below).
- else the DIE of abbreviation `c − 1` (which the table must have): its offset is appended to
  `dies`, the code written as ULEB128, then each attribute's value in order (§3.5). Then, of an
  abbreviation with children, the DIE is put on the stack; of one without, a sibling to derive
  is set.

The output may not grow past `len`. A sibling **to derive** (a `DW_AT_sibling` of symbol 0) is
written as zeros of its form's size and set, when its DIE's subtree ends, to the output's
length less its base (below), which must fit the size.

### 3.5 Values

Forms are those of DWARF 5 (§7.5.6), the sizes of fixed forms by the unit: an address the
address size, `DW_FORM_ref_addr` the address size in version 2 and 4 bytes after; offsets
(`strp`, `sec_offset`, `line_strp`, `strp_sup`, `GNU_ref_alt` `0x1f20`, `GNU_strp_alt`
`0x1f21`) 4 bytes. A number written in a fixed form must fit its size; in other forms it is
LEB128. A form `0x16` (`DW_FORM_indirect`) is replaced by a varint of `data`, written as
ULEB128, as many times as it is given. Then, by the form:

- `strp` (`0x0e`): a varint `y` of `str`. 1: the offset is a varint of `str` (below 2^32).
  Else the string `i` (index): `y = 0`: `str_next`; `2 ≤ y < 66`: entry `y − 2` of
  `str_cache`, which must exist; else `str_prev[slot(attribute)]` moved by `y − 66`. `i` must
  be a string; its offset is written. Then `str_next = max(str_next, i + 1)`,
  `str_prev[slot(attribute)] = i`, and `i` goes to the front of `str_cache` (leaving its place
  there; the list keeps its first 64).
- `ref1`, `ref2`, `ref4`, `ref8`, `ref_addr` (`0x11`–`0x14`, `0x10`): the **base** is 0 for
  `ref_addr`, else the unit's start. Of `DW_AT_sibling` (`0x01`), a varint of `ref` first: 0,
  the sibling is to derive (§3.4; at most one a DIE), and nothing more is read; else the value
  follows. A varint `y` of `ref`: 0: a varint of `ref` written as the value. Else the DIE
  `i`: `1 ≤ y < 65`: entry `y − 1` of `ref_cache`, which must exist; else
  `ref_prev[slot(attribute)]` moved by `y − 65` (64-bit). `ref_prev[slot(attribute)] = i` and `i`
  goes to the front of `ref_cache` (as `str_cache`). The value is `dies[i]` less the base
  where `i` was decoded; else zeros are written, set to that once the section is decoded
  (when `dies[i]` must exist).
- `addr` (`0x01`): the value is the base moved by a varint of `addr` (64-bit): the base is,
  for `DW_AT_high_pc` (`0x12`) of a DIE whose `DW_AT_low_pc` (`0x11`) came before as an
  address, that, else `prev_addr`. `prev_addr` is set to the value but of `DW_AT_high_pc`.
- `string` (`0x08`): bytes of `cstr` up to and with a NUL, copied.
- `block1`, `block2`, `block4`, `block`, `exprloc` (`0x0a`, `0x03`, `0x04`, `0x09`, `0x18`): a
  varint `n` of `expr`, written as the length (1, 2, 4 bytes, ULEB128, ULEB128), then `n` bytes
  of `expr` copied.
- `data16`, `ref_sig8` (`0x1e`, `0x20`): 16 or 8 bytes of `expr` copied.
- `flag_present`, `implicit_const` (`0x19`, `0x21`): nothing.
- the other forms of fixed size and `udata`, `sdata`, `ref_udata`, `strx`, `addrx`,
  `loclistx`, `rnglistx`, `GNU_addr_index` (`0x1f01`), `GNU_str_index` (`0x1f02`): a number,
  of a stream and as follows; any other form fails.

  | form | attribute | stream | value |
  |---|---|---|---|
  | `sec_offset`, `line_strp`, `strp_sup`, `GNU_strp_alt`, `GNU_ref_alt`, `ref_sup4`, `ref_sup8` | any | `sec` | `prev[slot(attribute XOR form << 16)]` moved |
  | `loclistx`, `rnglistx` | any | `idx` | as `sec` |
  | `strx`, `strx1`–`strx4`, `GNU_str_index` | any | `idx` | `prev_strx` moved |
  | `addrx`, `addrx1`–`addrx4`, `GNU_addr_index` | any | `idx` | `prev_addrx` moved |
  | others | `decl_line`, `decl_column` (`0x3b`, `0x39`) | `line` | `prev[slot(attribute)]` moved |
  | others | `call_line`, `call_column` (`0x59`, `0x57`) | `call` | as `line` |
  | others | `decl_file`, `call_file` (`0x3a`, `0x58`) | `file` | a varint |
  | others | `high_pc` (`0x12`) | `pclen` | a varint |
  | others | any other | `data` | a varint, of `sdata` unzigzagged |

  "Moved" is by a varint of the stream (64-bit), the previous value then set to the value. Of
  `sdata`, the value is taken as a 64-bit two's complement number.

The section ends when `unit` is at its end; the references to DIEs not decoded then are set,
every stream must be at its end and the output `len` bytes long.

### 3.6 Encoding *(encoder)*

Units of versions 2 to 5 of 32-bit DWARF are taken whose DIEs parse by their tables, with every
LEB128 of the DIEs in the fewest bytes and nothing after the DIEs; the others are kept as they
are (the DIEs of those are not indexed). Strings are coded by their index where the offset is
a string's, references by the index of the DIE they point to where it is one of a unit taken,
a sibling derived where it is the end of the subtree. The encoder decodes what it made and
takes the section only where it comes back.
