# The `git-pack-recons` and `git-pack-index-recons` modules

This document specifies the module `git-pack-recons` of aqr-formula entry formulas, and
`git-pack-index-recons`, the pack with its index and reverse index (§6). The module
rebuilds a git pack bit for bit from the DEFLATE streams of its entries (which an application
of aqr-flate's module `flate-set` gives it, with the Adler-32 and sizes of their data), and a
*layout* holding the rest of the pack. What follows from the data (the entries' headers, the
zlib checksums, the pack's trailer) costs nothing where it is what git writes, and is kept as
it is where it is not. The module itself runs no DEFLATE code: an implementation of it can be
written from this document alone, and aqr-flate's `FORMAT.md` specifies the application
giving its first input. Sections marked *(encoder)* describe the choices this implementation
makes when it builds formulas; the module does not depend on them.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [The layout](#3-the-layout)
4. [Encoding](#4-encoding-encoder)
5. [Validation](#5-validation)
6. [The pack with its indexes](#6-the-pack-with-its-indexes)

## 1. Conventions

- **varint**: unsigned LEB128, least significant group first.
- A **continued** field is bytes while bit 7 is set, then one more: 1 to 10 bytes.
- The pack is git's (`gitformat-pack`): a 12-byte header (`PACK`, the version and the count of
  entries, each 4 bytes big endian), the entries, and the SHA-1 of all that (20 bytes). An
  entry is a header (a continued field: bits 4-6 of its first byte are the type, its size is
  bits 0-3 of the first byte, then 7 bits of each next byte, least significant first), then for
  an ofs-delta (type 6) the distance to its base (a continued field) and for a ref-delta (type
  7) its base's id (20 bytes), then a zlib stream (RFC 1950): 2 bytes of header, a DEFLATE
  stream, and the Adler-32 of its data, big endian.
- **header(t, n)**: git's header of an entry of type `t` and size `n`: the fewest bytes
  holding `n` as above.
- **sha1(d)**: the SHA-1 of `d` (FIPS 180-4).

## 2. The module

- **Module**: name `git-pack-recons`, version 1.
- **Params**: none (empty).
- **Resources**: none.
- **Inputs**: exactly three:
  1. `deflated`: the DEFLATE streams of the rebuilt entries one after another, then for each
     in order, 20 bytes: the Adler-32 of its data (4 bytes, big endian), the length of its
     data and the length of the stream (8 bytes each, little endian);
  2. `layout` (class `git-pack/layout`): §3;
  3. `kept` (class `git-pack/kept`): the bytes kept as they are, in order.
- **Output**: the pack of §3. Its length must be the `len` the application declares.

With `n` rebuilt entries in the layout (§3), the last `20n` bytes of `deflated` are the table,
and the bytes before it are cut into the streams by the lengths it gives, which must use them
up exactly.

The encoder gives `deflated` as an application of aqr-flate's module `flate-set` with params
`[3]` (aqr-flate's `FORMAT.md`, "Formula module"), which outputs exactly that. Its data is the
data of the rebuilt entries one after another, a content of the application: slices of
streams of class `raw` (blobs and deltas), `git-pack/text` (commits and tags) and
`git-pack/table` (trees), which may be recompressed in turn; its records a stream of class
`flate/record`, and its index one of class `flate/index`.

## 3. The layout

The layout is:

| field | |
|---|---|
| `n` | varint: the count of entries |
| `tags` | `n` bytes, one per entry |
| per entry, in order | what its tag tells (below) |
| `trailer` | 1 byte: 1 where the SHA-1 is derived, else 0 |

The output is the first 12 bytes of `kept` (the pack header), then each entry, then, if
`trailer` is 1, `sha1` of the output so far, then all of `kept` that is left. The layout must
be used up exactly.

A tag of 0 is an entry **kept**: its layout field is a varint `L`, and the entry is the next
`L` bytes of `kept`.

Otherwise bit 3 (`8`) must be set, the entry is **rebuilt**, and only bits 0-5 may be set: bits
0-2 are its type `t`, bit 4 (`16`) is *header kept* and bit 5 (`32`) *zlib header as before*.
Its stream, data length `size` and Adler-32 are the next of `deflated`'s (§2). The entry is:

1. its header: with *header kept*, a continued field of `kept`; else `header(t, size)`;
2. if `t` is 6, a continued field of the layout; if `t` is 7, the next 20 bytes of `kept`;
3. its zlib header: with *zlib header as before*, that of the previous rebuilt entry (`78 9C`
   before the first); else the next 2 bytes of the layout;
4. its stream, then its Adler-32.

The type of a rebuilt entry is not checked, nor its header against its data: what is kept is
taken as it is.

## 4. Encoding *(encoder)*

- A pack is taken where it starts with `PACK` and version 2 or 3, and each of the count of
  entries it declares is of a type of git's (1-4, 6, 7) and has a zlib header followed by a
  DEFLATE stream that ends within the data, then 4 bytes. Ids are taken as 20 bytes (SHA-1
  repositories). Otherwise it is not a pack, and the encoder makes no formula.
- An entry is rebuilt where the sizes its header declares, of it and of the entries before it,
  fit the encoder's budget of inflated bytes; its data is as long as its header says; its
  Adler-32 is right; aqr-flate takes the stream; and its record is shorter than the stream and
  16 bytes. Others are kept. Streams of 256 KiB or more are rebuilt only where a probe tells
  that it may pay, the data held by other files archived along with it counted as nothing.
- The records are of the profile the encoder is given (aqr-compress: aqr-flate's default
  below level 7, its adaptive profile with zlib emulation from 7 on). git writes every object
  with the same zlib settings: with zlib emulation, those of the 8 largest streams are found by
  aqr-flate on its own, and the most frequent of them given as a hint for the others, which are
  emulated exactly where they predict every token (a record of a few bytes).
- The data of an entry of 256 KiB or more is a slice of its own (recompressed as a file, and
  found again where the same file is archived along with the pack); the rest go together by
  kind.
- The trailer is derived where it is the SHA-1 of the pack; anything after it is kept.

## 5. Validation

The module fails, which the specification ("Module execution") makes an invalid archive
failure, on params, resources or inputs other than §2's; a layout or `kept` that runs out, or
a layout not used up; a tag of other bits than §3's; a `deflated` shorter than its table or not
cut exactly by it; a stream length that does not fit; a `trailer` other than 0 and 1; or an
output whose length is not `len`.

## 6. The pack with its indexes

A pack's index (`.idx`, version 2) and reverse index (`.rev`, version 1) (`gitformat-pack`)
follow from the pack and the ids of its entries: only the ids are stored, in the order of the
entries in the pack.

- **Module**: name `git-pack-index-recons`, version 1.
- **Params**: a byte of flags, of which only bit 0 (*with reverse index*) may be set, then a
  varint `P`, using up the params.
- **Resources**: none.
- **Inputs**: exactly four: `deflated`, `layout` and `kept` as §2's, then `ids` (class
  `git-pack/ids`): `20n` bytes for some `n`, the ids of the entries in their order.
- **Output**: the pack of §3 of `P` bytes (`len` §3's) with the offsets `s_0, ..., s_(m-1)` of
  its entries in order and the offset `s_m` after the last (where the trailer starts, derived
  or not); then the index; then, *with reverse index*, the reverse index. `m` must be `n`, and
  `s_m + 20` at most `P`. Its length must be the `len` the application declares.

Let `id_j` be the `j`-th 20 bytes of `ids`, `t` the 20 bytes of the pack from `s_m` (its
trailer), and `π` the entries `0, ..., n-1` sorted by `id_j` in byte order, of the same id in
their order (a stable sort). All numbers are big endian. The index is:

1. `FF 74 4F 63 00 00 00 02`;
2. for each byte value `b` from 0 to 255, the count of entries whose id starts with a byte of at
   most `b` (4 bytes);
3. `id_j` for each `j` in the order `π`;
4. for each `j` in the order `π`, the CRC-32 (ISO 3309, as zlib's) of the pack's bytes from
   `s_j` to `s_(j+1)` (4 bytes);
5. for each `j` in the order `π`, `s_j` (4 bytes) if it is at most `7FFFFFFF`; else
   `80000000` plus the count of entries before `j` in the order `π` at an offset past
   `7FFFFFFF`;
6. `s_j` (8 bytes) of each entry at an offset past `7FFFFFFF`, in the order `π`;
7. `t`, then `sha1` of everything before it in the index.

The reverse index is `52 49 44 58 00 00 00 01 00 00 00 01` (`RIDX`, version 1, SHA-1), then for
each entry `j` in the pack's order its place in `π` (4 bytes, from 0), then `t`, then `sha1` of
everything before it in the reverse index.

The module fails, besides on what §5 lists for the pack, on params other than the above, an
`ids` whose length is not `20m`, or an output whose length is not `len`.

*(encoder)* The index is taken where it is of version 2 and its offsets, sorted, are where
entries start: the first at 12, and each entry, parsed as §4 does, ending where the next one
starts (each on its own, in parallel). The ids are read off it by those offsets; it is taken
only where it is the index above of them (and the reverse index, where there is one, the
reverse index above), else the files are archived as they are. aqr-compress finds a pack's
indexes among the files archived along with it by the pack's trailer, which theirs repeat.
