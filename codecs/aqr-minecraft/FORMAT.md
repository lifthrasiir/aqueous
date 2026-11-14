# The `mca-recons` module

This document specifies the module `mca-recons` of aqr-formula entry formulas. The module makes
a Minecraft region file (Anvil, `.mca`) again bit for bit of its header, the data of its chunks
and a *layout* telling where each chunk is and what of it is kept as it is. A chunk's zlib
stream is made again of its data by zlib's compressor (one `deflate(Z_FINISH)` call), as
aqr-flate's `FORMAT.md` specifies it ("zlib emulation"), of the parameters of the
application's params: the module stores no DEFLATE of its own. Sections marked *(encoder)*
describe the choices this implementation makes when it builds formulas; the module does not
depend on them.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [The layout](#3-the-layout)
4. [Encoding](#4-encoding-encoder)
5. [Validation](#5-validation)

## 1. Conventions

- **varint**: unsigned LEB128, least significant group first.
- Numbers in the file are big endian.
- A **sector** is 4096 bytes. A region file is a header of two sectors, 1024 *locations*
  (4 bytes each: the first sector of a chunk in its 3 high bytes, its count of sectors in the
  low byte) then 1024 *timestamps* (4 bytes each), then sectors. A chunk at sector `s` of `c`
  sectors takes bytes `4096 s` to `4096 (s + c)`: its length `L` (4 bytes: of the compression
  type and the data), the compression type (1 byte), `L - 1` bytes of data, and the rest of
  its sectors (the *padding*).
- **The chunks** of a header, of a file of `size` bytes: the entries whose location has a
  first sector `s >= 2` and a count `c >= 1`, in the order of their first sectors (none has the
  same first sector as another, as they do not overlap: each must start at or after the end of
  the one before it, and the last must end by sector `floor(size / 4096)`). Any other location
  holds no chunk.
- **zlib(d, p)**: the zlib stream (RFC 1950) of `d` as zlib makes it with the parameters `p`:
  the header zlib writes for them (CMF `8 + ((wbits - 8) << 4)`; FLG of the level's flags, 0 for
  levels 0 and 1 and the strategies Huffman only and RLE, 1 for levels 2 to 5, 2 for 6, 3
  else, and the check bits), the DEFLATE stream of `d` (aqr-flate's zlib emulation of `p`),
  and the Adler-32 of `d`.

## 2. The module

- **Module**: name `mca-recons`, version 1.
- **Params**: 4 bytes: zlib's level (1 to 9), strategy (0 default, 1 filtered, 2 Huffman only,
  3 RLE, 4 fixed), window bits (9 to 15) and memory level (1 to 9).
- **Resources**: none.
- **Inputs**: exactly four:
  1. `header` (class `mca/header`): the file's first 8192 bytes, as they are;
  2. `layout` (class `mca/layout`): §3;
  3. `data` (class `mca/data`): the data of the chunks made of it (rebuilt and stored, §3), one
     after another;
  4. `kept` (class `mca/kept`): the bytes kept as they are, in order.
- **Output**: the region file of §3. Its length must be the `len` the application declares.

It is a part module (windows of a page for `header` and `layout`, of 16 pages for the others
and the output): it takes every input in order, in pieces, and puts the file in order, a chunk
at a time, holding the header, the chunks' locations and, for a chunk rebuilt, its data and its
sectors. Its plan follows the largest chunk rebuilt, not the file.

## 3. The layout

The layout is, for each chunk (§1) of `header` in order, its *tag* (a varint), then one byte,
`tail`. A tag's bits:

| bits | |
|---|---|
| 0-1 | how the chunk is made: 0 *kept*, 1 *rebuilt*, 2 *stored* (3 is not valid) |
| 2 | `gap`: the bytes between the end of the chunk before (or of the header) and this one's first sector are kept, else zero |
| 3 | `pad`: the chunk's padding is kept, else zero (0 for a kept chunk) |
| 4 on | `n`: the bytes of the chunk's data (0 for a kept chunk) |

The output is `header`, then for each chunk, the bytes of its gap (kept: the next bytes of
`kept`; else zero bytes), then the chunk's `4096 c` bytes:

- **kept**: the next `4096 c` bytes of `kept`.
- **rebuilt**: of the next `n` bytes of `data`, `z = zlib(d, p)` of the params; its length
  `1 + len(z)` (4 bytes), the compression type 2, `z`, then the padding.
- **stored**: its length `1 + n` (4 bytes), the compression type 3, the next `n` bytes of
  `data`, then the padding.

The padding is the rest of the chunk's sectors, after its data (kept: the next bytes of `kept`;
else zero bytes). Then the bytes up to `len` after the last chunk (or the header): the next
bytes of `kept` where `tail` is 1, zero bytes where it is 0 (another value is not valid).

## 4. Encoding (encoder)

- A file is taken where its first 8192 bytes are a header of at least one chunk (§1), each of a
  length `L` that its sectors hold (`L + 4 <= 4096 c`) and of a compression type Minecraft
  writes (1 gzip, 2 zlib, 3 none, 4 LZ4, 127 a named one; with bit 7, its data in a file of its
  own, `L` then 1). Nothing else of a region file is magic.
- The parameters: of the first four zlib chunks, the candidates of their zlib headers (the
  window bits of CMF, the levels of FLG's flags, memory levels 8 and 9, the default and filtered
  strategies) tried in turn on all four; the first making all of them again, else the one
  making the most. Minecraft (Java's `Deflater`, zlib 1.2.11 to 1.3.1 alike) writes every chunk
  at level 6, memory level 8: all 1,435,711 chunks of the 18 GB world measured (README).
- A zlib chunk is rebuilt where its stream is `zlib(d, p)` of its data `d` (inflated, all the
  chunks of a file 64 MiB and 64 times the file at most), so that the stream is what the module
  makes; a chunk of type 3 is stored; any other is kept.
- The gaps and paddings are kept where any byte of them is not zero. Minecraft does not clear
  the sectors it frees, so they hold the streams of earlier writes of chunks, whole in free
  sectors and the tails of longer ones in padding; `kept` is of no kind, so that the archive
  finds the whole zlib streams in it.
- The layout is checked to make the file again of the streams as they are before the formula is
  given.

## 5. Validation

A decoder fails where:

- the params are not 4 bytes of the ranges of §2, or `header` is not 8192 bytes;
- the chunks of `header` overlap or end past sector `floor(len / 4096)`;
- a tag's bits 0-1 are 3, a kept chunk's tag has `pad` or `n`, or `tail` is not 0 or 1;
- a chunk's data does not fit its sectors (`n + 5 > 4096 c`, or `len(z) + 5 > 4096 c`);
- the inputs end before the output is made, or any of them is not taken up exactly.
