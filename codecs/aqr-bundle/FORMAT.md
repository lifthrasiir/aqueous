# The `bundle-recons` module

This document specifies the module `bundle-recons` of aqr-formula entry formulas. The module
rebuilds an archive that only bundles files, an ar, tar or cpio archive, bit for bit from the
contents of its members and a *layout* holding the rest of the archive. The layout keeps every
byte of the headers, but the fields that follow from the contents (sizes, name sizes, checksums)
are stored as their XOR with that prediction, and padding that is zeros (or ar's `\n`) is not
stored, so that whatever a writer put there is kept exactly and what follows from the contents
costs zero bytes. The document is complete: an implementation of the module can be written from
it alone. Sections marked *(encoder)* describe the choices this implementation makes when it
builds formulas; the module does not depend on them.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [The layout](#3-the-layout)
4. [Predictions](#4-predictions)
5. [Encoding](#5-encoding-encoder) *(encoder)*
6. [Validation](#6-validation)

## 1. Conventions

- **varint**: unsigned LEB128, least significant group first, of at most 64 bits.
- **XOR coding**: a field stored as `x` against a prediction `p` is `actual = x XOR p`, byte by
  byte over its whole width.
- `pad(q, a)`: the bytes from `q` to the next multiple of `a`, `(a − q mod a) mod a`.
- Positions are byte offsets in the output, from 0.
- `dec(v, w)`: `v` in decimal, without leading zeros (its last `w` digits where it has more),
  left aligned in `w` bytes and
  padded with spaces. `oct(v, w)`, `hex(v, w)`, `HEX(v, w)`: `v mod 8^w` (or `16^w`) in `w`
  digits, padded with zeros on the left; `hex` of the digits `0-9a-f`, `HEX` of `0-9A-F`.

## 2. The module

- **Module**: name `bundle-recons`, version 1.
- **Params**: none (empty).
- **Resources**: none.
- **Inputs**: exactly two:
  1. `data` (class `raw`): the bodies of the members that are not in the layout, in order;
  2. `layout` (class `bundle/layout`): §3.
- **Output**: the archive of §3. Its length must be the `len` the application declares.

`data` is what the members hold, as it is; it is a content of the application and may be
recompressed in turn.

## 3. The layout

The layout is records, each a tag byte and its fields, until the layout ends. The archive is
the output of the records in order. A *body* of `n` bytes is the next `n` bytes of `data`, or,
where the record's flags have `INLINE` (1), the next `n` bytes of the layout at the place given.

| tag | record | fields in the layout |
|---|---|---|
| 0 | gap | `varint n`, then `n` bytes: output as they are |
| 1 | zeros | `varint n`: `n` zero bytes are output |
| 2 | ar member | §3.1 |
| 3 | tar member | §3.2 |
| 4 | cpio member | §3.3 |

### 3.1 ar members

```text
u8      flags         INLINE (1); NOPAD (2)
varint  p             bytes of name after the header (BSD `#1/p`)
varint  n             bytes of the body
60 bytes              the header, its size field XOR coded (§4)
p bytes               the name
[n bytes]             the body, if INLINE
[1 byte]              where p + n is odd and NOPAD is clear: the pad byte XOR 0x0A
```

The output is the header, the name, the body, then, where `p + n` is odd and NOPAD clear, the
pad byte.

### 3.2 tar members

```text
u8      flags         INLINE (1); PAD (2)
varint  n             bytes of the body
512 bytes             the header, its size and checksum fields XOR coded (§4)
[n bytes]             the body, if INLINE
[pad(n, 512) bytes]   the padding, if PAD
```

The output is the header, the body, and `pad(n, 512)` bytes of padding: those given where PAD is
set, else zeros. The size field is decoded first, then the checksum field (its prediction is of
the rest of the header).

### 3.3 cpio members

```text
u8      form          0: newc, 1: crc, 2: odc, 3: binary little endian, 4: binary big endian
u8      flags         INLINE (1); PAD (2); UPPER (4)
varint  m             bytes of the name, its NUL included
varint  n             bytes of the body
h bytes               the header, of h bytes by the form, its fields XOR coded (§4)
m bytes               the name
[a bytes]             the name's padding, if PAD
[n bytes]             the body, if INLINE
[b bytes]             the body's padding, if PAD
```

| form | magic | h | alignment |
|---|---|---|---|
| newc | `070701` | 110 | 4 |
| crc | `070702` | 110 | 4 |
| odc | `070707` | 76 | 1 |
| binary | `C7 71` (little endian) or `71 C7` (big endian) | 26 | 2 |

The output is the header, the name, its padding (to the alignment, by position: `a = pad(q, A)`
where `q` is the position after the name), the body, and its padding (`b = pad(q', A)` after the
body): those given where PAD is set, else zeros. The header's checksum (crc) is predicted from the
body, so it is decoded once the body is read.

## 4. Predictions

| record | field (offset, width) | prediction |
|---|---|---|
| ar | size (48, 10) | `dec(p + n, 10)` |
| tar | size (124, 12) | where `n < 2^33`: `oct(n, 11)` then a NUL; else `0x80`, three zero bytes, then `n` as 8 bytes big endian |
| tar | checksum (148, 8) | `oct(s, 6)`, a NUL and a space, where `s` is the sum of the header's bytes as unsigned, those of the field taken as 8 spaces |
| cpio newc, crc | filesize (54, 8) | `hex(n, 8)` |
| cpio newc, crc | namesize (94, 8) | `hex(m, 8)` |
| cpio newc | check (102, 8) | `hex(0, 8)` |
| cpio crc | check (102, 8) | `hex(c, 8)`, where `c` is the sum of the body's bytes, modulo `2^32` |
| cpio odc | namesize (59, 6) | `oct(m, 6)` |
| cpio odc | filesize (65, 11) | `oct(n, 11)` |
| cpio binary | namesize (20, 2) | `m mod 2^16`, as a word |
| cpio binary | filesize (22, 4) | `(n >> 16) mod 2^16`, then `n mod 2^16`, as words |

Words are 16 bits in the form's byte order. Where UPPER is set, `HEX` takes the place of `hex`.
Other fields are stored as they are.

## 5. Encoding *(encoder)*

`aqr_bundle::formula::encode_with` takes an archive by its start: `!<arch>\n` (ar, thin archives
not taken), a cpio header (a magic, then fields of the form's digits, then a name ending with a
NUL that fits), or a tar header (its checksum, as the unsigned sum or the signed sum of some old
writers; with ustar or GNU magic, or a name and an octal mode of v7). It reads members from the
start while their headers parse and their bodies and padding fit, and keeps the rest as a gap:

- **ar**: the global header is a gap. A body of odd size has its pad byte unless the next header
  is right after it (or nothing follows). The member `//` (GNU's long names) is INLINE; symbol
  tables (`/`, `/SYM64/`, `__.SYMDEF…`) are data of kind table; BSD names (`#1/p`) are taken
  apart from the body.
- **tar**: a member's body is its size field's, else the `size` of a pax header (`x`) before it.
  Links, devices, directories and FIFOs (types `1` to `6`) are given no body where the next
  block is a header, zeros or the end, else the size. Bodies of pax headers (`x`, `g`) and GNU
  long names (`L`, `K`) are INLINE. Runs of zero blocks are zeros records, and so is the rest of
  the archive where it is all zeros.
- **cpio**: runs of zero bytes between members and archives are zeros records, and archives one
  after another are read on.

A member's name (from the long names of ar, a pax `path` or a GNU long name, ustar's prefix
joined) gives the kind of its contents (`Options::kind`); the contents of each kind are a stream
of their own, slices of it in order, those of at least 1 KiB slices of their own. The layout
is checked by building the archive from it, which must give the input; else there is no
formula. An archive without a member has none either.

aqr-compress takes an archive at the start of data, and ar and cpio archives (of ASCII headers)
anywhere in it, and cuts it after its last member and the zeros after it.

## 6. Validation

The module fails if:

- the params are not empty, or the inputs are not two;
- the layout is malformed: a truncated varint or field, an unknown tag, form or flag, or a range
  past the end of `data` or of the layout;
- `data` is not used up exactly;
- the output would be longer than `len`, or is not `len` bytes long.
