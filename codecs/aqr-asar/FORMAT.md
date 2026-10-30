# The `asar-recons` module

This document specifies the module `asar-recons` of aqr-formula entry formulas. The module
rebuilds an asar archive (Electron's) bit for bit from its *data*, the bytes after its header,
and two inputs holding the header: the header's JSON with *holes* where a value follows from the
data (a file's size and offset, the SHA-256 of its contents and of their blocks), and an *index*
of the files the holes tell of. Whatever does not follow from the data is kept as it is. The
document is complete: an implementation of the module can be written from it alone. Sections
marked *(encoder)* describe the choices this implementation makes when it builds formulas; the
module does not depend on them.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [The index](#3-the-index)
4. [The header](#4-the-header)
5. [The archive](#5-the-archive)
6. [Encoding](#6-encoding-encoder) *(encoder)*
7. [Validation](#7-validation)

## 1. Conventions

- **varint**: unsigned LEB128, least significant group first, of at most 64 bits. **svarint**: a
  varint of the zigzag code of a signed 64-bit value (`(v << 1) XOR (v >> 63)`).
- `u32le`: 4 bytes, little endian.
- `pad(q, a)`: the bytes from `q` to the next multiple of `a`, `(a − q mod a) mod a`.
- `dec(v)`: `v` in decimal, ASCII, without leading zeros (`0` for zero).
- `hex(h)`: bytes `h` as two lower case hexadecimal digits each (`0-9a-f`), high nibble first.
- `sha256(x)`: the SHA-256 digest of `x` (FIPS 180-4), 32 bytes.

## 2. The module

- **Module**: name `asar-recons`, version 1.
- **Params**: none (empty).
- **Resources**: none.
- **Inputs**: exactly three:
  1. `data` (class `raw`): the bytes of the archive after its header, as they are;
  2. `header` (class `asar/header`): §4;
  3. `index` (class `asar/index`): §3.
- **Output**: the archive of §5. Its length must be the `len` the application declares.

`data` is what the archive holds, as it is; it is a content of the application and may be
recompressed in turn.

## 3. The index

```text
varint   n                     files
n × {
  svarint  delta               the file's offset minus the end of the one before (0 for the first)
  varint   size                its size
}
16 bytes                       the archive's first 16 bytes, XOR coded (§5)
rest                           the bytes between the JSON and the data, as they are
```

File `i` has `offset_i = end_{i−1} + delta_i` (`end_{−1} = 0`), computed modulo `2^64`, and
`end_i = offset_i + size_i`. Its *contents* are bytes `offset_i` to `end_i` of `data`. Files may
be in any order, overlap, and leave bytes of `data` between them.

## 4. The header

`header` is the JSON text, but where a NUL byte (0x00) is found: a NUL and the byte after it are a
hole, of what that byte tells, and output in their place:

| byte | hole | output |
|---|---|---|
| 1 | size | the next file of the index becomes the *current* file; `dec(size)` |
| 2 | offset | `dec(offset)` of the current file |
| 3 | hash | `hex(sha256(c))`, where `c` is the current file's contents |
| 4 | blocks | after the byte, a varint `b` (not 0) in `header`; for each block of `c` of `b` bytes (the last one shorter where `b` does not divide its size), in order, `"` `hex(sha256(block))` `"`, separated by commas (`,`); nothing where `c` is empty |

Every other byte is output as it is. The holes take the files of the index in order, each once;
the output is the *JSON*, of `e` bytes.

## 5. The archive

The archive is:

1. 16 bytes: those of the index XOR the prediction `u32le(4)`, `u32le(c + 4)`, `u32le(c)`,
   `u32le(e)`, where `c = 4 + e + pad(e, 4)` (the pickles of Chromium's `Pickle`, as asar
   writes them);
2. the JSON;
3. `rest`, of the index;
4. `data`.

Let `s` be the second `u32le` of the output (after the XOR). The data must start at `8 + s`: that
is, `8 + s = 16 + e + |rest|`.

## 6. Encoding *(encoder)*

`aqr_asar::formula::encode_with` takes an archive that starts `u32le(4)`, `u32le(s)`, two more
`u32le`, the last `e`, and the JSON, of `e` bytes from byte 16, starting `{"files":`, with
`16 + e ≤ 8 + s ≤` the archive's length; the JSON must parse (whitespace allowed) and have no
NUL. Directories are objects with `files`; every other member of a `files` object is a file,
taken where it has `size`, a number of digits only, and `offset`, a string of digits only,
whose contents fit `data`, and is not `"unpacked":true`. Its values become holes where they are
written as the module writes them:

- `size` where it is written `dec(size)`; the other holes only where it is, and only for values
  after it in the text;
- `offset` (between its quotes) where it is written `dec(offset)`;
- in `integrity`, the `hash` (between its quotes) where it is `hex(sha256(c))`, and the
  `blocks` (between their brackets) where they are as the module writes them for `blockSize`
  (`@electron/asar` writes blocks of 4 MiB, none for an empty file).

Files are in the index in the order of the text. The archive is rebuilt from the formula's
inputs, which must give it back; else there is no formula. An archive without a file taken has
none either.

The files' contents, sorted by offset, give the kinds of the slices of `data` by their paths
(`Options::kind`, `/` joining the names from the root as the JSON writes them, escapes kept); the
bytes no file tells of (left by tools that take files out of the header) are of no kind, and of
files that overlap only the first is taken. The contents of each kind are a stream of their own,
slices of it in order, those of at least 1 KiB slices of their own.

aqr-compress takes an archive at the start of data (a file, or a file's contents in another
archive), to the end of the data.

## 7. Validation

The module fails if:

- the params are not empty, or the inputs are not three;
- the index is malformed: a truncated varint or field, more files than half its bytes, or a
  file's contents past the end of `data`;
- the header is malformed: a NUL at its end, a hole of an unknown byte, a hole other than size
  without a current file, a size hole past the files of the index, a truncated varint or a block
  size of 0 in a blocks hole;
- a file of the index is left over;
- the data does not start at `8 + s` (§5);
- the output would be longer than `len`, or is not `len` bytes long.
