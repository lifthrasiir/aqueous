# The `lz4-recons` module

This document specifies the module `lz4-recons` of aqr-formula entry formulas. The module
rebuilds an LZ4 block or LZ4 frame from the data it holds: its output is what liblz4 1.10.0
gives for that data at a fast level. The document is meant to be complete: an implementation
of the module can be written from it alone. Sections marked *(encoder)* describe the choices
this implementation makes when it builds formulas; the module does not depend on them.

The module is a compressor, so the algorithm itself is normative. The output must match
liblz4 to the byte. The algorithm below is liblz4's `LZ4_compress_generic` for a block without
a dictionary, as built for a 64-bit little-endian target with the default `LZ4_MEMORY_USAGE`
of 14, and `LZ4F_compressFrame` around it.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [Blocks](#3-blocks)
4. [Frames](#4-frames)
5. [Encoding](#5-encoding-encoder) *(encoder)*
6. [Validation](#6-validation)

## 1. Conventions

- Integers are unsigned. `le16`, `le32` and `le64` are little-endian integers.
  `rd32(p)` and `rd64(p)` read 4 and 8 bytes of the data at position `p` as `le32` and `le64`.
- **varint**: unsigned LEB128, least significant group first. A writer always uses the
  shortest form.
- **xxh32(d)**: XXH32 of `d` with seed 0.
- Arithmetic on 32-bit values (`u32`) and 64-bit values (`u64`) wraps where it is marked
  "wrapping". Everywhere else it is exact.
- The LZ4 block and frame formats are those of the LZ4 documentation
  (`lz4_Block_format.md`, `lz4_Frame_format.md`). This document only says which of the valid
  encodings is produced.

## 2. The module

- **Module**: name `lz4-recons`, version 1.
- **Params**:

  ```text
  u8      kind          0: a block, 1: a frame
  varint  A             acceleration, 1..65537
  if kind = 1:
    u8    FLG           the frame's FLG byte (§4)
    u8    BD            the frame's BD byte (§4)
  ```

  Nothing else may follow. Trailing bytes are an error.
- **Resources**: none.
- **Inputs**: exactly one, the data `D`.
- **Output**: for kind 0, the block `block(D, A)` of §3.3. For kind 1, the frame
  `frame(D, A, FLG, BD)` of §4. Its length must be the `len` the application declares.

## 3. Blocks

### 3.1 State

A compressor keeps its state across the blocks of a frame:

- `table`: 8192 entries of 32 bits, all zero at first. Each entry holds a position.
  Depending on the mode, either the first 4096 entries are used (the *wide* table) or all
  8192 are (the *narrow* table).
- `mode`: `cleared`, `narrow` or `wide`. Initially `cleared`.
- `offset`: a `u32`, initially 0. Positions in `table` are counted from the start of the
  first block compressed since the last reset, with a gap that §3.2 adds.

### 3.2 Preparing for a block

To compress a block of `n` bytes, the compressor picks its mode. The mode is `narrow` if
`n < 65547`, else `wide`. Then:

1. If `mode ≠ cleared`, and any of the following holds, the compressor clears `table` to
   zeros, sets `offset = 0` and sets `mode = cleared`:
   - the mode picked differs from `mode`;
   - the mode picked is `narrow` and `offset + n ≥ 65535`;
   - the mode picked is `wide` and `offset > 2^30`;
   - `n ≥ 4096`.
2. If `offset ≠ 0` and the mode picked is `wide`, then `offset += 65536`. The gap puts every
   position kept in the table more than 65535 bytes back, so none of them is matched.
3. `small` is set if the mode picked is `narrow` and `offset ≠ 0`. Positions below the block
   are then never matched.

A fresh state (§3.3) passes through these steps unchanged.

If `n = 0`, the block is the single byte `00`, or nothing fits if the limit is 0 (§3.4).
The state is otherwise unchanged. Below, `n ≥ 1`.

### 3.3 Compressing a block

`block(D, A)` is the block that a fresh state (§3.1) gives for `D` with no limit on its
size (§3.4). For `n = |D|` bytes, with `start = offset` (after §3.2) and the mode picked:

- `offset += n` and `mode` becomes the mode picked.
- `hash(p)`:
  - narrow: `(rd32(p) × 2654435761 wrapping in u32) >> 19`;
  - wide: `((rd64(p) << 24) × 889523592379 wrapping in u64) >> 52`.
- `idx(p) = start + p`. A table entry `e` is the position `e - start` of `D`.
- A table entry `e` is a *candidate* for position `ip` if all of these hold:
  - not `small` or `e ≥ start`;
  - the mode is narrow, or `e + 65535 ≥ idx(ip)`;
  - `rd32(e - start) = rd32(ip)`.
- The acceleration is `A`, clamped to 1..65537.

Sequences are written as the LZ4 block format defines them. A sequence has a token, then
literal length bytes, literals, an offset (`le16`) and match length bytes. A length field of
15 is continued by bytes of 255 and a final byte below 255. If `n < 13`, the block is only
the last literals (below), with `anchor = 0`. Otherwise:

```text
mflimit = n - 11; matchlimit = n - 5
anchor = 0
table[hash(0)] = idx(0)
ip = 1; fh = hash(1)
loop:
    # search: the first step is 1, then A, growing by 1 every 64 probes
    fwd = ip; step = 1; search = A << 6
    repeat:
        h = fh; e = table[h]
        ip = fwd; fwd += step; step = search >> 6; search += 1
        if fwd > mflimit: goto last
        fh = hash(fwd)
        table[h] = idx(ip)
    until e is a candidate for ip
    m = e - start
    # extend backwards
    while ip > anchor and m > 0 and D[ip-1] = D[m-1]: ip -= 1; m -= 1
    write a token, the literal length ip - anchor and the literals D[anchor..ip]
  match:
    write the offset ip - m
    k = the number of bytes from ip + 4 on equal to those from m + 4 on, with ip + 4 + k ≤ matchlimit
    ip += k + 4
    write the match length k (in the token, and continued)
    anchor = ip
    if ip ≥ mflimit: goto last
    table[hash(ip - 2)] = idx(ip - 2)
    h = hash(ip); e = table[h]; table[h] = idx(ip)
    if e is a candidate for ip:
        m = e - start
        write a token with literal length 0
        goto match
    ip += 1; fh = hash(ip)
last:
    write a token, the literal length n - anchor and the literals D[anchor..n]
```

The match length written is `k`, the length minus 4. Neither the offset nor the length is
ever zero or out of range, so every block is valid.

### 3.4 Limited output

In a frame (§4), a block of `n` bytes must fit in `cap = n - 1` bytes. If `cap` is at least
`n + n / 255 + 16`, there is no limit. Otherwise compression stops, and nothing fits, as soon
as one of these checks fails. In each check, `w` is the number of bytes written so far.

- before the literals of a sequence, with the token written:
  `w + L + 8 + L / 255 ≤ cap`, where `L` is the literal length;
- after the offset of a match is written: `w + 6 + (k + 240) / 255 ≤ cap`;
- before the last literals: `w + R + 1 + (R + 240) / 255 ≤ cap`, where `R = n - anchor`.

The checks overestimate. A block may be refused even though it would have fitted. The table
and offset keep what was done before the stop.

## 4. Frames

`frame(D, A, FLG, BD)` is `LZ4F_compressFrame` with independent blocks at a fast level, using
the acceleration `A`. Level `l` uses acceleration `1 - l` if `l < 0`, else 1.

- `FLG` must have version `01` (bits 7-6). Bit 5 (independent blocks) must be set, bit 1
  (reserved) clear and bit 0 (dictionary ID) clear. Bit 4 enables block checksums, bit 3
  the content size and bit 2 the content checksum.
- `BD` must have bits 7 and 3-0 clear, and a block size ID `b = BD >> 4` of 4..7. Blocks
  hold at most `S = 2^(8 + 2b)` bytes.

The frame is:

1. `04 22 4D 18`, `FLG`, `BD`, then `le64 |D|` if bit 3 of `FLG` is set, then the header
   checksum: `(xxh32(FLG .. last byte before it) >> 8) & 255`.
2. With one state (§3.1), fresh at the start, for each piece `P` of `D` cut into `S` bytes
   from the start (the last piece is shorter; there is none if `D` is empty):
   - compress `P` with the limit `|P| - 1` (§3.2-§3.4). If it fits in `c` bytes, write
     `le32 c` and the block. Otherwise write `le32 (|P| | 2^31)` and `P` itself.
   - if bit 4 of `FLG` is set, `le32 xxh32(the block or P as written)`.
3. `le32 0`, then `le32 xxh32(D)` if bit 2 of `FLG` is set.

The state is kept across pieces. A piece usually clears it (§3.2), but a last piece of under
4 KiB after a wide one does not. Its table then holds the previous piece's positions, all too
far back to match. That is not the same as a fresh table, whose zeros are position 0 of the
piece.

*Notes.* `LZ4F_compressFrame` lowers the block size ID it is asked for to the smallest one
whose blocks hold all of the data. It also sets bit 5 when the data fits in one block. Both
show in the `FLG` and `BD` it writes, which are taken as they are. The `lz4` command line
writes the same frames with its default independent blocks.

## 5. Encoding *(encoder)*

`aqr_lz4::formula::encode` takes two containers at the start of a file:

- **mozlz4** (Firefox): `mozLz40\0`, `le32 n`, then a block of `n` bytes of data. The block
  ends where its data does (`block::end`). The formula is the 12-byte header, an application
  of kind 0 and what follows the block, sliced as it is.
- **LZ4 frame**: a frame without a dictionary whose header checksum holds. The formula is an
  application of kind 1 and what follows the frame, sliced as it is. Frames with linked
  blocks or written at an HC level (3 and above) are not reproduced.

The data is decoded, then compressed with accelerations 1, 2, ..., 10, 12, 16, 17, 32 and
65537, in that order. The first one whose output is the original stream is taken. If none
is, there is no formula. A block is compared sequence by sequence and a frame block by block,
so a wrong acceleration costs little. The container bytes go in a stream of class `lz4`, the
data in one of class `raw`, which aqr-compress recompresses in turn.

## 6. Validation

The module fails if:

- the params are malformed: an unknown kind, an acceleration outside 1..65537, `FLG` or `BD`
  outside §4, a missing byte or trailing bytes;
- there is not exactly one input;
- `|D|` exceeds `0x7E000000` (liblz4's `LZ4_MAX_INPUT_SIZE`), or the output is not `len`
  bytes long.
