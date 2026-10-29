# aqr-hdf5 format

This document specifies the module `hdf5-recons-float` of aqr-formula entry formulas, and how
aqr-hdf5 takes an HDF5 file apart around it. The formula is the file's bytes, with the DEFLATE
streams of its deflated chunks given by applications of aqr-flate's module `flate-set`
(aqr-flate's `FORMAT.md`, "Formula module"), and the data of chunks of floats by
`hdf5-recons-float` (§4, §5). It is meant to be complete: an implementation of the module can
be written from it alone. Sections marked *(encoder)* describe the choices this
implementation makes when it builds formulas; the module does not depend on them.

HDF5 itself is the HDF5 File Format Specification, version 3.0, cited by its sections.

## 1. The formula *(encoder)*

The streams:

- `hdf5/meta`: the Fletcher-32 checksums of chunks of floats where it is before deflate (4
  bytes each, §4), then the bytes of the file outside the rebuilt DEFLATE streams, in order:
  the superblock, object headers, B-trees, heaps, data not deflated, chunks not rebuilt, and of
  each rebuilt chunk its zlib header (2 bytes), Adler-32 (4 bytes) and, where Fletcher-32
  follows deflate, that checksum (4 bytes);
- `hdf5/data` (kind `table`): the data of the other rebuilt chunks, inflated, one after another
  in the order of the file;
- `flate/record`: the aqr-flate records of the rebuilt chunks, likewise;
- `hdf5/float-coefs` (kind `table`), `hdf5/float-model` and `hdf5/float-raw` (kind
  `compressed`): the inputs of the applications of `hdf5-recons-float`, one after another.

The root is the concatenation of slices of `hdf5/meta` and of the rebuilt streams in the order
of the file. The streams are given by `flate-set` as aqr-flate's `formula::streams` groups
them: by profile, one application for each profile (split into its streams, the lengths of
each stream's data and record in a stream of class `flate/index`), or the application alone
where it rebuilds one stream. A stream's data is a slice of `hdf5/data`, or for a chunk of
floats an output of an application of `hdf5-recons-float` (split into the dataset's chunks),
followed by its Fletcher-32 where that is before deflate.

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

## 4. Chunks of floats *(encoder)*

A dataset's rebuilt chunks are coded by the float model (§5) together, in the order of its
index (that of their offsets in the dataset), where

- its datatype is floating-point of 4 or 8 bytes (§IV.A.2.d, class 1), not shared;
- every one of them went through the same filters before deflate, which are none, shuffle,
  Fletcher-32, or shuffle then Fletcher-32, the shuffle of the datatype's bytes (its client
  data); its data is the dataset's chunk size (and 4 bytes of Fletcher-32);
- they hold 1024 samples at least, and the model codes them in fewer bytes than zstd at
  level 3 does the samples (little endian, one after another), and within what the module
  takes (§5.3).

The shift is the number of low bits that are zero in every sample, at most the sample's bits
less 8; the order is 32.

## 5. `hdf5-recons-float`

- **Module**: name `hdf5-recons-float`, version 1.
- **Params**: varints of `chunks` and `elements` (at least 1), then a byte of flags (1: the
  samples are binary64, else binary32; 2: big endian; 4: shuffled; the other bits zero), a
  byte `shift` (less than the sample's bits) and a byte `order` (at most 32).
- **Resources**: none.
- **Inputs**: exactly three: `coefs`, `model` and `raw`.
- **Output**: `chunks` chunks of `elements` samples each, `n = chunks × elements` samples in
  all, of `s` bytes (8 or 4) each; its length must be `n × s`, which the application declares.

### 5.1 Samples

A sample is `s × 8` bits, `B = s × 8 − shift` of them coded: its bits shifted right by `shift`
(the bits shifted out are zero). Its *value* is the IEEE 754 binary64 or binary32 number of
its bits, as binary64. Its *ordinal* `o` is of its `B` bits `b`: where bit `B − 1` is set,
`~b` in `B` bits, else `b` with bit `B − 1` set (the order of the values, a bijection on `B`
bits).

The samples are in blocks of 65536 (the last perhaps shorter). `coefs` holds, for each block
in order, `order` coefficients `w[0..order]` as binary64 little endian: its length must be
`ceil(n / 65536) × order × 8`.

The prediction of sample `i` of block `w`: `p = 0`, then for `k` from 0 to `order − 1`, where
`i ≥ k + 1`, `p = p + w[k] × x[i − k − 1]` (binary64 arithmetic, round to nearest, each
product rounded, then the sum; `x` the values of the samples before). For binary32 samples
`p` is then rounded to binary32. Where `p` is finite, its ordinal (of its bits shifted right
by `shift`) is the prediction `q`; else `q` is the ordinal of sample `i − 1`, or for sample 0
that of 0.

A sample's ordinal is `o = q + d` (wrapping, in `B` bits), `d` given by `z`, the zigzag of `d`
in `B` bits (`d = (z >> 1) ^ −(z & 1)`).

### 5.2 Coding

`model` is a binary arithmetic code (the 32-bit coder of aqr-fits' `FORMAT.md`, §3) of
adaptive bits: a probability `p1` of 16 bits, at first 32768, updated after each bit by
`p1 += ((65535 or 0) − p1) × r >> 16` (arithmetic shift), `r = ⌊131072 / (2u + 3)⌋` for its
`u`-th use (from 0, `u` at most 30), and clamped to 32..65504.

For each sample in order, its `z` is read:

1. Its length `L = bitlen(z)`: 7 bits, most significant first, of a binary tree (node 1, then
   `2 × node + bit`) of adaptive bits by context `c = ⌊(2 L1 + L2 + L3 + 2) / 4⌋`, `L1`, `L2`,
   `L3` the lengths of the three samples before (0 before the first). `L` must be at most `B`.
2. `L = 0`: `z = 0`; `L = 1`: `z = 1`. Else `z` has its bit `L − 1` set, and below it,
   `t = min(2, L − 1)` bits from the highest, of a binary tree of adaptive bits by `L`, then
   the other `L − 1 − t` bits from `raw` (MSB first, zeros past its end), highest first.

### 5.3 Validation

The params must be as above and the output's length `n × s`, at most 64 × (the bytes of
`model` and `raw` + 64); `coefs` must be as long as §5.1 tells; a length `L` over `B` is an
error, and so are reading more than 4 bytes past the end of `model` and past the end of
`raw`.

### 5.4 The chunks

Each chunk's samples (`elements` of them, in order) are written with their bytes in the byte
order of the flags; where shuffled, as HDF5's shuffle filter lays them out: the first byte of
every sample, then the second of every sample, and so on.
