# aqr-fsb format

FMOD's FSB5 sound banks store Vorbis streams without their setup headers: a sample keeps only the
CRC-32 of the setup it was encoded with, and FMOD's runtime supplies the header. Recompressing
such a stream needs the header, so aqr-fsb carries a database from which every setup FMOD is
known to use is assembled, looked up by its CRC-32. This document describes the database (§3),
how a setup is assembled from it (§4) and found by a CRC-32 (§5).

The document is normative and complete: an implementation can be written from it alone.
Sections marked *informative* describe how the database was made; a reader of it needs none of
them.

External reference: the Vorbis I specification (Xiph.Org), §4.2.4 "Setup header" and the
sections it refers to. Only §4 here leans on it, and §4 restates what it needs.

Contents:

1. [Conventions](#1-conventions)
2. [Overview](#2-overview)
3. [The database](#3-the-database)
4. [Assembling a setup](#4-assembling-a-setup)
5. [Finding a setup by its CRC-32](#5-finding-a-setup-by-its-crc-32)
6. [How the database was made](#6-how-the-database-was-made-informative) *(informative)*

## 1. Conventions

- **Bit strings.** Vorbis packs bits least significant first: the first bit of a packet is bit 0
  of byte 0, the ninth is bit 0 of byte 1. An n-bit field with value v occupies the next n bits,
  bit 0 of v first. A bit string of n bits is written here as the integer whose bit i is the
  string's i-th bit, and stored as that integer in ⌈n/8⌉ bytes, little-endian, the unused high
  bits of the last byte zero.
- **varint**: an unsigned integer in LEB128: 7 bits per byte, least significant group first,
  the high bit set on every byte but the last. Encoders use the shortest form; decoders reject
  a varint over 10 bytes or above 2^64 − 1.
- **u32**: 4 bytes, little-endian.
- **CRC-32**: zlib's (the reflected polynomial 0xEDB88320, initial value and final XOR
  0xFFFFFFFF), over a setup header's bytes. FSB5 stores this value.

## 2. Overview

A setup header (Vorbis I §4.2.4) is a list of codebooks, then a time-domain placeholder, then
lists of floors, residues, mappings and modes, then a framing bit. Every item of these lists is
a contiguous run of bits. The setups libvorbis makes, FMOD's among them, share nearly all of
these runs, so the database stores each distinct run once, as a **part**, and each setup as the
list of its parts.

One field varies where nothing else does: a residue's `end`, which follows the sample rate and
the encoder's quality. Parts of residues store `end` as zero, and a setup gives its residues'
ends separately. A **skeleton** is the list of parts of a setup; a setup is a skeleton and one
`end` per residue of it.

The database lists the skeletons, and **rows** pairing a CRC-32 with a skeleton and its ends for
the setups FMOD makes at its standard sample rates. A setup made at another rate has a known
skeleton and other ends; §5.2 finds those from the CRC-32 alone.

## 3. The database

The database is the file `data/vorbis-setups.bin`. In order:

| field | contents |
|---|---|
| magic | the 4 bytes `AQVS` |
| version | 1 byte, 1 |
| codebooks | a part list |
| floors | a part list |
| residues | a part list |
| mappings | a part list |
| modes | a part list |
| skeleton count | varint |
| skeletons | that many skeletons |
| row count | varint |
| rows | that many rows |

and nothing after.

A **part list** is a varint count, then that many parts. A **part** is a varint n ≥ 1, the
number of its bits, then the bit string in ⌈n/8⌉ bytes (§1). Parts are numbered from 0 in their
list. Within a list no two parts are equal.

A residue part has bits 40 to 63 (its `end`, §4.1) zero.

A **skeleton** is five id lists, in the order codebooks, floors, residues, mappings, modes. An
id list is a varint count, then that many varints, each the number of a part of that kind.
Counts are 1 to 256 for codebooks and 1 to 64 for the others; a part may occur more than once in
a list. Skeletons are numbered from 0.

A **row** is a u32 CRC-32, a varint skeleton number, then one varint `end` per id in that
skeleton's residue list, in its order. Rows are sorted by CRC-32, ascending, and no two have
the same CRC-32. A row's CRC-32 is that of the setup it assembles (§4); a reader may check it.

A reader rejects a database that breaks any of the above: a wrong magic or version, a varint
that does not end within the file, a part, skeleton or row that runs past the end, an id or
skeleton number out of range, counts out of range, an `end` of 2^24 or more, or bytes after the
last row.

## 4. Assembling a setup

### 4.1 Residue parts

A residue's bits begin (Vorbis I §8.6.1) with its type (16 bits), `begin` (24), `end` (24),
`partition size − 1` (24), then fields that end the part. So in a residue part, bits 40 to 63
hold `end` (zero in the database) and bits 64 to 87 hold the partition size minus one.

### 4.2 Assembly

Given a skeleton and its ends e₀, e₁, … (one per residue id, each below 2^24), the setup header
is the bit string made of, in order:

1. 8 bits: 5 (the packet type).
2. 6 × 8 bits: the bytes of `vorbis` (0x76 0x6F 0x72 0x62 0x69 0x73), each byte a field of 8
   bits.
3. 8 bits: the number of codebook ids − 1.
4. Each codebook id's part, in order.
5. 6 bits: 0 (one time-domain placeholder), then 16 bits: 0 (its value).
6. 6 bits: the number of floor ids − 1, then each floor id's part, in order.
7. 6 bits: the number of residue ids − 1, then each residue id's part, in order, the i-th with
   eᵢ added at bit 40: the part's value plus eᵢ × 2^40.
8. 6 bits: the number of mapping ids − 1, then each mapping id's part, in order.
9. 6 bits: the number of mode ids − 1, then each mode id's part, in order.
10. 1 bit: 1 (the framing flag).

Then zero bits up to a byte boundary. The bytes are the setup header, a complete Vorbis packet.
The parts are concatenated as they are: they contain references to other items by their
position in the setup, and the skeleton's order keeps those positions.

A row's setup is its skeleton assembled with its ends.

## 5. Finding a setup by its CRC-32

### 5.1 Rows

Given a CRC-32 from an FSB5 sample, a reader first looks for a row with that CRC-32. Every
setup FMOD is known to use has one.

### 5.2 Search

With no row, a reader may search the skeletons. A candidate is a skeleton s with ends e₀, e₁, …
where each eᵢ is a multiple of residue i's partition size (§4.1) with 0 ≤ eᵢ ≤ 2048. (FMOD's
long blocks are 2048 samples; no setup of its has a larger `end`.) The search
returns every candidate whose assembled setup has the CRC-32 sought, over all skeletons.

A match is only probable: among the up to 32 × 129² candidates some other setup can share a
CRC-32 by chance (about one sought CRC-32 in 8,000). The caller treats a found setup as a guess and confirms it by the stream, which a
wrong setup does not parse (or does not reproduce).

*Informative: searching fast.* The ends change only fixed bit positions of a setup of fixed
length. CRC-32 is affine over equal-length messages: crc(a ⊕ b) = crc(a) ⊕ crc(b) ⊕ crc(0ⁿ),
with 0ⁿ the zero message of that length. So with x₀ the skeleton assembled with all ends zero,
and Dᵢ(e) the CRC-32 of the message that is zero but for e at residue i's end field, minus
crc(0ⁿ), the CRC-32 of the setup with ends eᵢ is crc(x₀) ⊕ D₀(e₀) ⊕ D₁(e₁) ⊕ …, each Dᵢ
linear in e (the XOR of its values at e's set bits). For two residues: tabulate D₁ over its
candidates in a map, and for each e₀ look up crc(x₀) ⊕ D₀(e₀) ⊕ target. Every database skeleton
has one or two residues.

## 6. How the database was made (informative)

The setups are those of `vorbis_encode_setup_vbr(channels, rate, quality)` of libvorbis 1.3.7
changed as FMOD changed it. FMOD codes more than two channels with the mono setups, so channels
are 1 or 2. The change, inferred by matching libvorbis's output against the 161 setups known
from FMOD (vgmstream's table), is described in `scripts/vorbis_setups.py`; with it, every one of
the 161 is reproduced. Unmodified libvorbis (1.1.0 to 1.3.7 agree) gives 107 of them.

`scripts/vorbis_setups.py` runs a harness (`scripts/vorbis_setups_harness.c`, libvorbis's public
API only) over the rates, channels 1 and 2 and quality −0.1 to 1.0 every 0.001.
`scripts/vorbis_setup_db.py` splits the setups into parts, checks that each assembles back byte
for byte, and writes the database and `data/vorbis-setups.tsv`. Every setup found contributes
its skeleton; rows are those found at FSB5's rates (8000, 11000, 11025, 16000, 22050, 24000,
32000, 44100, 48000 Hz) and 96000 Hz. The TSV gives, per row, the skeleton and ends, whether
unmodified libvorbis makes it (`libvorbis`) or only the changed one (`fmod-rule`), whether
vgmstream's table has it, and the channels, rates and quality intervals that give it.

The changed libvorbis is not in this repository. The database holds the parts of the setups,
as found in libvorbis's output (Xiph.Org's codebooks among them).

The database has 480 codebooks (69,374 bytes), 7 floors, 32 residues, 4 mappings and 2 modes,
32 skeletons and 260 rows, 76,133 bytes in all. Of the rows, 197 are setups unmodified
libvorbis makes and 63 only the changed one; all 161 of vgmstream's table are among them.
