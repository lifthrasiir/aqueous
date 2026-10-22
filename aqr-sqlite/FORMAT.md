# The `sqlite-recons` module

This document specifies the module `sqlite-recons` of aqr-formula entry formulas, which
rebuilds a SQLite 3 database file from the streams of a transform. It is meant to be
complete: a decoder, and an encoder whose output a decoder accepts, can be written from it
alone. Sections marked *(encoder)* describe the choices this implementation makes. A
decoder does not need them, but an encoder must follow them to produce the same streams as
this implementation.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [SQLite structures](#3-sqlite-structures)
4. [The group directory](#4-the-group-directory)
5. [Records](#5-records)
6. [Predicted index entries](#6-predicted-index-entries)
7. [Pages](#7-pages)
8. [Encoding](#8-encoding-encoder) *(encoder)*
9. [Validation](#9-validation)

## 1. Conventions

- Integers are unsigned unless they are marked signed. Signed integers are 64-bit two's
  complement. "Wrapping" arithmetic is modulo 2^64. All other arithmetic is exact, and a
  result out of the stated range is an error.
- `u8` is a byte. `be16` and `be32` are big-endian integers (SQLite's own fields).
- **varint**: unsigned LEB128. Each byte holds 7 bits, least significant group first, and
  bit 7 is set on every byte except the last. A reader takes at most 10 bytes and drops
  bits beyond 64. If the 10th byte still has bit 7 set, the input is malformed. A writer
  always uses the shortest form.
- **svarint**: a signed value `v` written as the varint of `zz(v) = (v << 1) ^ (v >> 63)`
  (arithmetic shift). It is read back as `unzz(u) = (u >> 1) ^ -(u & 1)`.
- **SQLite varint**: SQLite's own variable-length integer ([§3.1](#31-sqlite-varints)).
  It is distinct from the varint above.
- **xxh3** means XXH3-64 with seed 0 and the default secret.
- A **stream** is a sequence of bytes that a decoder reads from front to back. Reading past
  its end is an error.
- Page numbers start at 1. Page `p` occupies bytes `(p-1)·P .. p·P` of the output, where
  `P` is the page size.

## 2. The module

### 2.1 Interface

- **Module**: name `sqlite-recons`, version 1.
- **Params**:

  ```text
  varint  P           page size: a power of two, 512..65536
  varint  U           usable size of a page: 480..P
  u8      fmt4        nonzero: the database's schema format is 4 or later
  ```

  Nothing else; trailing bytes are an error.
- **Inputs**: exactly 18 slots, slot `i` being stream `i` of §2.2, with the layout given
  there. An empty stream is an empty slot. Their classes are `sqlr/<name>`: `sqlr/kind`,
  `sqlr/raw`, `sqlr/dir`, `sqlr/rec`, `sqlr/rowid`, `sqlr/rawpay`, `sqlr/tag`, `sqlr/int`,
  `sqlr/real`, `sqlr/len`, `sqlr/bytes`, `sqlr/grp`, `sqlr/phdr`, `sqlr/child`, `sqlr/ikey`,
  `sqlr/cptr`, `sqlr/ovf`, `sqlr/fill`.
- **Output**: `len` bytes, the length the application declares for it.

`np = floor(len / P)` is the number of pages and must be at least 1. `tail = len - np·P`
is the number of trailing bytes after the last whole page.

### 2.2 Streams

Each stream can be parsed with the help of the streams before it only. The last column
names what determines a stream's structure, meaning how many items it holds and of what
kind.

| # | Name | Contents | Structure from |
|---|---|---|---|
| 0 | `kind` | One `u8` per page: the page's kind ([§7.1](#71-page-kinds)) | itself (`np` items) |
| 1 | `raw` | The bytes of raw pages, `P` each, in page order; then the `tail` bytes | 0 |
| 2 | `dir` | The group directory ([§4](#4-the-group-directory)) | itself |
| 3 | `rec` | Per group: per record, its code ([§5.2](#52-record-codes)) | 2 |
| 4 | `rowid` | Per group: rowid deltas (tables), rank deltas (predicted entries) | 2, 3 |
| 5 | `rawpay` | Per group: payloads of records stored raw | 2, 3 |
| 6 | `tag` | Per group and column: a tag per value ([§5.3](#53-values)) | 2, 3 |
| 7 | `int` | Per group and column: integers and decimal mantissas | 2, 6 |
| 8 | `real` | Per group and column: reals stored as 4 or 8 bytes | 2, 6 |
| 9 | `len` | Per group and column: lengths of text and blobs, or repeat distances | 2, 6 |
| 10 | `bytes` | Per group and column: contents of text and blobs | 2, 6, 9 |
| 11 | `grp` | Per b-tree page of kind 2, 10, 13: its group (varint) | 0 |
| 12 | `phdr` | Per b-tree page: first freeblock, cell count, fragmented bytes, content start ([§7.2](#72-b-tree-pages)) | 0 |
| 13 | `child` | Child page numbers of interior pages, as deltas | 0, 12 |
| 14 | `ikey` | Keys of table interior cells, as deltas | 0, 12 |
| 15 | `cptr` | Per cell: where it sits on its page ([§7.3](#73-cell-placement)) | 12; the cell sizes the codes refer to need 0-14 |
| 16 | `ovf` | Overflow page numbers, as deltas | 0-12 |
| 17 | `fill` | Every byte that no other stream determines ([§7.5](#75-gaps-and-the-fill-stream)) | 0-16 |

Streams 3 to 10 are the **group streams**. The directory splits each of them into one
consecutive **part** per group ([§4](#4-the-group-directory)). Streams 6 to 10 are further
split into one part per column of the group.

### 2.3 Decoding outline

1. Take the streams from the inputs (§2.2).
2. Read the directory, and split the group streams into parts (§4).
3. For each table group that index groups are predicted from, decode all of its records and
   order its rows for each of those indexes (§6).
4. Rebuild pages 1 to `np` in order (§7). A b-tree page takes the next records of its group,
   decoding them as needed (§5). Each group's records are taken in the order they are coded.
5. Append the `tail` bytes from `raw`.
6. Check that every stream and every part is used up exactly (§9).

The output starts as `len` zero bytes. Pages are written as described in §7. A later write
to the same page replaces an earlier one. A page that is never written stays zero.

## 3. SQLite structures

This section restates the parts of the
[SQLite file format](https://www.sqlite.org/fileformat2.html) that the coding relies on, as
this format defines them.

### 3.1 SQLite varints

A SQLite varint is 1 to 9 bytes, big-endian. Each of the first 8 bytes contributes its low
7 bits and continues if bit 7 is set. A 9th byte, if reached, contributes all 8 bits.
Values are 64-bit, and signed values (rowids, keys) are their two's complement bit patterns.

The **minimal** form of `v` is the only form that this format writes and the only form that
the encoder accepts:

- `len(v) = 9` if `v ≥ 2^56`. Otherwise `len(v) = max(1, ceil(bitlen(v) / 7))`, with
  `bitlen(0) = 0`.
- For `len(v) ≤ 8`: the bytes are the 7-bit groups of `v`, most significant first, with
  bit 7 set on all but the last.
- For `len(v) = 9`: the first 8 bytes are the 7-bit groups of `v >> 8`, each with bit 7
  set, and the 9th byte is `v & 0xff`.

### 3.2 Serial types and records

| Serial type `st` | Value | Content size |
|---|---|---|
| 0 | NULL | 0 |
| 1, 2, 3, 4, 5, 6 | integer, big-endian two's complement | 1, 2, 3, 4, 6, 8 |
| 7 | real: IEEE 754 binary64, big-endian | 8 |
| 8, 9 | the integer 0 or 1 | 0 |
| 10, 11 | (reserved, never valid here) | — |
| even, ≥ 12 | blob | `(st - 12) / 2` |
| odd, ≥ 13 | text | `(st - 13) / 2` |

`size(st)` is the content size from this table. "Integer serial types" are 1 to 6, 8 and 9.
The **value** of an integer serial type is: 0 for 8, 1 for 9, and otherwise the content
read as a sign-extended big-endian integer.

**Canonical serial type** of an integer `v` (what SQLite picks). If `fmt4` is set and `v` is
0 or 1, it is `8 + v`. Otherwise let `u = v` if `v ≥ 0`, else `u = !v` (bitwise not). Then
the canonical type is 1 if `u ≤ 0x7f`, 2 if `u ≤ 0x7fff`, 3 if `u ≤ 0x7fffff`, 4 if
`u ≤ 0x7fffffff`, 5 if `u ≤ 0x7fffffffffff`, and 6 otherwise. The content of an integer
`v` under a serial type with size `k` is the low `k` bytes of `v`, big-endian.

A **record** with serial types `st_1..st_c`:

```text
SQLite varint H              header length in bytes, including this varint
SQLite varint st_1 .. st_c
content_1 .. content_c       size(st_i) bytes each
```

`H` follows from `s = Σ len(st_i)`: let `k = len(s + 1)`. Then `H = s + k` if
`len(s + k) = k`, and `H = s + k + 1` otherwise.

A **strict parse** of a payload accepts it as a record only if all of the following hold.
The encoder uses strict parses (§8), and so does §6.2.

- The header length and every serial type are minimal SQLite varints.
- `H ≤` the payload length.
- No serial type is 10 or 11 or above `2^32 - 1`.
- The serial-type varints are read within the header (none may run past offset `H`), they
  end exactly at `H`, and `H` is the value the formula above gives for them.
- The contents end exactly at the end of the payload.

### 3.3 B-tree pages

A page's b-tree header is at offset `h = 100` on page 1 (after the database header) and at
`h = 0` on every other page:

| Offset | Size | Field |
|---|---|---|
| `h` | 1 | page type: 2 (index interior), 5 (table interior), 10 (index leaf), 13 (table leaf) |
| `h+1` | 2 | first freeblock, `be16` |
| `h+3` | 2 | number of cells `nc`, `be16` |
| `h+5` | 2 | start of the cell content area, `be16` (0 means 65536) |
| `h+7` | 1 | fragmented free bytes |
| `h+8` | 4 | right child page, `be32` (interior pages only) |

The header is `hl = 12` bytes on interior pages (types 2 and 5) and 8 bytes on leaf pages.
It is followed by `nc` cell pointers (`be16` page offsets). `ptr_end = h + hl + 2·nc`.

Cells:

| Type | Cell |
|---|---|
| 13 | SQLite varint `plen`, SQLite varint rowid, local payload, [`be32` first overflow page] |
| 5 | `be32` left child, SQLite varint key |
| 10 | SQLite varint `plen`, local payload, [`be32` first overflow page] |
| 2 | `be32` left child, SQLite varint `plen`, local payload, [`be32` first overflow page] |

The **local** part of a payload of `plen` bytes, with `U` the usable size and integer
division throughout:

```text
X = U - 35                              on table leaves (type 13)
X = (U - 12) · 64 / 255 - 23            otherwise
M = (U - 12) · 32 / 255 - 23
local = plen                            if plen ≤ X
        K = M + (plen - M) mod (U - 4)  otherwise, and then
        local = K if K ≤ X, else M
```

When `local < plen`, the cell ends with the first overflow page number. The rest of the
payload continues on `ceil((plen - local) / (U - 4))` overflow pages. Each overflow page
holds a `be32` next page number (0 on the last page), then up to `U - 4` payload bytes.

## 4. The group directory

The records of every b-tree form a **group**: either the rows of a table (with rowids) or
the entries of an index. Stream 2 (`dir`) is:

```text
varint  G                  number of groups
G × group entry
```

Group entry:

```text
u8      table              nonzero: a table group; 0: an index group
varint  t                  0: none; else the records may be predicted from group t - 1 (§6)
if t ≠ 0:
  varint  m                number of key columns, ≤ 65536
  m × (varint col, u8 flags)
varint  rec_len            lengths of the group's parts of streams 3, 4, 5
varint  rowid_len
varint  rawpay_len
varint  ncols              number of columns
ncols × column entry
```

Column entry:

```text
varint  tag_len            length of the column's part of stream 6
varint  mode               integer coding (§5.4), < 72
varint  int_len            length of the column's part of stream 7
varint  real_len           ... stream 8
varint  len_len            ... stream 9
varint  bytes_len          ... stream 10
```

Parts are taken from the front of each group stream, group by group and, within a group,
column by column. The first group's part of `rec` is the first `rec_len` bytes of stream 3,
the second group's part follows it, and so on. In streams 6 to 10, column 0 of group 0
comes first, then column 1 of group 0, and so on. The parts must cover each group stream
exactly. `dir` must end after the last group entry.

Key columns (`t ≠ 0`):

- `col = 0` means the rowid. Otherwise the key column is table column `col - 1`. `col` must
  be below `2^32`.
- `flags` bit 0 (`DESC`) reverses the column's order. Bit 1 (`NOCASE`) compares text
  ignoring ASCII case. Other bits must be 0.
- Group `t - 1` must exist and must be a table group whose own `t` is 0. The group that
  refers to it must be an index group. Several index groups may refer to the same table
  group.

## 5. Records

### 5.1 State

Each group keeps the following state, all starting at 0 or empty:

- `prev_rowid` and `prev_rank`, signed.
- Per column: `prev`, signed, and `distinct`, a list of byte strings.

A record is decoded from the group's parts. In table groups, decoding also yields the
record's rowid.

### 5.2 Record codes

1. In a table group, first read `d` = svarint from `rowid`, and set
   `prev_rowid = prev_rowid + d` (wrapping). The record's rowid is `prev_rowid`.
2. Read `code` = varint from `rec`:
   - `code = 0`: **raw**. Read `n` = varint from `rec`. The payload is the next `n` bytes
     of `rawpay`.
   - `code = 1`: **predicted** (index groups with `t ≠ 0` only). Read `d` = svarint from
     `rowid`, and set `rank = prev_rank + 1 + d` (wrapping) and `prev_rank = rank`.
     `rank` must index the order of §6.3, and its row must be complete (§6.2). The payload
     is the entry built from that row (§6.4).
   - `code ≥ 2`: **by columns**. The record has `c = code - 2` columns, and `c` must not
     exceed the group's `ncols`. Its values come from columns `0..c-1` of the group (§5.3),
     and it is assembled as in §5.5.

### 5.3 Values

Values are decoded column by column, from column 0 to column `c - 1`. A record-wide signed
`last` starts at 0. For column `j`, read one `tag` byte from its part of `tag`:

| Tag | Value | Serial type | Reads |
|---|---|---|---|
| 0 | NULL | 0 | — |
| 1 | integer `v = int(0)` | canonical for `v` (§3.2) | `int`; sets `last = v` |
| 2 | real, raw | 7 | 8 bytes from `real`: the content |
| 3 | text of `n` bytes | `13 + 2n` | `n` = varint from `len` (`n < 2^32`); `n` bytes from `bytes` |
| 4 | blob of `n` bytes | `12 + 2n` | as tag 3 |
| 5 | real from a float | 7 | 4 bytes from `real` |
| 6 | text, repeated | `13 + 2n` | `d` = varint from `len` |
| 7 | blob, repeated | `12 + 2n` | as tag 6 |
| 16 + s, for s in 1..6, 8, 9 | integer `v = int(0)`, serial type `s` | `s` | `int`; sets `last = v` |
| 32 + e, for e in 0..22 | decimal: mantissa `m = int(e)` | 7 | `int`; sets `last = m` |

Any other tag is an error. Details:

- **Integers** (tags 1, 17..22, 24, 25). The content is the low `size(s)` bytes of `v`,
  big-endian. Tags 24 and 25 (serial types 8 and 9) still read `v` from `int` and update
  `prev` and `last` like any other integer. There is no content, and `v` is not checked.
- **Tag 5**. The 4 bytes are a big-endian IEEE 754 binary32. The content is that value
  converted exactly to binary64, written as 8 big-endian bytes.
- **Decimals** (tags 32..54). Convert `m` to binary64, rounding to nearest with ties to
  even. Divide by `10^e` (exact in binary64 for `e ≤ 22`), with IEEE division rounding to
  nearest, ties to even. The content is the 8 big-endian bytes of the result.
- **Text and blobs** (tags 3 and 4). The content is the `n` bytes. If `n ≥ 2`, the
  content is also appended to the column's `distinct` list.
- **Repeats** (tags 6 and 7). `id = |distinct| - 1 - d` must be ≥ 0. The content is
  `distinct[id]` and `n` is its length. Nothing is appended. Text and blobs share one
  `distinct` list per column, so the tag alone decides between text and blob.

### 5.4 Integer coding

`int(e)` reads the next svarint `d` from the column's part of `int`. The column's `mode`
from the directory gives two numbers:

- `k = mode mod 3`, the predictor.
- `S = floor(mode / 3)`, and `scale = S - 1` if `S ≥ 1`, else 0.

```text
x = d + 0        if k = 0
x = d + prev     if k = 1       (prev: the column's previous x)
x = d + last     if k = 2       (last: the record's previous integer or mantissa, see §5.3)
prev = x                        (all additions wrapping)
if scale = 0:  result = x
else:          q = scale - e, which must be in 0..18
               x must be a multiple of 10^q (as a signed value)
               result = x / 10^q
```

`prev` is updated with `x` before scaling. `last` takes the result after scaling. Integer
tags call `int(0)`.

### 5.5 Assembly

A by-columns record with values of serial types `st_1..st_c` and contents
`content_1..content_c` is the record of §3.2: its header length from the formula, the
minimal SQLite varints of the serial types, then the contents.

## 6. Predicted index entries

An entry of an index on a rowid table holds the indexed columns of a row followed by its
rowid. When an index group has `t ≠ 0`, its predicted entries are built from the rows of
table group `T = t - 1`.

### 6.1 Rows

Before any page is rebuilt, decode all records of `T` in order (§5), until its `rec` part
is used up (every record reads at least one byte of it). This gives rows
`0..N-1`, each with a payload and a rowid. These records are the same ones that the pages
later take from `T`. Decoding them again from the start, or keeping them, gives the same
result. `T` may not contain predicted records itself.

### 6.2 Key values

For each row `r` and key column `j` (column number `col_j`, which is `ROWID` when the
directory has `col = 0`), the **key value** `(st, content)` is:

- If `col_j = ROWID`: serial type 6, and the 8 big-endian bytes of the row's rowid.
- Otherwise, if the row's payload passes a strict parse (§3.2) and has more than `col_j`
  columns: that column's serial type and content.
- Otherwise: NULL (serial type 0).

A row is **complete** if its payload passes a strict parse and every non-`ROWID` key column
is within its columns. Only complete rows can be predicted, but every row takes part in the
order.

### 6.3 Index order

The rows of `T` are sorted by the comparison below. Its last step compares row numbers, so
the order is total and unique. Position `i` in the sorted list is **rank** `i`.

```text
cmp(a, b):
  for j in 0..m-1:
    o = key_cmp(flags_j, value(a, j), value(b, j));  if o ≠ equal: return o
  o = compare rowid(a), rowid(b) as signed integers;  if o ≠ equal: return o
  return compare a, b
```

`key_cmp(f, x, y)`:

1. If `f` has `NOCASE`, and both serial types are odd and ≥ 13 (text), compare the contents
   after mapping bytes `A`..`Z` to `a`..`z`, lexicographically by unsigned byte, with a
   proper prefix ordered first.
2. Otherwise, use `value_cmp(x, y)`.
3. If `f` has `DESC`, reverse the result.

`value_cmp(x, y)`:

- Classes order first: NULL (0) < numeric (1..9) < text (odd, ≥ 13) < blob (even, ≥ 12).
- NULL equals NULL. Text against text, and blob against blob, compare lexicographically by
  unsigned byte, with a proper prefix ordered first.
- Two integers (any integer serial type, including the rowid's 6) compare by value.
- Two reals `x`, `y` compare numerically if neither is NaN, so that −0.0 equals +0.0.
  Otherwise they compare by IEEE 754 `totalOrder`: −NaN < −∞ < … < −0.0 < +0.0 < … < +∞ <
  +NaN, with NaNs ordered by payload.
- An integer `i` against a real `r` uses `int_real(i, r)`. A real against an integer is the
  reverse of that.

```text
int_real(i, r):
  r is NaN:     greater                    (i > r)
  r < -2^63:    greater
  r ≥ 2^63:     less
  y = r truncated toward zero, as an integer
  i ≠ y:        compare i, y
  else:         compare (i converted to binary64, to nearest, ties to even) with r numerically
```

### 6.4 The entry

The entry predicted from row `r` with rowid `R` has `m + 1` columns. Let `rst` be the
canonical serial type of `R` (§3.2).

- Key column `j` with `col_j = ROWID`: serial type `rst`, not the 6 used for ordering in
  §6.2, content the low `size(rst)` bytes
  of `R`.
- Any other key column: the key value's serial type and content (§6.2).
- Last column: serial type `rst`, content the low `size(rst)` bytes of `R`.

The entry is assembled as a record (§3.2).

## 7. Pages

Pages `p = 1..np` are rebuilt in order. Signed state shared by all pages starts at 0:
`prev_child`, `prev_ikey` and `last_ovf`.

### 7.1 Page kinds

Read the kind `k` from `kind`:

| `k` | Kind | Rebuilt as |
|---|---|---|
| 0 | raw | The next `P` bytes of `raw` |
| 1 | zero | `P` zero bytes (nothing is written) |
| 3 | overflow | Nothing now. The page is written by the cell that owns it (§7.4) |
| 2, 5, 10, 13 | b-tree page of that type | §7.2 |

Any other kind is an error.

### 7.2 B-tree pages

Let `h` be 100 on page 1 and 0 otherwise. On interior pages (`k` = 2 or 5), `hl = 12`. On
leaf pages, `hl = 8`. The page is built in a buffer of `P` bytes.

1. If `k ≠ 5`, read `g` = varint from `grp`. `g` must be below `G`, and group `g` must be a
   table group exactly when `k = 13`.
2. Read `fb`, `nc` and `frag` as varints from `phdr`. They must satisfy `fb ≤ 0xffff`,
   `nc ≤ 0xffff` and `frag ≤ 0xff`. `ptr_end = h + hl + 2·nc` must be `≤ U`. Set
   `page[h] = k`, `be16 page[h+1] = fb`, `be16 page[h+3] = nc` and `page[h+7] = frag`.
3. Start a placement list (§7.3), and set `minoff = U`. For each cell `i = 0..nc-1`, build
   its bytes:
   - If interior: `c = prev_child + 1 + svarint(child)`, which must be in `0..2^32-1`. Set
     `prev_child = c` and append `be32 c`.
   - If `k = 5`: `key = prev_ikey + svarint(ikey)` (wrapping). Set `prev_ikey = key` and
     append the SQLite varint of `key`.
   - Otherwise, take the next record of group `g` (§5), with payload `pay` of `plen` bytes
     and, if `k = 13`, rowid `R`. Append the SQLite varint of `plen`, then, if `k = 13`,
     the SQLite varint of `R`. Compute `local` (§3.3, the table-leaf rule if `k = 13`) and
     append `pay[0..local]`. If `local < plen`, read the overflow chain: for each of
     `ceil((plen - local) / (U - 4))` pages, `q = last_ovf + 1 + svarint(ovf)`, which must
     be in `2..np`, and set `last_ovf = q`. Append `be32` of the chain's first page.

   Let `size` be the cell's length. Read the cell's offset from `cptr` (§7.3). It must
   satisfy `off ≥ ptr_end` and `off + size ≤ U`. Write the cell at `off`, write
   `be16 off` at `h + hl + 2i`, and set `minoff = min(minoff, off)`. Then write the cell's
   overflow pages (§7.4).
4. If interior, read the right child `c` like a left child (same state) and write `be32 c`
   at `h + 8`.
5. `cs = minoff + svarint(phdr)` must be in `1..65536`. Write `be16 (cs mod 65536)` at
   `h + 5`.
6. Fill the gaps (§7.5) and write the page.

### 7.3 Cell placement

SQLite fills the cell content area downwards, so a new cell usually ends where an earlier
one begins. A page's **open ends** are a list `E`, which starts as `[U]`. A cell's code in
`cptr` is a varint. Let `sz = max(size, 4)` and `n = |E|`:

- An even code `2r` means the cell ends at `E[n-1-r]` (the `r`-th most recent end):
  `off = E[n-1-r] - sz`. Here `r < n` and `off ≥ 0` are required.
- An odd code `2u + 1` means `off = E[n-1] - sz + unzz(u)`, which must be ≥ 0.

Then update the list:

1. Scanning from the end, remove the first element of `E` equal to `off + sz`, if any.
2. Then, if `|E| = 64`, remove `E[0]`. This check comes after step 1.
3. Append `off`.

*(encoder)* The encoder looks for `off + sz` among `E[n-1], E[n-2], …`, at most 16 of them.
It uses `2r` for the first match at distance `r`, and otherwise `2·zz(off - (E[n-1] - sz)) + 1`.

### 7.4 Overflow pages

A cell whose payload has `local < plen` owns the chain `q_0..q_{c-1}` read in §7.2. Right
after the cell is placed, its overflow pages are written in chain order, starting at
payload offset `at = local`:

```text
take = min(plen - at, U - 4)
page q_i = be32 (q_{i+1}, or 0 for the last) | pay[at .. at+take] | the next P - 4 - take bytes of fill
at = at + take
```

These `fill` bytes are read when the cell is placed. The page that holds the cell reads its
own gaps from `fill` only after all of its cells (§7.5).

### 7.5 Gaps and the fill stream

The covered intervals of a b-tree page are `[h, ptr_end)` and `[off, off + size)` for each
cell. Sorted by start, they must not overlap. Every byte of the page outside them is
copied, in increasing offset, from `fill`. This includes:

- bytes `0..100` on page 1, which hold the database header;
- free space and freeblocks;
- the bytes from `U` to `P`, which are reserved.

The `fill` stream therefore holds, page by page in page order, the overflow tails of the
page's cells in cell order, followed by the page's gaps.

### 7.6 The tail

After page `np`, the last `tail` bytes of the output are the next `tail` bytes of `raw`.

## 8. Encoding *(encoder)*

### 8.1 Eligibility

An input has a formula when it looks like a database:

- it is at least 512 bytes long and starts with `"SQLite format 3\0"`;
- the page size `be16` at offset 16 is 1 (meaning 65536) or a power of two in 512..32768;
- `U = P - byte[20]` is at least 480;
- the input holds at least one page.

`fmt4` is `be32` at offset 44 being ≥ 4. Otherwise, or when verification fails, or when
the transform fails, the input has no formula. Verification rebuilds the input from the
streams and compares it, and can be turned off. A transform failure is a panic.

### 8.2 Page classification

Pages are classified in order `p = 1..np`, and pages already claimed by an overflow chain
are skipped. Let `t = page[h]`.

- If `t` is not 2, 5, 10 or 13: the page is zero (kind 1) if all of its `P` bytes are 0,
  and raw (kind 0) otherwise.
- Otherwise, parse the page. Any failure makes it raw. Every varint must be a minimal
  SQLite varint read within `[0, U)`, and:
  - `h + hl ≤ U` and `ptr_end ≤ U`;
  - each cell pointer `off` satisfies `ptr_end ≤ off < U`, and `off + 4 ≤ U` on interior
    pages;
  - a cell with `local < plen` needs `ceil((plen - local) / (U - 4)) < np`;
  - every cell ends within `U`;
  - the intervals of §7.5 do not overlap.
- Then claim each cell's overflow chain, in cell order. Each chain page `q` must satisfy
  `2 ≤ q ≤ np`, must not be claimed yet, and must not have 2, 5, 10 or 13 as its byte 0.
  Claim it and subtract `min(left, U - 4)` from the bytes `left`. When `left` reaches 0,
  the page's `be32` next pointer must be 0. Otherwise follow that pointer. On any failure,
  release this page's claims and make the page raw. On success its kind is `t`.

After the pass, every claimed page has kind 3.

### 8.3 Groups

Two b-tree pages are connected when one is an interior page (kind 2 or 5) whose left or
right child pointer names the other. Only pointers to pages in `1..np` of a b-tree kind
count. A **tree** is a connected component. Each tree gives at most two groups: one for its
table pages (kind 13) and one for its index pages (kinds 2 and 10). Table interior pages
(kind 5) have no records.

Groups are numbered in the order of their first page in the file. A group's records are
its pages' cell payloads, in page order and cell order, with overflow payloads put
together. A table group's rowids are its cells' rowids.

### 8.4 Index prediction

This step runs only if page 1 has kind 13 or 5 and its tree has a table group (the schema).
The text encoding `enc` is `be32` at offset 56 (2 UTF-16le, 3 UTF-16be, anything else
UTF-8). An ASCII name compares equal to a text content when the content is that name in
`enc`.

1. **Schema.** For each schema record, in order, that passes a strict parse with at least 4
   columns: column 0 (type) and column 2 (`tbl_name`) must be text, and column 3
   (`rootpage`) an integer in `1..2^32-1`. If the type is `table`, record (content of column
   1, root). If it is `index`, record (content of column 2, root). Then, for each index in
   order, pair it with the first table whose name content is byte-for-byte equal.
2. **Pairs.** For each (index root `ir`, table root `tr`) in that order: both must be pages
   of a b-tree kind. Let `gi` be the index group of `ir`'s tree and `gt` the table group of
   `tr`'s tree. Skip the pair if either group is missing or `gi` already has a key.
   Otherwise discover the key. If that succeeds, group `gi` gets `t = gt + 1` and the key.
3. **Rows by rowid.** "The rows with rowid `R`" are the rows of `gt` with that rowid, in row
   order. A table can have several rows with the same rowid if it is broken.
4. **Key columns.** Sample the entries at positions `0, s, 2s, …`, with
   `s = max(floor(E / 64), 1)` and `E` the number of entries. An entry counts only if it
   passes a strict parse with at least one column, its last column is an integer `R`, a row
   has rowid `R` (take the first one), and that row passes a strict parse. The first entry
   that counts fixes `k` = its column count − 1. Later entries with another column count
   are skipped. For each counted entry and each `j < k`:
   - `rowid_hits[j]` increases by one if entry column `j` is an integer equal to the row's
     rowid;
   - `hits[j][c]` increases by one for each table column `c` with the same serial type and
     content.

   Key column `j` is the candidate with the most hits. The candidates are the table columns
   and `ROWID`. Ties go to the lowest column, and `ROWID` loses all ties. It must have more
   than half of the counted entries, or discovery fails. If no entry counts, the key has no
   columns.
5. **Flags.** Over the first 8192 entries, consider each pair of consecutive entries that
   both pass a strict parse with more than `m` columns. Find the first key column `j` where
   their values differ by `value_cmp`. For each of the 4 flag values `f`, count a
   violation when `key_cmp(f, previous, current)` is "greater". Column `j` takes the `f`
   with the fewest violations, the smallest `f` on ties.
6. **Coding.** For each entry of a group with a key: if it passes a strict parse, its last
   column is an integer `R`, and some row with rowid `R` is complete and predicts exactly
   the entry's bytes (§6.4), it is coded as predicted (code 1) with the rank of the first
   such row. Otherwise it is coded like any other record.

### 8.5 Records

A table group writes each rowid's delta from the previous one (svarint, wrapping, starting
at 0). A record that fails a strict parse is raw. Otherwise it is coded by columns, and the
group's `ncols` is the most columns of any such record. Per value:

- **NULL**: tag 0.
- **Integer** of serial type `s` and value `v`: tag 1 if `s` is canonical for `v`, else tag
  `16 + s`. `v` goes to the column's integers with exponent 0.
- **Real** (serial type 7) with bits `b` and value `x`:
  1. If `x` is finite, then for `e = 0..22`: `m = round(x · 10^e)`, where the product is
     binary64 and ties round away from zero. If `|m| ≥ 2^53`, stop. If `m / 10^e`,
     computed as in §5.3, has bits `b`, the value is a decimal: tag `32 + e`, and `m` goes
     to the integers with exponent `e`.
  2. Otherwise, let `f` be `x` rounded to binary32 (nearest, ties to even, overflowing to
     ±∞). If `f` is not NaN and converts back to bits `b`, use tag 5 with the 4 bytes of
     `f`. A NaN is never coded as tag 5, because a decoder need not keep its payload
     through binary32.
  3. Otherwise, use tag 2 with the 8 bytes.
- **Text or blob** of `n` bytes: if `n ≥ 2` and an equal value is in the column's distinct
  list, use tag 6 (text) or 7 (blob) with `|distinct| - 1 - id`. The lookup goes through a
  map from xxh3 to the latest id with that hash, then compares bytes. Otherwise, use tag 3
  or 4 with `n` and the bytes, and if `n ≥ 2`, add the value to the list.

**Integer mode.** A column's integers `v_i` (with exponents `e_i`) are coded by whichever
choice gives the fewest `int` bytes:

- `top = max e_i`. A scaled choice exists if `top > 0` and every `v_i · 10^(top - e_i)`
  fits a signed 64-bit integer with `top - e_i ≤ 18`.
- For the unscaled values, and then for the scaled ones, compute the cost of `k = 0` (the
  values) and of `k = 1` (deltas from the previous value, starting at 0, wrapping). The cost
  is the total svarint length. Take the lower cost, and `k = 0` on a tie. The scaled
  choice replaces the unscaled one only if it is strictly cheaper.
- `k = 2` uses deltas against `last` (§5.3) with unscaled values. It replaces the choice
  above only if it is strictly cheaper.

`mode = k + 3·(top + 1)` when scaled, and `k` otherwise.

### 8.6 Order of output

The directory and the group parts are written group by group, in group number order.
Within a group, the order is `rec`, `rowid`, `rawpay`, then every column's parts. Streams
0, 1 and 11 to 17 follow the page order of §7 exactly.

## 9. Validation

A decoder must reject the following rather than produce output:

- Params that are cut short, or have bytes after `fmt4`; a number of inputs other than 18;
  a stream or a part cut short anywhere (a varint, a value).
- `P` not a power of two in 512..65536, `U` outside 480..`P`, or `np = 0`.
- `kind` shorter or longer than `np`.
- The directory: `m > 65536`, `col ≥ 2^32`, flags above 3, `mode ≥ 72`, a `t` that names a
  missing group, a group that is not a table, or a group that is itself predicted, a
  predicted table group, or parts that do not cover their streams exactly.
- Records: a predicted record in a group without a key, a rank outside `0..N-1`, an
  incomplete row, `c > ncols`, an unknown tag, a text or blob length of `2^32` or more, a
  repeat distance beyond the list, or an `int` value that does not scale exactly (§5.4).
- Pages: an unknown kind, a group number out of range or of the wrong kind, header fields
  out of range, a child outside `0..2^32-1`, an overflow page outside `2..np`, a cell code
  that gives no valid offset, a cell outside `[ptr_end, U)`, overlapping intervals, or a
  content start outside `1..65536`.
- Any byte left over at the end in streams 0, 1, 11-17 or in any group part. Equivalently,
  every record of every group is taken by a page.

This implementation also bounds its work on corrupt input. It checks `kind` and the tail
before allocating the output. It also rejects a table whose key column contents, for one
index, add up to `2^32 - 1` bytes or more.
