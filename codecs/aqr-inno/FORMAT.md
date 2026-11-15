# aqr-inno formula

aqr-inno has no module of its own. The formula of an Inno Setup installer is the installer's
bytes in order, with each LZMA stream of its file data an application of aqr-lzma's `lzma-recons`
([its FORMAT.md](../aqr-lzma/FORMAT.md)) over the stream's data and record, and the rest slices of
the installer's own streams (aqr-exe's, as the installer is a PE image). This document tells what
is taken, and is *informative*.

External reference: Inno Setup's source layout (`Setup.Loader`, `Struct`), as read of public
descriptions; no Inno Setup code is read.

## The installer

A PE image whose loader holds the **offset table**, 44 bytes:

| at | size | |
|---|---|---|
| 0 | 12 | `rDlPtS\xcd\xe6\xd7\x7b\x0b\x2a` |
| 12 | 4 | version, 1 |
| 16 | 4 | total size of the installer (without a certificate after it) |
| 20 | 4 | offset of the compressed setup program |
| 24 | 4 | its data size once uncompressed |
| 28 | 4 | its CRC-32 |
| 32 | 4 | offset of the setup header |
| 36 | 4 | offset of the file data |
| 40 | 4 | CRC-32 of the 40 bytes before |

All fields are little-endian. The file data (after the image's sections), the setup header and the
setup program lie in this order, the last ending at the total.

Before 5.1.5 (4.0 on), the DOS header holds `Inno` at 0x30, then the offset of the table and its
complement (`le32` each); the table, 40 bytes, ends the installer:

| at | size | versions 04, 05 (seen of 4.0.0a, 4.0.5) | version 07 (seen of 4.1.8, 5.0.4) |
|---|---|---|---|
| 0 | 12 | `rDlPtS04\x87eVx`, `rDlPtS05\x87eVx` | `rDlPtS07\x87eVx` |
| 12 | 4 | total size | total size |
| 16 | 4 | offset of the setup program | offset of the setup program |
| 20 | 4 | its compressed size | its uncompressed size |
| 24 | 4 | its uncompressed size | its CRC-32 |
| 28 | 4 | its CRC-32 | offset of the setup header |
| 32 | 4 | offset of the setup header | offset of the file data |
| 36 | 4 | offset of the file data | CRC-32 of the 36 bytes before |

The file data comes first; in 4.0 the setup program, then the setup header; from 4.1 the header,
then the program. The file data of 4.0 is of zlib (by default) or bzip2 chunks, of the same head.

The **file data** is chunks one after another, to the setup header: `zlb\x1a`, the LZMA properties
byte, the dictionary size (`le32`), then the stream (LZMA1 as in `.lzma` files: it starts with the
range coder's zero byte, and ends with the end marker). A solid installer has one chunk; otherwise
each file is one.

## What is taken

A table is taken where its CRC-32 holds, its signature and version are right, and
`data < header < exe < total <= file size`, the data after the table; an earlier one where its
pointer's complement holds, its signature is of a version above, its CRC-32 holds (07), and the
data comes before the setup program and the header, both before the table, which is within the
total. The file data ends at the setup header or the setup program, whichever comes first. Chunks are walked from the data's
offset: a chunk is taken where its properties byte is valid and its stream decodes to its end marker
within the bytes before the setup header, and is what encoding its packets gives
(aqr-lzma's `recons::split`). The walk ends at the first chunk that fails; those before it are taken.

Of zlib (4.0): the chunks are `zlb\x1a`, then a zlib stream (RFC 1950: a header of deflate with a
window of up to 32 KiB and no dictionary, then its Adler-32), walked as those of LZMA, a chunk
taken where its stream inflates and its checksum holds; and the setup program, a zlib stream where
it comes before the setup header (tables 04 and 05).

## The formula

The streams are those of aqr-exe's formula of the image (the rest of the image, the 9 bytes before
each stream included), and for each LZMA stream three more: `inno` (its data, a content scanned in
turn), `lzma/record` and `lzma/index` (empty). The node of the stream is the application of
`lzma-recons` to the three, of the stream's length. A zlib stream is a content of the format zlib
(class `inno`), recompressed in turn as zlib streams are (aqr-flate).
