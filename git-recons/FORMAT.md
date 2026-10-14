# The `gitr` container format

This document specifies the files written by `gitr c` (`git_recons::container::compress`
over `git_recons::deconstruct`). A decoder can be written from it alone. Sections marked
*(encoder)* describe the choices this implementation makes; a decoder does not need them.

The format stores a directory: a git checkout with its `.git` (and any other files). Every
regular file is rebuilt bit for bit; symlinks are kept as their targets; nothing else
(permissions, times, empty directories) is kept.

Contents:

1. [Conventions](#1-conventions)
2. [Container](#2-container)
3. [Streams](#3-streams)
4. [The file table](#4-the-file-table)
5. [Objects and the table](#5-objects-and-the-table)
6. [Object records](#6-object-records)
7. [References](#7-references)
8. [Typed literals](#8-typed-literals)
9. [Payloads](#9-payloads)
10. [Objects, resolved](#10-objects-resolved)
11. [zlib streams](#11-zlib-streams)
12. [Files](#12-files)
13. [Encoding](#13-encoding-encoder) *(encoder)*
14. [Validation](#14-validation)

## 1. Conventions

- Integers are unsigned unless marked signed. `u8` is a byte, `u16le`/`u64le` little-endian
  integers, `be32`/`be64` big-endian (git's own fields).
- **varint**: unsigned LEB128, least significant 7 bits first, bit 7 set on every byte but
  the last; at most 10 bytes. A writer uses the shortest form.
- **svarint**: a signed `v` as the varint of `zz(v) = (v << 1) ^ (v >> 63)`, read back as
  `unzz(u) = (u >> 1) ^ -(u & 1)`.
- An **id** is 20 bytes (a SHA-1 object name). **hex** is its 40 lowercase hexadecimal
  digits. `sha1(x)` is SHA-1; **xxh3** is XXH3-64 with seed 0; **crc32** is the CRC-32 of
  zlib (git's index CRCs).
- **zstd** is Zstandard data (RFC 8878). **zlib** means the zlib format (RFC 1950) and, in
  [§11](#11-zlib-streams), the output of zlib 1.3.1's `deflate.c`.
- Git's pack and loose object formats are those of git's `Documentation/gitformat-pack.txt`;
  this document restates what it relies on.
- A **part** of a stream is a contiguous range of it, read from front to back; reading past
  its end is an error.

## 2. Container

```text
magic 69 04 7f 82 2b 43 ("aQR/gitD" in base64) | u16le version (1)
NSTREAMS = 10 streams, in the order of §3, each:
    varint(c << 1 | z)  [varint(n) if z]  c bytes
varint(symlinks), each: varint(len) path, varint(len) target
per file of the file table, in its order: u64le xxh3 of the file
```

A stream with `z = 0` is its `c` bytes; with `z = 1` they are zstd data that decompress to
exactly `n` bytes. After the streams come the symlinks (paths relative to the root,
`/`-separated, UTF-8; targets as bytes), then one hash per file: each rebuilt file must have
that xxh3, and the container must end there. Any other magic or version is an error.

*(encoder)* Every stream longer than 64 bytes is compressed with zstd at level 19 (`-l`),
long-distance matching with a 2^27 window for streams over 2 MiB, and libzstd's workers
(whose output does not depend on their number). It is stored as is when that is not
smaller.

## 3. Streams

| # | name | holds |
|---|---|---|
| 0 | `files` | the file table ([§4](#4-the-file-table)) |
| 1 | `sha` | the object ids, 20 bytes each: the **table** |
| 2 | `entry` | object records ([§6](#6-object-records)); id positions of git-dir files ([§12.5](#125-git-git-dir-files)) |
| 3 | `ops` | delta instructions ([§9.1](#91-deltas)) |
| 4 | `refs` | references ([§7](#7-references)) |
| 5 | `blob` | plain literals: blob content, blob delta inserts |
| 6 | `tree` | tree literals |
| 7 | `commit` | commit and tag literals |
| 8 | `raw` | verbatim bytes |
| 9 | `text` | git-dir files without their ids |

Every file takes a part of each of streams 2-9 (possibly empty), in file order; every object
record takes a part of each of streams 3-9 within its file's. The parts follow each other:
the first file's part of a stream starts at 0, and the parts of all files cover the stream
exactly. The `sha` stream's length must be a multiple of 20; ids are numbered from 0 (the
**table index** `j`).

## 4. The file table

```text
varint(n)
n file records, in path order
```

A file record:

```text
u8 kind
varint(common) varint(len) len bytes        -- the path, front-coded
fields of the kind
varint(mask)  varint(length) for each set bit k (0-7), of stream 2 + k
```

The path is the first `common` bytes of the previous file's path (the empty string before the
first), then the `len` bytes. Paths are `/`-separated, relative to the root, UTF-8, and
strictly increasing as byte strings. A LOOSE record's path is completed by its id
([§4.1](#41-kinds)). The mask's bits above 7 must be 0; a stream whose bit is clear has an
empty part.

### 4.1 Kinds

| kind | name | fields | file |
|---|---|---|---|
| 0 | RAW | | its `raw` part ([§12.1](#121-raw-same-blob)) |
| 1 | SAME | svarint `d` | the same bytes as file `i + d` ([§12.1](#121-raw-same-blob)) |
| 2 | BLOB | svarint `d` | a blob of the table ([§12.1](#121-raw-same-blob)) |
| 3 | BLOB_CRLF | svarint `d` | that blob with LF as CR LF |
| 4 | LOOSE | svarint `d` | a loose object ([§12.2](#122-loose-loose-objects)) |
| 5 | PACK | varint version, varint count, varint size, u8 level, u8 trailer | a pack ([§12.3](#123-pack-packs)) |
| 6 | IDX | svarint `d` | the .idx of pack file `i + d` ([§12.4](#124-idx-and-rev-pack-indexes)) |
| 7 | REV | svarint `d` | the .rev of pack file `i + d` |
| 8 | GIT | u8 flags | a git-dir file with ids ([§12.5](#125-git-git-dir-files)) |

`i` is the file's index in the table. For SAME, file `i + d` must exist, differ from `i`, and
not be SAME; for IDX and REV it must be PACK.

BLOB and BLOB_CRLF carry a table index `j = prev_blob + 1 + d`, where `prev_blob` is the `j`
of the previous BLOB or BLOB_CRLF record (−1 before the first). LOOSE carries `j = prev_loose
+ 1 + d` likewise, over LOOSE records. `j` must be below the number of ids. A LOOSE path, as
read, is its directory (it ends with `objects/`); the full path is that, then the first two
hex digits of id `j`, `/`, and the other 38.

PACK: `version` (a u32), `count` (at most 2^32 − 1) entries, `size` the pack's length in
bytes (a hint: a decoder may allocate it), `level` 1-9 (the zlib level of entries with mode
DEFAULT, [§6](#6-object-records)), `trailer` 0-2 ([§12.3](#123-pack-packs)).

## 5. Objects and the table

Objects are the **entries** of PACK files (their records, in file order, then in pack
order) and the LOOSE files. Each has a table index `j`:

- A loose object's is in its file record.
- A pack entry's is the next **new** index unless its record says DUP: pack entries take
  `0, 1, 2, …` in order (across all PACK files), and a DUP entry instead names an earlier
  one ([§6](#6-object-records)).

The **location** `loc(j)` of an index is the first pack entry, in the order above, that took
`j` as new; if none, the first LOOSE file (in file order) with that `j`; if none either, `j`
has no location (it is only a name).

*(encoder)* The table holds the ids of the packs' entries (packs in path order, entries by
offset), each at its first occurrence, then those of loose objects (in path order) not seen
in packs. Ids come from the packs' .idx files; they are names, not checked against the
content (a decoder never hashes an object). A pack takes part only with a version 2 .idx
whose offsets are consistent with it; other packs and indexes are GIT files.

## 6. Object records

A record is in its file's `entry` part (a pack's records one after another, filling it; a
loose object's single record):

```text
varint h
svarint dup                   if h & DUP
-- type 0:
varint dist
-- otherwise:
varint size
varint dist                   if type = 6
varint base_size              if h & XBASE
u8 level                      if zmode = 1
-- always:
u8 mask   varint(length) for each set bit k (0-6), of stream 3 + k
```

| bits of `h` | |
|---|---|
| 0-2 | type: 0 (whole entry verbatim), 1 commit, 2 tree, 3 blob, 4 tag, 6 ofs-delta, 7 ref-delta; 5 is invalid |
| 3-4 | zmode: 0 DEFAULT (zlib at the file's level: the PACK's, 1 for a loose object), 1 LEVEL (zlib at `level`, 1-9), 3 VERBATIM (the zlib stream in `raw`); 2 is invalid |
| 5 | DUP: the table index is `next + dup`, where `next` is the index the entry would take as new; it must be below `next` (pack entries only) |
| 6 | XBASE: the delta's base size is `base_size` |
| 7 | PLAIN: the delta is in `raw` as it is |
| 8 | UNTYPED: the delta's literals are plain |
| 9+ | must be 0 |

The lengths give the record's parts of streams 3-9; they follow each other within the file's
parts. A pack's records need not cover its parts entirely: the `raw` part may end with the
trailer ([§12.3](#123-pack-packs)). A loose object's lengths must be its file's.

**size** is what the object claims: for types 1-4 its size; for a delta the target size of
the delta. **dist** of an ofs-delta: its base is the entry `dist` (≥ 1) places before it in
the same pack.

**Type 0** records hold the whole entry in their `raw` part, as it is. Their `dist`: for an
ofs-delta (by the entry's own header) whose base offset is an entry start, the base is
`dist − 1` places before (and `dist` is that + 1); for a ref-delta whose base id is in the
table at `jb`, `dist = zz(jb − j) + 1`, and id `jb` must equal the entry's base id; 0
otherwise (then the base has no location). Loose objects have types 1-4 only.

**The base** of a delta entry `e`: for type 6 the entry `dist` before; for type 7 the base
reference, the first reference of its `refs` part ([§7](#7-references), binary, with
`cur = j`), and the base is `loc` of that index, none for an id not in the table.

**The claimed size** of an entry: its `size`, or for type 0 what its bytes claim: the header
size for types 1-4; for a delta, the target size read from the start of its inflated data
(two LEB128 numbers, the second), 0 if that cannot be read.

**The chain type** of an entry: its type if 1-4 (for type 0, the type in its header), else
the chain type of its base.

## 7. References

A reference names an id, coded against a cursor `cur` (an index): for objects `cur` starts
at the object's own `j`; for a git-dir file at 0. After each reference to the table, `cur` is
that index.

- **Binary reference** (a varint `v`): `v = 0`: an id not in the table, its 20 bytes follow
  in `refs`; else the table index `cur + unzz(v − 1)` (which must be valid).
- **Hex reference** (a varint `v`): `v = 0`: no id here (see [§8](#8-typed-literals)); `v =
  1`: an id not in the table, 20 bytes follow; else the index `cur + unzz(v − 2)`.

## 8. Typed literals

The bytes of an object, a whole one or a delta's inserts, are coded through a state machine
that sees every byte of the target in order: literal bytes as they are decoded, and bytes a
delta copies as they are copied. Its kind comes from the object type: tree (2) → TREE, commit
(1) and tag (4) → COMMIT, blob (3) → PLAIN. Literal bytes come from the kind's stream (`blob`
for PLAIN, `tree`, `commit`), references from `refs`.

A **literal run** of `n` bytes is decoded as follows, while fewer than `n` bytes have been
produced, with `left` the bytes still to produce:

- TREE, at an id (`id_left = 20`) with `left ≥ 20`: a binary reference; its 20 bytes are
  produced.
- COMMIT, before the body, with `left ≥ 40`, when the current line so far is exactly `tree `,
  `parent ` or `object `: a hex reference. An id produces its 40 hex digits. `v = 0` produces
  one byte from the literal stream instead.
- Otherwise one byte from the literal stream.

Every produced byte is fed to the state:

- TREE: `id_left` (initially 0). A byte with `id_left > 0` decrements it; a NUL byte with
  `id_left = 0` sets it to 20.
- COMMIT: `body` (initially false) and the current line so far (its length, saturating at
  255, and its first 8 bytes). Once in the body nothing changes. A LF ends the line: if the
  line was empty, the body starts; the line is reset. Any other byte extends the line.
- PLAIN: no state.

## 9. Payloads

The **payload** of a record is the object's content (types 1-4), the delta (types 6, 7), or
the content after `type size\0` for a loose object.

- Type 0: from its entry bytes ([§10](#10-objects-resolved)).
- zmode VERBATIM: the inflated zlib stream (its `raw` part); for a loose object, without its
  header.
- Types 1-4: one literal run of `size` bytes, kind by type ([§8](#8-typed-literals)), `cur =
  j`.
- Delta with PLAIN: its `raw` part.
- Other deltas: [§9.1](#91-deltas).

### 9.1 Deltas

The delta is rebuilt as git's delta format: LEB128 base size, LEB128 target size (shortest
forms), then instructions. The target size is `size`. The base size is `base_size` with
XBASE, else the base's claimed size; without XBASE the entry must have a base. The literal
kind is PLAIN with UNTYPED, else that of the base's chain type; a delta without a base must
have both XBASE and UNTYPED. With a TREE or COMMIT kind the base object is resolved
([§10](#10-objects-resolved)) to feed copied bytes to the state.

Instructions are read from `ops` until `size` bytes are produced (exactly):

- `varint(n << 1)`, `n` 1-127: an insert: the byte `n`, then a literal run of `n` bytes
  (one state for the whole delta, `cur` continuing after the base reference of a ref-delta).
- `varint(size << 1 | 1)` `svarint(o)`, `size` 1 to 2^24 − 1: a copy of `size` bytes at
  offset `prev_end + o` (0 to 2^32 − 1; `prev_end` is the end of the previous copy, 0 at the
  start). It is written in git's form: `0x80 | flags`, then the offset's nonzero bytes (bits
  0-3), the size's nonzero bytes (bits 4-6), low first; a size of 0x10000 is written with no
  size bytes. With a typed kind the copied bytes of the base are fed to the state (they must
  lie within it).

## 10. Objects, resolved

The **object** of an entry: for types 1-4 its type and payload; for a delta, the base's
object with the delta applied as git applies it (the base size must match the base, every
copy must lie within it, the result must have the target size), and the base's type. Type 0
entries are read from their bytes: the pack entry header (type, size as git reads it), an
ofs offset or 20-byte base id, then a zlib stream that inflates to the payload; their base is
as in [§6](#6-object-records).

## 11. zlib streams

A record's zlib stream (types 1-7) is, by zmode:

- DEFAULT or LEVEL at level `L`: what zlib 1.3.1 writes for the payload with `deflateInit2`
  level `L`, method 8, windowBits 15, memLevel 8, the default strategy, in one
  `deflate(Z_FINISH)` call. That is: `78 01` (`L` = 1), `78 5e` (2-5), `78 9c` (6) or `78 da`
  (7-9), the raw DEFLATE data, and the payload's Adler-32 (be32). For a loose object the
  payload is `type size\0` then the content, `type` one of `commit tree blob tag`, `size`
  in decimal.
- VERBATIM: its `raw` part.

*(implementation)* `zl.rs` produces the DEFLATE data with flate-recons' port of zlib's
`deflate_slow`/`deflate_fast`, `longest_match`, `fill_window` and `_tr_flush_block`, which is
bit-exact with zlib 1.3.1 (flate-recons' tests check it against the real library). Git's
chunked `deflate()` calls give the same bytes as one call.

## 12. Files

### 12.1 RAW, SAME, BLOB

- RAW: its `raw` part.
- SAME: the bytes of the file it names.
- BLOB: the object at `loc(j)` (which must exist and be a blob). BLOB_CRLF: the same with
  every LF byte replaced by CR LF.

### 12.2 LOOSE: loose objects

Its record's zlib stream ([§11](#11-zlib-streams)).

### 12.3 PACK: packs

```text
"PACK" | be32 version | be32 count | count entries | trailer (20 bytes)
```

Each entry, in order: for type 0, its `raw` part. Otherwise git's entry header for its type
and the payload's length (type in bits 4-6 of the first byte, the length 4 bits then 7 bits
per byte, low first, shortest form); for type 6 the distance `d` in bytes back to the base
entry's start, in git's offset encoding (bytes `b0 … bk`, bit 7 set on all but `bk`, and
`d = b0 & 127`, then `d = ((d + 1) << 7) | (bi & 127)` for each further byte); for type 7 the base id; then the zlib stream.

The trailer: mode 0, sha1 of the pack before it; mode 1, the id in the file name (the 40
hex digits after `pack-`); mode 2, the last 20 bytes of the pack's `raw` part.

### 12.4 IDX and REV: pack indexes

Both describe the pack they name, whose entries have ids `id(j)` (table order), offsets
(where each entry starts) and CRC-32s of their bytes. With the entries sorted by id:

- .idx, version 2: `ff 74 4f 63`, be32 2, 256 be32 fan-out counts (entries with first byte
  ≤ b), the ids, the CRC-32s (be32), the offsets (be32; an offset of 2^31 or more is written
  as `2^31 | k`, the k-th of a following table of be64 offsets, in this order), the pack's
  trailer, and sha1 of all that.
- .rev: `RIDX`, be32 1, be32 1, then for each entry in pack order its position in id order
  (be32), the pack's trailer, and sha1 of all that.

### 12.5 GIT: git-dir files

```text
entry part: varint(g << 1 | hex) per id, to its end
text part:  the file without its ids
refs part:  binary references, cur from 0
```

The file is built from the text part: per id, `g` bytes of text, then the id (its 40 hex
digits if `hex`, else its 20 bytes); then the rest of the text. With flag bit 0 (TRAILER),
sha1 of the result is appended. Other flag bits are 0.

## 13. Encoding *(encoder)*

### 13.1 Files

Files are classified in this order: SAME (the first file with the same bytes, for files of
32 bytes and more), PACK (with a usable .idx), LOOSE (a valid loose object), IDX and REV
(when they are exactly what [§12.4](#124-idx-and-rev-pack-indexes) gives), BLOB/BLOB_CRLF
(a nonempty file whose git blob id, of its bytes or of them with CR LF as LF, is in the
table and resolves to them; working files and `.git` files alike), GIT (any other file under
a `.git` component), RAW.

GIT: the trailer flag when the last 20 bytes are sha1 of the rest; then, scanning left to
right, a run of exactly 40 lowercase hex digits (not preceded or followed by one) that is an
id of the table, or else a 20-byte window that is one (behind a 2^24-bit filter on its first
8 bytes).

### 13.2 Entries

A pack entry is type 0 when it is not chosen ([§13.3](#133-which-zlib-streams-are-rebuilt)),
when its header is not git's shortest, its ofs base is not an entry start, its zlib stream
does not end where the entry does or inflate to its size, or no zlib level reproduces it.
Otherwise mode DEFAULT when its level is the pack's (the first reproduced entry's) and LEVEL
otherwise. A loose object is VERBATIM in the same cases.

A delta is PLAIN unless its sizes and every instruction are in git's shortest form, inserts
are 1-127 bytes and the instructions produce the target size exactly. It is XBASE when the
base's claimed size differs (or there is no base), UNTYPED when its chain type is unknown or
its base does not resolve (or a copy lies outside it).

### 13.3 Which zlib streams are rebuilt

Rebuilding a zlib stream means running zlib's search again on both sides, about as slow as
zlib itself (80-85 MB/s of data on an Apple M4). A cost model (`src/budget.rs`) estimates,
without a clock, the time of deconstruction and of reconstruction on one core of an M4, and
streams are chosen while both stay within `ns_per_byte` (default 10, 100 MB/s) times the
input's size:

- **Base**: per file, per byte of pack files, of loose objects, of the working tree, and per
  object, for each side.
- **Per stream** rebuilt: zlib's search, from statistics of the stream (bytes of the zlib
  stream, literals, matches, and how many earlier positions share each position's hash
  within the window, capped at 32), by deflate_fast (header `78 01`) or deflate_slow, by
  size class (below 16, 64, 256 KiB, beyond); times a factor, plus a constant and a cost per
  byte of data, for each side.
- **Order**: by estimated output saved per estimated time, from the headers alone: saved
  bytes are the zlib stream's size times 0.7 (commits, trees, tags), 0.4 (blobs), 0.3
  (deltas) or 0.4 (loose objects); time is a rough model of the sizes. The streams are
  offered in that order (ties by entry), each parsed for its statistics; the first that does
  not fit ends the choice (`-k`: pass over it instead).
- `-A` rebuilds every stream; `-B 0` none.

The constants are fitted with `scripts/fit-emulation.py` and `scripts/fit-budget.py` to
measurements by `examples/emucost.rs` and `examples/calib.rs`.

## 14. Validation

A decoder must reject: a wrong magic or version; streams that do not decode to their
lengths; a file table or record that runs past its stream or part, has invalid kinds,
types, modes or flags, indices or references out of range, a SAME/IDX/REV naming the wrong
kind of file, paths that are not UTF-8; parts that do not cover streams 2-9 exactly; a
delta whose instructions do not produce its size, copies outside the base, bases that do not
resolve where needed, delta chains with cycles; zlib streams of type 0 or VERBATIM records
that do not inflate; and any file whose xxh3 differs from the container's.
