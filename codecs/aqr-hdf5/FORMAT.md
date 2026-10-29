# aqr-hdf5 format

This document specifies how aqr-hdf5 takes an HDF5 file apart into an aqr-formula entry
formula, and what the formula stores. aqr-hdf5 has no module of its own: the formula is the
file's bytes, with the DEFLATE streams of its deflated chunks given by applications of
aqr-flate's module `flate-set` (aqr-flate's `FORMAT.md`, "Formula module"). Everything here
but §1 is the encoder's choice *(encoder)*; a decoder needs only aqr-formula and `flate-set`.

HDF5 itself is the HDF5 File Format Specification, version 3.0, cited by its sections.

## 1. The formula

Three streams:

- `hdf5/meta`: the bytes of the file outside the rebuilt DEFLATE streams, in order: the
  superblock, object headers, B-trees, heaps, data not deflated, chunks not rebuilt, and of
  each rebuilt chunk its zlib header (2 bytes), Adler-32 (4 bytes) and, where Fletcher-32
  follows deflate, that checksum (4 bytes);
- `hdf5/data` (kind `table`): the data of the rebuilt chunks, inflated, one after another
  in the order of the file;
- `flate/record`: their aqr-flate records, likewise.

The root is the concatenation of slices of `hdf5/meta` and of the rebuilt streams in the order
of the file. The streams are given by `flate-set` as aqr-flate's `formula::streams` groups
them: by profile, one application for each profile (split into its streams, the lengths of
each stream's data and record in a stream of class `flate/index`), or the application alone
where it rebuilds one stream.

## 2. What is read *(encoder)*

An HDF5 file starts with a superblock (§II.A, versions 0 to 3) at offset 0 of the data, or
after a user block of 512, 1024, 2048 or 4096 bytes (a file's user block is not looked for in
other data). Its addresses are relative to the base address (the superblock's offset where the
superblock tells 0); it ends at its end-of-file address, which the library writes with the base
added. Only the sizes of offsets and lengths, the base, the end and the root group's object
header are read from it.

Objects are read from the root group's object header on, each once (by address), at most
2^20 of them, 64 groups deep:

- **Object headers** of version 1 (§IV.A.1.a) and 2 (`OHDR`, §IV.A.1.b), continuation blocks
  (message 0x10; `OCHK` for version 2) followed. Shared messages (flag bit 1) are not read.
- **Groups**: symbol table messages (0x11: the v1 B-tree of type 0 and its symbol nodes,
  `SNOD`), link messages (0x06), and link info messages (0x02) of dense storage, whose fractal
  heap (`FRHP`, §III.G, not filtered) has its direct blocks (`FHDB`, under the root block and
  indirect blocks `FHIB`) read as link messages one after another from the first object to
  the first that does not read as one. (The heap's v2 B-tree index of names is not read: a
  heap written once and never changed holds its objects so.) Only hard links are followed.
- **Datasets**: an object header with a data layout message (0x08) of chunked storage and a
  filter pipeline message (0x0B, versions 1 and 2). The datatype message (0x03) is read for
  the class, size, byte order and sign, which nothing uses yet.
- **Chunk indexes**: layout versions 1 to 3 (v1 B-tree of type 1, `TREE`, its keys of
  dimensionality offsets); versions 4 and 5 (the fields of version 4 for chunked storage) with
  a single chunk index of a filtered chunk, or a fixed array index (`FAHD`/`FADB`, §VII.B) of
  filtered chunks, paged or not. Implicit, extensible array and v2 B-tree indexes are not
  read: their datasets' chunks stay in `hdf5/meta`.

A chunk is taken where it lies within the file; of chunks that overlap (in a corrupt file),
the first by address.

## 3. The chunks rebuilt *(encoder)*

A chunk's filters are its dataset's pipeline but those its filter mask skips (bit `i` set:
filter `i` not applied). A chunk is rebuilt where

- deflate (filter 1) is the last of them, or is followed by Fletcher-32 (filter 3) alone;
- it starts with a zlib header (RFC 1950: CM 8, CINFO at most 7, no preset dictionary, the
  check right) and is a whole zlib stream up to its end (but the 4 bytes of Fletcher-32 after
  deflate): the DEFLATE stream ends 4 bytes before it, and the 4 bytes are the Adler-32 of
  the data;
- where the filters before deflate are shuffle (2) and Fletcher-32 only, the data is the
  dataset's chunk size (the product of the layout's dimensions, the last of them the element's
  bytes) and 4 bytes for each Fletcher-32;
- the data, all chunks so far together, is within the limit (by default 64 MiB and 32 times
  the file; in an archive, what is left of its budget), counting each chunk by its size above,
  or else by its bytes in the file;
- aqr-flate takes it, and its record is shorter than its DEFLATE stream.

aqr-flate's settings of zlib (with an adaptive profile) are found first on the 8 largest
chunks and tried first on the others, as the chunks of a file are deflated alike.
