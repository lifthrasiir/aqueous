# The `snappy-recons` module

This document specifies the module `snappy-recons` of aqr-formula entry formulas. The module
rebuilds a Snappy stream from its data, or a LevelDB table from the contents of its blocks. Its
Snappy streams are what Snappy 1.2.2 gives at its default level (`snappy::RawCompress`,
`snappy_compress`). The document is meant to be complete: an implementation of the module can
be written from it alone. Sections marked *(encoder)* describe the choices this implementation
makes when it builds formulas; the module does not depend on them.

The module is a compressor, so the algorithm itself is normative. The output must match Snappy
to the byte. Since 1.1.10, Snappy hashes differently depending on how it was built: with the
CRC-32C instruction where the target has one (SSE4.2, ARMv8 with CRC), or by multiplication
otherwise. Both hashes are specified here (§3.2).

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [Snappy streams](#3-snappy-streams)
4. [LevelDB tables](#4-leveldb-tables)
5. [Encoding](#5-encoding-encoder) *(encoder)*
6. [Validation](#6-validation)

## 1. Conventions

- Integers are unsigned. `le16` and `le32` are little-endian integers. `rd32(p)` reads 4 bytes
  of the data at position `p` as `le32`. Arithmetic on `u32` values wraps where it is marked
  "wrapping"; everywhere else it is exact.
- **varint**: unsigned LEB128, least significant group first. A writer always uses the
  shortest form.
- **crc32c(d)**: CRC-32C (Castagnoli, reflected polynomial `0x82F63B78`), with the register
  set to `0xFFFFFFFF` at the start and inverted at the end. **crcw(c, v)** is the raw update
  that the instructions compute (`__crc32cw(c, v)` of ARMv8, `_mm_crc32_u32(c, v)` of x86): the
  register `c`, without inversions, updated by the 4 bytes of `v` in little-endian order.
- **masked(d)**: LevelDB's masked CRC: with `c = crc32c(d)`, `((c >> 15) | (c << 17)) +
  0xA282EAD8`, wrapping in `u32`.
- The Snappy format is that of Snappy's `format_description.txt`, and the LevelDB table format
  that of LevelDB's `doc/table_format.md`. This document only says which of the valid
  encodings is produced.

## 2. The module

- **Module**: name `snappy-recons`, version 1.
- **Params**: exactly 2 bytes.

  ```text
  u8      kind          0: a Snappy stream, 1: a LevelDB table
  u8      hash          0: multiplication, 1: CRC-32C (§3.2)
  ```

- **Resources**: none.
- **Inputs**:
  - kind 0: exactly one, the data `D`;
  - kind 1: exactly three, `data` (the contents of the blocks rebuilt), `layout` and `kept`
    (the blocks kept as they are), as §4.2 defines them. Their classes are `ldb/blocks`,
    `ldb/layout` and `ldb/kept`.
- **Output**: for kind 0, `snappy(D)` of §3. For kind 1, the table of §4. Its length must be
  the `len` the application declares.

## 3. Snappy streams

### 3.1 The stream

`snappy(D)` is the varint of `|D|`, which must be below `2^32`, followed by the compressed
fragments of `D`. `D` is cut into fragments of 65536 bytes from the start; the last one is
shorter, and there is none if `D` is empty. Each fragment is compressed apart (§3.3) with a
table of `T` entries of 16 bits, all zero. `T` is the smallest power of two at least the
fragment's length, but at least 256 and at most 32768.

### 3.2 Hashing

The entry of 4 bytes `v` is `(h & M) >> 1`, where `M = 2 × (T - 1)` and `h` is:

- multiplication: `(v × 0x1E35A7BD wrapping in u32) >> 16`;
- CRC-32C: `crcw(v, M)`.

### 3.3 Compressing a fragment

For a fragment `F` of `n` bytes, positions are counted from its start. Each table entry holds a
position. An *element* is written as the Snappy format defines it:

- **literal** of `L` bytes: if `L - 1 < 60`, the tag `(L - 1) << 2`. Otherwise the tag
  `(59 + c) << 2`, then `L - 1` in `c` bytes, little endian, where `c = floor(log2(L - 1) / 8) + 1`.
  Then the bytes themselves.
- **copy** of `len` bytes at offset `o`: while `len ≥ 68`, a piece of 64; then, if `len > 64`,
  a piece of 60. Then the rest, which is 4..64 bytes. A piece of `k` bytes is written as
  `1 | (k - 4) << 2 | (o >> 8) << 5` then `o & 255` if `k < 12` and `o < 2048`. Otherwise it is
  written as `2 | (k - 1) << 2` then `le16 o`.

`probe(q)` looks position `q` up: with `e = entry(rd32(q))`, it reads `c = table[e]`, sets
`table[e] = q` and is a *hit* if `rd32(c) = rd32(q)`. There is no distance check: every entry,
zero included, is a position before `q`.

If `n < 15`, the fragment is a single literal of all of it (none if `n = 0`). Otherwise, with
`limit = n - 15`:

```text
ip = 0
loop:
    next_emit = ip; ip += 1; skip = 32
    if limit - ip ≥ 16 (as signed numbers):
        for q = ip, ip + 1, ..., ip + 15:
            if probe(q) is a hit with c: ip = q; candidate = c; goto found
        ip += 16; skip += 16
    loop:
        step = skip >> 5; skip += step
        if ip + step > limit: ip = next_emit; goto remainder   # ip is not probed
        if probe(ip) is a hit with c: candidate = c; break
        ip += step
  found:
    write a literal of F[next_emit .. ip]
    loop:
        k = 4 + the number of bytes from ip + 4 on equal to those from candidate + 4 on,
            up to the end of the fragment
        write a copy of k bytes at offset ip - candidate
        ip += k
        if ip ≥ limit: goto remainder
        table[entry(rd32(ip - 1))] = ip - 1
        if probe(ip) is a hit with c: candidate = c
        else: break
remainder:
    if ip < n: write a literal of F[ip .. n]
```

## 4. LevelDB tables

### 4.1 The table

A table is a sequence of blocks, then a footer of 48 bytes. Each block is its stored bytes `S`,
its type byte `t` (0: raw, 1: Snappy), and `le32 masked(S ‖ t)`. The footer holds the handles
(`varint offset`, `varint size` of `S`) of the metaindex block, then of the index block. These
are the last two blocks. It is padded with zeros to 40 bytes and ends with `le64
0xDB4775248B80FB57`. The handles of the data blocks are the values of the index block, and
those of the meta blocks (the filter) are the values of the metaindex block.

### 4.2 Inputs

`layout` describes the blocks in order. Each one is:

```text
varint  n             bytes taken from data or kept
u8      code          0: raw, 1: Snappy, 2: kept
if code = 2:
  u8    t             the type byte
```

- code 0: `S` is the next `n` bytes of `data`, and `t = 0`.
- code 1: `S` is `snappy(P)` (§3) of the next `n` bytes `P` of `data`, and `t = 1`.
- code 2: `S` is the next `n` bytes of `kept`, and `t` is as given.

The table is the blocks of `layout`, each followed by its trailer, then the footer made from the
last two blocks. `layout` must describe at least two blocks, and `data` and `kept` must be used
up exactly.

## 5. Encoding *(encoder)*

`aqr_snappy::formula::encode` takes a LevelDB table: a file ending with the magic number whose
blocks, found from the footer, the index and the metaindex, tile it. The last two must be the
metaindex and the index.

- The hash is decided by the first Snappy block that exactly one hash reproduces. If none
  decides, multiplication is used.
- A Snappy block that the hash reproduces gets code 1. A raw block gets code 0. Any other block
  (another compressor, another type) gets code 2.
- The table is rebuilt from the inputs and must be the file. Otherwise, or if no block gets
  code 1, there is no formula.

The contents of the blocks (`data`) are recompressed in turn by aqr-compress. A LevelDB table
is known by its end, so aqr-compress takes one only as a whole file, or as the whole of what it
recompresses, never found in the middle of data. `formula::stream` makes an application of
kind 0, and `formula::find_hash` finds the hash for one.

## 6. Validation

The module fails if:

- the params are not 2 bytes of known values, or the inputs are not as many as the kind takes;
- `|D|` is `2^32` or more;
- `layout` is malformed: a truncated varint or code, an unknown code, `n` past the end of `data`
  or `kept`, fewer than two blocks, or `data` or `kept` not used up;
- the output is not `len` bytes long.
