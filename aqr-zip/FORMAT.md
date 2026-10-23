# The `zip-recons` module

This document specifies the module `zip-recons` of aqr-formula entry formulas. The module
rebuilds a ZIP archive bit for bit from the contents of its entries, its DEFLATE bodies by
aqr-flate, and a *layout* holding the rest of the archive. The layout keeps every byte of the
headers, but each fixed field is stored as its XOR with a prediction from the contents and
from the records before it, so that whatever a writer put there is kept exactly and what
follows from the rest costs zero bytes. The document is meant to be complete together with
aqr-flate's `FORMAT.md` (the records): an implementation of the module can be written from
them alone. Sections marked *(encoder)* describe the choices this implementation makes when it
builds formulas; the module does not depend on them.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [The layout](#3-the-layout)
4. [Predictions](#4-predictions)
5. [Encoding](#5-encoding-encoder) *(encoder)*
6. [Validation](#6-validation)

## 1. Conventions

- Integers are unsigned and little endian. `le16`, `le32` and `le64` are integers of 2, 4 and
  8 bytes. `clip32(v)` is `min(v, 0xFFFFFFFF)`, and `low32(v)` is `v mod 2^32`.
- **varint**: unsigned LEB128, least significant group first. **svarint**: a varint of the
  zigzag of a signed 64-bit value (0, −1, 1, −2, … as 0, 1, 2, 3, …).
- **crc32(d)**: the CRC-32 of ZIP (reflected polynomial `0xEDB88320`, register set to
  `0xFFFFFFFF` and inverted at the end).
- The ZIP records are those of PKWARE's APPNOTE.TXT 6.3.10. Each starts with a 4-byte
  signature, then a *fixed part*, then its variable fields:

  | record | signature | fixed part | then |
  |---|---|---|---|
  | local file header | `PK 03 04` | 26 bytes | name, extra field |
  | data descriptor | `PK 07 08`, or none | 12 or 20 bytes | |
  | central directory header | `PK 01 02` | 42 bytes | name, extra field, comment |
  | zip64 end of central directory record | `PK 06 06` | 52 bytes | extensible data |
  | zip64 end of central directory locator | `PK 06 07` | 16 bytes | |
  | end of central directory record | `PK 05 06` | 18 bytes | comment |

  Offsets of fields below are within the fixed part, after the signature. A data descriptor
  is `le32` CRC-32, then the compressed and uncompressed sizes, both `le32` (*short*) or both
  `le64` (*long*).
- **XOR coding**: a field stored as `x` against a prediction `p` is `actual = x XOR p`, byte by
  byte over its whole width.
- The **zip64 data** of an extra field: the extra field is read as blocks (`le16` tag, `le16`
  size, that many bytes) from its start, while a whole block header fits. The zip64 data is the
  data of the first block of tag `0x0001`, unless a block runs past the end before it, in which
  case there is none. XOR coding of zip64 data with a list of values (each 8 or 4 bytes) XORs
  the values, in order, into the data from its start, as long as the next value fits entirely.
  The block headers are never coded, so the zip64 data is found the same way before and after.

## 2. The module

- **Module**: name `zip-recons`, version 1.
- **Params**: none (empty).
- **Resources**: none, or the two resources of aqr-flate's module `flate-recons` for the
  profile of the records: the catalog's rules, then the default models or the priors
  (aqr-flate's `FORMAT.md`, "Resources"). They are required when the layout has a deflated
  body (§3.2), and then all records must be of that one profile.
- **Inputs**: exactly four:
  1. `data` (class `raw`): the contents of the stored and deflated bodies, in order;
  2. `records` (class `flate/record`): the aqr-flate records of the deflated bodies, in order;
  3. `layout` (class `zip/layout`): §3;
  4. `kept` (class `zip/kept`): the gaps and the bodies kept as they are, in order.
- **Output**: the archive of §3. Its length must be the `len` the application declares.

`data` is what the entries hold, as it is; it is a content of the application and may be
recompressed in turn.

## 3. The layout

### 3.1 Records

The layout is `svarint base` and then records, each a tag byte and its fields, until the
layout ends. The archive is the output of the records in order. Positions are byte offsets in
the archive; the *offset* of position `q` is `q − base`, taken modulo `2^64`.

| tag | record | fields in the layout |
|---|---|---|
| 0 | gap | `varint n` |
| 1 | local entry | §3.2 |
| 2 | central directory header | §3.3 |
| 3 | zip64 end of central directory record | `varint e`, `e` bytes of extensible data, 52 bytes of fixed part |
| 4 | zip64 end of central directory locator | 16 bytes of fixed part |
| 5 | end of central directory record | `varint c` (at most 65535), `c` bytes of comment, 18 bytes of fixed part |

A gap is the next `n` bytes of `kept`, as they are. The other records are output as their
signature, their fixed part decoded against its prediction (§4), and their variable fields.

### 3.2 Local entries

A local entry is a local file header, a body, and a data descriptor if there is one. Its
fields in the layout:

```text
u8      body          0: stored, 1: deflated, 2: kept, 3: same
body 0: varint n      the body is the next n bytes of data
body 1: varint n      the next n bytes of data, D
        varint r      the next r bytes of records, R: the body is aqr-flate's stream
                      rebuilt from D and R
body 2: varint n      the body is the next n bytes of kept
        le32 crc      the entry's CRC-32 and uncompressed size (§3.4)
        varint size
body 3: varint k      the body is that of the k-th local entry before this one (k ≥ 1)
u8      desc          0: none; 1: short; 2: short, with signature; 3: long; 4: long, with signature
varint  nlen          at most 65535, then nlen bytes: the name
varint  xlen          at most 65535, then xlen bytes: the extra field, its zip64 data coded
26 bytes              the fixed part, XOR coded
12 or 20 bytes        the data descriptor's fields after its signature, XOR coded (none if desc is 0)
```

The output is `PK 03 04`, the fixed part, the name, the extra field, the body, and the data
descriptor (`PK 07 08` first where desc is 2 or 4).

The fixed part is decoded in two steps, since the predictions of the CRC-32 and sizes depend on
the flags: the flags (offset 2) are decoded first against the prediction's flags, which do not
depend on them, then the whole fixed part against the prediction made with those flags. The
zip64 data of the extra field is then decoded with the values of §4.1.

### 3.3 Central directory headers

```text
varint  ref           0: no local entry; else the entry next + unzigzag(ref − 1)
u8      same          bit 0: the name is the local entry's; bit 1: the extra field is
if bit 0 clear: varint nlen (≤ 65535), nlen bytes of name
if bit 1 clear: varint xlen (≤ 65535), xlen bytes of extra field, its zip64 data coded
varint  clen          at most 65535, then clen bytes: the comment
42 bytes              the fixed part, XOR coded
```

Local entries are numbered from 0 in the order of the layout; `next` starts at 0 and is set to
`i + 1` after each header that refers to local entry `i`. `ref` must name an entry before this
header, and `same` must be 0 where `ref` is 0; other bits of `same` are invalid. A name or extra
field that is the local entry's is the bytes of that entry in the output, and its zip64 data is
not coded. Otherwise the zip64 data is decoded, after the fixed part, with the values of §4.2.

### 3.4 The entry

Each local entry has an *entry*: its CRC-32, compressed size and uncompressed size, from which
its fields are predicted.

| body | CRC-32 | compressed size | uncompressed size |
|---|---|---|---|
| stored | crc32 of its data | `n` | `n` |
| deflated | crc32 of `D` | length of the body | `n` |
| kept | as given | `n` | as given |
| same | that of the entry whose body it takes | | |

## 4. Predictions

The predictions come from the records before, through this state, kept as the layout is read:

- `L`: the fixed part of the last local entry, at first 26 zero bytes;
- `C`: the fixed part of the last central directory header, at first 42 zero bytes;
- the local entries so far: the position of each, its fixed part, entry, name, extra field and
  body;
- since the last end of central directory record (at first, since the start): `count`, the
  central directory headers, `bytes`, their bytes in all (signature and variable fields
  included), `first`, the position of the first; and `e64`, the position of the last zip64 end
  of central directory record.

`start(q)`, where the central directory of an end record at position `q` starts, is `first`,
else `e64`, else `q`.

### 4.1 Local entries

With `f` the actual flags and the entry `(crc, csize, usize)`, the prediction is `L` with:

| offset | field | prediction |
|---|---|---|
| 0, 2, 6, 8 | version needed, flags, time, date | as in `L` |
| 4 | method | 0 for a stored body, 8 for a deflated one, else as in `L` |
| 10 | CRC-32 | 0 if bit 3 of `f` is set, else `crc` |
| 14, 18 | compressed, uncompressed size | 0 if bit 3 of `f` is set; else `0xFFFFFFFF` if the extra field has zip64 data; else `low32` of `csize`, `usize` |
| 22, 24 | name length, extra field length | those of the name and extra field |

The zip64 data is coded with `usize` then `csize` (8 bytes each), or two zeros if bit 3 of `f`
is set. The data descriptor's fields are predicted as `crc`, then `csize` and `usize`, of 4
bytes (`low32`) or 8. After the entry, `L` is its fixed part.

### 4.2 Central directory headers

With the local entry referred to, at position `q`, and its entry, the prediction is `C` with:

| offset | field | prediction |
|---|---|---|
| 0 | version made by | as in `C` |
| 2–11 | version needed, flags, method, time, date | the local entry's (bytes 0–9 of its fixed part); as in `C` without one |
| 12 | CRC-32 | `crc`; 0 without a local entry |
| 16, 20 | compressed, uncompressed size | `clip32` of `csize`, `usize`; 0 without |
| 24, 26, 28 | name, extra field, comment lengths | those given |
| 30, 32, 34 | disk number start, internal and external attributes | as in `C` |
| 38 | offset of the local header | `clip32` of the offset of `q`; 0 without |

The zip64 data is coded with the values, in this order, of those of the actual fields that are
all ones: uncompressed size (`usize`, 8 bytes) if the field at 20 is `0xFFFFFFFF`, compressed
size (`csize`, 8) if that at 16 is, the offset of `q` (8) if that at 38 is, and 0 (4 bytes) if
the disk number start at 30 is `0xFFFF`. Without a local entry, `usize`, `csize` and the offset
are 0. After the header, `C` is its fixed part.

### 4.3 End records

For a zip64 end of central directory record at position `q` with `e` bytes of extensible data,
the prediction is: size of the record `44 + e` (`le64`), version made by 45, version needed 45,
both disk numbers 0, both entry counts `count` (`le64`), the size of the directory `bytes`
(`le64`), its offset the offset of `start(q)` (`le64`). `e64` is then `q`.

For a locator: disk 0, the offset of `e64` (`le64`; 0 if there is none), 1 disk in all.

For an end of central directory record at position `q` with a comment of `c` bytes: both disk
numbers 0, both entry counts `min(count, 0xFFFF)`, the size of the directory `clip32(bytes)`,
its offset `clip32` of the offset of `start(q)`, comment length `c`. Then `count`, `bytes`,
`first` and `e64` are reset.

## 5. Encoding *(encoder)*

`aqr_zip::formula::encode_with` reads the archive from its start, record after record:

- The central directory is first found from the end: the last end of central directory record
  (within 128 KiB of the end) whose directory, by its size and the zip64 record if a locator is
  before it, starts with a central directory header. Its base is where the end record puts
  offset 0. Of that base and 0, the one putting more of the directory's offsets on local file
  headers is taken.
- At each position, a record is read if one starts there and fits: a local entry's body ends by
  the compressed size of the central directory, else of the local header (with zip64 data),
  else where the DEFLATE stream ends, else (stored, with bit 3) at a data descriptor giving its
  own distance as the size. Where bit 3 is set, a data descriptor is read in the form whose
  compressed size is the body's: with its signature, or without one where a record or the end
  of the archive follows it. A record whose local header the central directory does not know of
  must not run over one that it does. Bytes where no record starts are gaps.
- Each body is made in the first of these ways that applies: *same* if an earlier body is the
  same bytes (16 bytes or more); *deflated* (method 8, not encrypted) if its data fits within
  `Options::limit` and aqr-flate's record is shorter than the body plus 16 bytes; *stored*
  (method 0, not encrypted); else *kept*, with the CRC-32 and size of the central directory,
  else the local header, else the data descriptor.
- The limit bounds the data inflated in all. The bodies are measured by a parse that keeps no
  data, then inflated in order of their expansion, the least first, while they fit: past it, the
  most expanding (zip bombs, 42.zip's zeros) are kept as they are, and cost no memory.
- `base` is the base found. The layout is checked by building the archive from it with the
  bodies as they are, without aqr-flate, which must give the input; else there is no formula.
  An archive without a local entry has none either.

aqr-compress gives the limit as what is left of its budget for the whole file
(`Options::expansion`), and recompresses `data` in turn. An archive is found in data at a local
file header, and cut where its records end with an end record, or else at the end record found
from the end of the data.

## 6. Validation

The module fails if:

- the params are not empty, the inputs are not four, or the resources are neither none nor two;
- there are resources and no deflated body, or a deflated body and no resources, or records of
  different profiles;
- the layout is malformed: a truncated varint or field, an unknown tag, body, descriptor form or
  `same` bit, a name, extra field or comment longer than 65535 bytes, a `ref` naming no earlier
  entry, `same` without a local entry, `k` of 0 or past the first entry, or a range past the end
  of `data`, `records` or `kept`;
- `data`, `records` or `kept` is not used up exactly;
- aqr-flate fails on a deflated body;
- the output is not `len` bytes long.
