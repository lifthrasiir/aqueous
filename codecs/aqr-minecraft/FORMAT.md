# The `mca-recons` and `dh-lods` modules

This document specifies the module `mca-recons` of aqr-formula entry formulas. The module makes
a Minecraft region file (Anvil, `.mca`) again bit for bit of its header, the data of its chunks
and a *layout* telling where each chunk is and what of it is kept as it is. A chunk's zlib
stream is made again of its data by zlib's compressor (one `deflate(Z_FINISH)` call), as
aqr-flate's `FORMAT.md` specifies it ("zlib emulation"), of the parameters of the
application's params: the module stores no DEFLATE of its own. A chunk's data may be its NBT
without the block states and light of its sections (its *skeleton*), those coded apart by a
model (§4, §5). Sections marked *(encoder)*
describe the choices this implementation makes when it builds formulas; the module does not
depend on them. §8 specifies `dh-lods`, of Distant Horizons' database.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [The layout](#3-the-layout)
4. [Skeletons and holes](#4-skeletons-and-holes)
5. [The model](#5-the-model)
6. [Encoding](#6-encoding-encoder)
7. [Validation](#7-validation)
8. [The module `dh-lods`](#8-the-module-dh-lods)

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

- **Module**: name `mca-recons`, version 2.
- **Params**: 4 bytes: zlib's level (1 to 9), strategy (0 default, 1 filtered, 2 Huffman only,
  3 RLE, 4 fixed), window bits (9 to 15) and memory level (1 to 9).
- **Resources**: none.
- **Inputs**: exactly five:
  1. `header` (class `mca/header`): the file's first 8192 bytes, as they are;
  2. `layout` (class `mca/layout`): §3;
  3. `data` (class `mca/data`): the data of the chunks made of it (rebuilt and stored, §3), one
     after another;
  4. `kept` (class `mca/kept`): the bytes kept as they are, in order;
  5. `holes` (class `mca/holes`): the holes of the chunks of NBT (§4), each chunk's coded by
     itself (§5), one after another.
- **Output**: the region file of §3. Its length must be the `len` the application declares.

It is a part module (windows of a page for `header` and `layout`, of 16 pages for the others
and the output): it takes every input in order, in pieces, and puts the file in order, a chunk
at a time, holding the header, the chunks' locations, the model (§5) and, for a chunk rebuilt,
its data, its holes coded and its sectors. Its plan follows the largest chunk rebuilt, not the
file.

## 3. The layout

The layout is, for each chunk (§1) of `header` in order, its *tag* (a varint), then one byte,
`tail`. A tag's bits:

| bits | |
|---|---|
| 0-1 | how the chunk is made: 0 *kept*, 1 *rebuilt*, 2 *stored* (3 is not valid) |
| 2 | `gap`: the bytes between the end of the chunk before (or of the header) and this one's first sector are kept, else zero |
| 3 | `pad`: the chunk's padding is kept, else zero (0 for a kept chunk) |
| 4 | `nbt`: the chunk's data is an NBT made of its skeleton and holes (§4), and a varint `h` follows the tag: the bytes of `holes` that its holes take (0 for a kept chunk) |
| 5 on | `n`: the bytes of the chunk's data (0 for a kept chunk) |

A chunk's *data* `d` is the next `n` bytes of `data`; where `nbt` is set, it is the NBT of `n`
bytes made of the next bytes of `data` (its skeleton) and the next `h` bytes of `holes` (§4).
The output is `header`, then for each chunk, the bytes of its gap (kept: the next bytes of
`kept`; else zero bytes), then the chunk's `4096 c` bytes:

- **kept**: the next `4096 c` bytes of `kept`.
- **rebuilt**: of its data `d`, `z = zlib(d, p)` of the params; its length `1 + len(z)` (4
  bytes), the compression type 2, `z`, then the padding.
- **stored**: its length `1 + n` (4 bytes), the compression type 3, its data, then the padding.

The padding is the rest of the chunk's sectors, after its data (kept: the next bytes of `kept`;
else zero bytes). Then the bytes up to `len` after the last chunk (or the header): the next
bytes of `kept` where `tail` is 1, zero bytes where it is 0 (another value is not valid).

## 4. Skeletons and holes

A chunk of NBT (bit `nbt`) is made by parsing it as it is filled: the NBT of `n` bytes, one
named compound (NBT's binary format, big endian), is read tag by tag, every byte of it the next
byte of the skeleton (in `data`) but the payloads of its *holes*, which are left zero and filled
by the model after the parse (§5). The parse ends where the compound does, which must be at `n`
bytes; lists and compounds are nested at most 512 deep.

The parse makes *sections*, at most 1024, numbered in the order they are made. A compound is
given a section (once, the first time it is asked for one) where it holds, directly, a compound
named `block_states`, a byte array named `BlockLight` or `SkyLight`, or a byte named `Y` (its
value the section's `Y`). Of a `block_states` compound, the list named `palette` is the
section's palette (its entries' type and count, and where they are) and the long array named
`data` its *block states*. Nothing in a palette's list, at any depth, counts. A section is
*twice* where any of these is found twice.

Each candidate is told, in order of the parse, whether it is a hole, by a bit of the model's
coder (§5; the flag of its kind): the `data` of a `block_states` compound (kind 0), and a
`BlockLight` (kind 1) or `SkyLight` (kind 2) of 2048 bytes (one of another length is no
candidate). A hole's payload (after its length) is not in the skeleton.

## 5. The model

A chunk's holes, `h` bytes of `holes`, are decoded by the carry-less binary coder of aqr-common
(as aqr-jpeg's FORMAT.md, L6, specifies it: a probability `p1` of 65536 of a 1, the input read
four bytes past its end at most), its bits of a model kept over the file's chunks (made new for
the file). An adaptive bit here starts at 32768 unless told otherwise and moves after each bit
`v` by `p += (65535 - p) >> 4` (a 1) or `p -= p >> 4` (a 0).

The model is the three flags (§4), and tables of adaptive bits indexed by the high bits of a
64-bit hash: `blocks` (2^18 bits, starting at 32768), `literals` (2^16) and `light` (2^20, the
bit at an index a multiple of 16 starting at 32768, the others as usual). `mix(x) = (x xor (x
>> 29)) * 0xbf58476d1ce4e5b9` (64 bits, wrapping); `slot_k(h)` is the high `k` bits of `h`. The
*hash* of a palette's entry is the high 32 bits of the 64-bit FNV-1a of the bytes of its payload.

After the parse, the holes are filled: the block states of each section that has them as a hole,
in order of the sections; then the block light, then the sky light, of each section that has it
as a hole, from the last section to the first.

### Block states

A section's block states are 4096 indices of its palette of `P` entries (2 to 4096; the section
not twice), in `ceil(4096 / floor(64 / b))` longs of `b = max(4, ceil(log2 P))` bits an index,
`floor(64 / b)` of them in each long from its least significant bits, the rest of it zero; an
index `i` is of the block at `x = i mod 16`, `z = floor(i / 16) mod 16`, `y = floor(i / 256)`.

The *layer below* is the top layer (`y` 15) of the section before it that has a palette, where
both have a `Y` and that one's is one less: each of its blocks the index in this palette of the
first entry of the same hash, else none. (Its blocks: of a palette of one entry, that one; else
of its block states, which must be of as many longs as its palette's indices take, an index past
its palette none.) Where there is none, the layer below is of no blocks.

The rows (`z`, `y`: 16 blocks along `x`), in order of `y` then `z`, are decoded:

- Where the block below the row's first is not none: a bit of whether the row is the row below
  (the layer below for `y` 0), at `slot_18(mix(key))` of `blocks`, `key = 2^49 xor last xor
  (copied_z << 1) xor (flat << 2) xor (hash(below's first) << 8)`, `last` whether the row
  before was so, `copied_z` whether the row of this `z` in the layer below was, `flat` whether
  the row below is all of one index. If it is, the row is that row.
- Else each block `x` from 0: of the neighbours at (dx, dz, dy) = (-1,0,0), (0,-1,0), (0,0,-1),
  (-1,-1,0), (1,-1,0), (-1,0,-1), (0,-1,-1), (1,0,-1), (0,1,-1), (-2,0,0), (0,-2,0), (0,0,-2),
  (-1,1,-1), (1,1,-1), (-1,-1,-1), (1,-1,-1), none past the section's sides but the layer below,
  the first not none and not one of those before it in that order is asked, in turn, by a bit
  of whether the block is it (the `k`-th asked, from 1): at `slot_18(mix(((c * 8 + pat) * 16 +
  k) xor (hash(v) << 16) xor (hash(o) << 40)))` of `blocks`, `c` the neighbour's place in the
  list, `v` its index, `o` the west neighbour's (or, for the west one, the one below), `pat` the
  bits of whether west = north, west = below and north = below (bits 0 to 2), the hash of none
  being 0x55555555. If none is it, the index's `ceil(log2 P)` bits from the top, each at
  `slot_16(mix(2^48 xor (hash(west) << 8)) xor mix(node))` of `literals`, `node` 1 then
  `node * 2 + bit`; it must be less than `P`.

The indices are packed back into the longs as above.

### Light

A section's light of a kind is 4096 nibbles (2048 bytes, the block `i`'s in byte `floor(i / 2)`,
low nibble first), the section not twice. The *section above* is the next section that has light
of the kind, where both have a `Y` and that one's is one more; the *layer above* is its bottom
layer where its light is of 2048 bytes (else of none, 16, each), and `state` its nibble where all
are the same, else 16; 17 where there is no section above, or its light is of another length. The
section's *blocks* are its block states' hashes (as for the layer below), where it has them.
`tag = kind << 56`.

A bit of whether all 4096 nibbles are the same, at `slot_20(mix(tag xor 0x100 xor state))` of
`light`; if they are, the nibble by the tree below at `mix(tag xor 0x200 xor state)`. Else the
rows (`y` from 15 down to 0, `z` from 0) are decoded:

- Where there is a row above (in the section, or of the section above): a bit of whether the row is
  the row above, at `slot_20(mix(tag xor 0x300 xor last xor (flat << 1) xor (up << 2) xor (b <<
  24)))`, `last` whether the row before was so, `flat` whether the row above is all one nibble,
  `up` its first nibble, `b` the hash of the row's first block (0 without blocks).
- Else each nibble `x` from 0, of `u` (the one above), `w` (west), `n` (north), 16 for none:
  `key = tag xor (u | w << 5 | n << 10) xor (b << 24)`, `b` the block's hash (0 without
  blocks); `guess` the first of `u`, `w`, `n` not none, else 0; a bit of whether it is the
  guess at the multiple of 16 at or below `slot_20(mix(key))`; else the nibble by the tree at
  `mix(key)`.

The tree at `h`: the nibble's 4 bits from the top, each at `slot_20(h)` rounded down to a
multiple of 16, plus `node` (1, then `node * 2 + bit`), of `light`.

## 6. Encoding (encoder)

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
- A chunk made of its data whose data is an NBT (§4) of at least one hole is given as its
  skeleton and holes: a section's block states are a hole where they are of a palette of 2 to
  4096 entries and pack back as they are (each index in the palette, the bits past them zero);
  light is a hole where its section is not twice. Each chunk's holes are coded by one
  `ArithEncoder` of aqr-common, of the model as it is after the file's chunks before it. The
  holes are `holes`'s, of kind compressed (they hardly compress further); the skeletons are of
  `data`.
- The layout is checked to make the file again of the streams as they are before the formula is
  given.

## 7. Validation

A decoder fails where:

- the params are not 4 bytes of the ranges of §2, or `header` is not 8192 bytes;
- the chunks of `header` overlap or end past sector `floor(len / 4096)`;
- a tag's bits 0-1 are 3, a kept chunk's tag has `pad`, `nbt` or `n`, or `tail` is not 0 or 1;
- a chunk of NBT does not parse (§4) to its `n` bytes, or a hole is not as §5 has it (a palette
  of fewer than 2 or more than 4096 entries, block states of another count of longs, a section
  twice, an index past its palette), or its holes' coder reads past their `h` bytes;
- a chunk's data does not fit its sectors (`n + 5 > 4096 c`, or `len(z) + 5 > 4096 c`);
- the inputs end before the output is made, or any of them is not taken up exactly.

## 8. The module `dh-lods`

`dh-lods` version 1 makes the contents of Distant Horizons' blobs (its database's `FullData`),
one after another, of which aqr-zstd's `zstd-frames` makes their frames: those of the coarser
levels' sections (*parents*) made of the finer ones' (their *children*), the others stored.

### 8.1 Sections

A row of `FullData` is a section of 64 by 64 columns (column `i = x * 64 + z`), its eight blobs,
in this order (*blob* 0 to 7): `Data` (the columns of `x` and `z` from 1 to 62), `NorthAdjData`
(`z` 0), `SouthAdjData` (`z` 63), `EastAdjData` (`x` 63), `WestAdjData` (`x` 0), each of their
columns with `x` outer and `z` inner; `ColumnGenerationStep` and `ColumnWorldCompressionMode`, a
byte for each column (in the order of `i`); `Mapping`, a 4-byte count (big endian) and its
*states*, each a 2-byte length and its bytes. A column is points from the top down, each a
state (an index of the mapping), a height, the Y of its bottom and its light (a byte).

A blob of columns is: a varint count for each column; for each point, a varint `id << 2 | (light
!= 0) << 1 | (e != 0)`; for each point its height (a varint); for each point with `e != 0`, `e`
zigzag (`e = bottom - (b - height)`, `b` the point before's bottom, 0 for the first of the blob);
for each point of light not 0, its light. The sides overlap at the corners: of a column of two
sides, the later (in the order of the blobs) is the section's.

A state is *air* where its bytes end with `_DH-BSW_AIR`; it is *opaque* where it is one of the
module's opaque states (§8.4).

### 8.2 Making a parent

A child's *quadrant* is 32 by 32 columns (`x * 32 + z`): of each, the *merge* of the child's
columns `(2x, 2z)`, `(2x, 2z + 1)`, `(2x + 1, 2z)`, `(2x + 1, 2z + 1)` (in that order), its
generation step the least of theirs and its compression mode the most. The merge of four columns:

- The Ys of all their points' bottoms and tops, in order, each once; between each two of them a
  run, from the lowest. Of each run `[lo, hi)`, `y = lo + floor((hi - lo) / 2)`, and of each of
  the four columns the point there (bottom <= y < top) or none.
- The state of the run: of the four, those not air are counted, each as the first of the four
  (in order) of the same state (none is a state as any other); the first with the most counts.
  Its light: each nibble the average (rounded down) of those of the four of that state, else of
  all four.
- A run of the same state and light as the one below is joined to it (its height added); the
  points are the runs, from the top down (where there are two at least).

The parent of children `(2x + a, 2z + b)` is the quadrants side by side (`a` along x, `b` along
z), then *culled*: of each column `(x, z)` of more than one point, in order of `x` then `z`, not
at a side of the section (`x` or `z` 0 or 63) and of four neighbours `(x +- 1, z)`, `(x, z +- 1)`
of points: each point, from the bottom up but the bottom one and the top one, opaque between
an opaque one above and an opaque one below, whose neighbours' points beside it (overlapping
its Ys, from the bottom up, each neighbour from where the point before stopped) are all opaque
up to one reaching its top, is taken out and joined into the one above (its height added, its
bottom the point's). Columns culled before are the neighbours as they are.

A parent's blobs: its mapping the states of its points in the order of their first use (columns
`i`, each from the top), then its blobs as §8.1 has them; a point of none is corrupt.

A quadrant's bytes: a varint of its mapping's bytes and the mapping (as `Mapping`, of its states
in the order of their first use), its 1024 columns (a blob of columns, §8.1), its generation
steps and compression modes (1024 bytes each).

### 8.3 The module

- **Params**: varints of the rows, the states and their bytes, the most points written of a
  section (of the sides' corners twice), the most points of a column, the most bytes of a
  quadrant, and the bytes of the pool.
- **Inputs**: `steps` (class `dh/steps`), `stored` (class `dh/stored`), `opaque` (class
  `dh/opaque`: the opaque states, as a `Mapping`).
- **Output**: the contents, of the `len` the application declares.

The steps, each a varint `tag` of `n = tag >> 2`, by `tag & 3`:

- 0, a content stored: the next `n` bytes of `stored`.
- 1, a content stored that is blob `k` of child row `r` (varints `r` and `k`): put, and kept (in
  the pool) until the row has all eight; at the eighth, a varint `q`: the child's quadrant is
  made of them (its bytes `q`, else corrupt), kept in a block of the pool of `q` bytes taken
  then, and the eight given back.
- 2, blob `k` of parent row `r` (varints `r` and `k`; `n` 0). At the row's first, four varints
  of its quadrants' sources (0: the next bytes of `stored`, a varint of their count and the
  quadrant's bytes; else the quadrant kept of row `source - 1`, its block given back), and a
  varint 1 where the parent's own quadrant is kept too (else 0): the parent is made (§8.2), its
  eight blobs kept in blocks taken in order, then its quadrant's. Each such step puts its blob
  and gives its block back.

The pool is aqr-zstd's (FORMAT.md, "The frames open"): blocks rounded up to 64 bytes, the first
free span that fits. Once the steps are done, every input must be taken whole, nothing kept, and
the bytes put the output's length; a block past the pool, a state past the params' or a row
given twice is corrupt.

The module lays its memory out as a stack: the readers' pieces, the opaque states, the rows,
the pool, and the buffers of a section, four quadrants, a parent and its blobs, and of a
child's quadrant's making, all of the params (`lods::plan`). Natively, the children's quadrants
are made on worker threads, each of its row's mapping alone, put in their blocks before they
are taken.

### 8.4 Encoding (encoder)

- A row is taken where its eight blobs are frames that zstd-frames makes again. The opaque
  states are those of the points that the parents' columns do not have of their children's
  merged columns (in-sample: they are stored). A parent is made where its blobs are made again
  byte for byte (96% of The End's, 99% of the Nether's).
- A quadrant comes of its child where the child's last blob (or, of a parent made, its first)
  comes before the parent's first; else it is stored. A parent none of whose quadrants come of a
  child is stored.
- What the pool holds at once is bounded (96 MiB, and the pool 160 MiB): where more would be,
  the span (a child's from its first blob until its parent is made, a parent's until its last
  blob) ending last is let go, a child's quadrant then stored, a parent then stored.
