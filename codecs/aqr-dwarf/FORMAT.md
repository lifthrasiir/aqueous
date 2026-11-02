# The aqr-dwarf modules

This document specifies the modules of aqr-formula entry formulas that rebuild sections of
DWARF debugging information, and of the tables beside it, from streams:
[`dwarf-apple`](#2-the-dwarf-apple-module), Apple's accelerator tables. The document is meant
to be complete: an implementation of the modules can be written from it alone. Sections
marked *(encoder)* describe the choices this implementation makes when it builds formulas;
the modules do not depend on them.

Contents:

1. [Conventions](#1-conventions)
2. [The `dwarf-apple` module](#2-the-dwarf-apple-module)

## 1. Conventions

- Integers are unsigned and little-endian: `u16`, `u32`. **varint**: unsigned LEB128, least
  significant group first, at most 10 bytes, read as a 64-bit value.
- `unzigzag(z) = (z >> 1) XOR −(z & 1)`. A value **moved** from `prev` by `z` is `prev +
  unzigzag(z)`, which must lie within `u32`.
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
