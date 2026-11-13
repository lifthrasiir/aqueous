# aqr-vorbis format

This document specifies the module `vorbis-recons` of aqr-formula entry formulas, which rebuilds
a Vorbis stream's audio packets from the stream's setup header and a **record** of the values the
packets code. The record codes each value as symbols of static frequency tables through a rANS
stream, in place of the packets' Huffman codes; the packets are written back from the values bit
for bit.

The document is normative and complete: the module can be implemented from it alone. Sections
marked *informative* describe this implementation's encoder; a decoder needs none of them.

External references:

- the Vorbis I specification (Xiph.Org), for the setup header (§4.2.4 and the codebook, floor 1
  and residue headers it holds, §3.2.1, §7.2.2, §8.6.1), and the decode of an audio packet
  (§4.3.1, with floor 1 §7.2.3 and residues §8.6.2). Floors of type 0 are not handled.
- aqr-mp3's FORMAT.md §8.1 to §8.3 (codecs/aqr-mp3), for the rANS coder: the slots of a table by
  its alias table, the decoder and the encoder (of one run here).

Contents:

1. [Conventions](#1-conventions)
2. [Overview](#2-overview)
3. [The module](#3-the-module)
4. [The record](#4-the-record)
5. [A packet's values](#5-a-packets-values)
6. [Keys and tables](#6-keys-and-tables)
7. [Encoding](#7-encoding-informative) *(informative)*
8. [The setup database](#8-the-setup-database-encoder) *(encoder)*

## 1. Conventions

- Integers are exact; `a / b` is the quotient rounded down. `bitlen(x)` is the number of bits of
  `x` (0 for 0); `ilog` is Vorbis I's (§9.2.1), the same.
- Bits of a packet are as Vorbis packs them: least significant bit of a byte first, an n-bit
  field's bit 0 first.
- **varint**: unsigned LEB128, least significant group first, at most 10 bytes.
- The **header** (§4) is a bit stream of the same order: bit `i` is bit `i mod 8` of byte
  `i / 8`; `k` bits of value `v` are its bit 0 first. `g(x)` is the Elias gamma code of `x + 1`:
  `bitlen(x + 1) − 1` zero bits, a 1, then the `bitlen(x + 1) − 1` bits of `x + 1` below its
  leading one (a field of that many bits).
- `zz(d)` of a signed `d` is `2d` for `d ≥ 0`, `−2d − 1` for `d < 0`.

## 2. Overview

A stream is coded in **packets** (Vorbis I §4.3), framed here each after its length: 16 bits,
little-endian, then the packet's bytes. The module outputs the framed packets of one stream,
all of them audio packets of one setup header.

A decoder of a packet (§4.3.1) reads, in an order the packet's own values decide, fields of a
fixed number of bits and entries of codebooks, coded by the books' Huffman codes. The values of a
packet are those, in that order (§5). Where the packet ends inside a field (end-of-packet), the
values stop before it. A packet is its values written in turn (a field's bits, an entry's
codeword), then its **tail**: the bits after the last value up to the end of its last byte.

The record holds the stream's packets' values as symbols of **keys** (§6): each key has a table
of frequencies, a default one the setup gives or one of its own the record gives. A key is chosen
for each value from its role, its book and the values coded before it; the symbols are in one
rANS stream.

## 3. The module

- **Module**: name `vorbis-recons`, version 3.
- **Params**: `varint channels` (1 to 255), then a byte: the short block size's exponent in its
  low 4 bits, the long one's in its high 4, each from 6 to 13 (64 to 8192 samples), the short
  not above the long. Then optionally a byte 1: the packets are **stripped** (Wwise's: no
  packet type, no window flags, §5). Nothing after.
- **Resources**: none.
- **Inputs**: exactly 2: `setup` (class `vorbis/setup`), the setup header (a whole packet of
  type 5, Vorbis I §4.2.4), parsed for the params' channels; `record` (class `vorbis/record`), §4.
- **Output**: the framed packets, `len` bytes (the declared length).

The setup header must parse under Vorbis I, with these restrictions: its floors are of type 1;
every codebook's codewords are assigned as §3.2.1 assigns them (in entry order, each used
entry the lowest codeword of its length that neither is prefixed by one taken nor prefixes one;
a book of a single used entry takes the codeword of zeros of its length); no codeword length
above 32; no assignment is overspecified. A tree with unused paths is allowed.

## 4. The record

```
varint H        the header's bytes
header          H bytes, a bit stream (§1), padded with zero bits to a byte
words           the rest: rANS words, u32 little-endian each (its length a multiple of 4)
```

The header holds, in order:

1. `g(P + 1)`: `P`, the packets.
2. `g(O + 1)`: `O`, the **odd** packets, then each, in increasing order of packet number `p`:
   `g(p − next + 1)` (`next` 0 for the first, else the previous odd packet's number plus 1);
   1 bit, whether it is cut short; where it is, `g(n + 1)` with `n` its values' count;
   `g(T + 1)` with `T` its tail's bits; then those `T` bits. A packet not among them is
   **whole** (its decode reads all it reads) and its tail is its padding, zero bits up to the end
   of the byte its last value ends in.
3. `g(K + 1)`: `K`, the keys with tables of their own, then each, in increasing order of key
   `k`: `g(k − next + 1)` (`next` as above), then its table's **levels**, one per symbol of the
   key's alphabet (§6.2): from `prev = 0`, `g(0)` then `g(r − 1)` is a run of `r` levels 0; any
   other `g(zz(l − prev) + 1)` is the level `l`, from 1 to 63, and `prev = l`. The levels must
   end exactly at the alphabet's end.

The words are the rANS stream (aqr-mp3 §8.2, one run: `start` once at the start): the
symbols of all packets' values in order, each read in its key's table (§6).

The decoder writes each packet's values and tail as §2 says and outputs its length (16 bits,
little-endian) and its bytes. It fails where:

- the header ends before what it holds (reading past its last byte), a value of it is out of
  range (`P` above `len / 3`, a packet number not below `P`, a tail of more than `8 × len`
  bits, a key not below the keys' count, a level out of 1 to 63), or a run of levels passes the
  alphabet;
- a value decoded is not one of the packet (a rank past the book's used entries);
- the decode of a whole packet ends short of the values, or a short one's decode reads all its
  values and ends without a missing one (its count not used up exactly);
- a packet's bits (its values and tail) do not fill whole bytes, or are none, or are more than
  65535 bytes, or the packets' bytes with their lengths exceed `len`, or end short of it;
- the rANS decoder reads past the words (`pos` above their count after any packet).

## 5. A packet's values

The decode of an audio packet of Vorbis I §4.3.1 reads, in order (each a **field** of `n` bits,
or an **entry** of a book, read by its Huffman code), with `C` the channels:

1. the packet type, 1 bit (field `Type`; 0, else the packet is not taken), but in stripped
   packets;
2. the mode number, `ilog(modes − 1)` bits (`Mode`), which must be a mode of the setup;
3. for a mode of long blocks, the previous and next window flags, 2 bits (`Window`), but in
   stripped packets;
4. for each channel `ch` in order, its floor (of the mode's mapping's submap of `ch`): 1 bit
   (`Nonzero`); where 1, Y[0] and Y[1], each `ilog(range − 1)` bits with `range` 256, 128, 86 or
   64 for multipliers 1 to 4 (`Y01`, `i` 0 and 1); then for each partition `part`, where its
   class has subclasses, an entry of its master book (`Master`), and each of the class's values
   `Y[i]` that has a book, an entry of it (`Y`), as §7.2.3 reads them;
5. each submap `s`'s residue, as §8.6.2 reads it, over the vectors of the channels of the
   submap in order (`v` their index there), those of floors unused and not coupled to a used one
   not decoded; for type 2 one vector of all of them interleaved (`v = 0`), not read at all where
   none is decoded. In pass 0, at the start of each run of partitions a classword covers, an
   entry of the classbook per vector decoded (`Class`, at partition `part`); then for each
   partition and vector, where its class has a book in pass `pass`, `reads` entries of it
   (`Vq`), `reads` the partition size divided by the book's dimensions (type 0: rounded down;
   types 1 and 2: up).

A field of 0 bits (the mode number of a setup of one mode) is a value 0, not coded. Where the
packet ends inside a field or a codeword (end-of-packet), the decode stops there.

## 6. Keys and tables

### 6.1 Keys

Keys are numbered in this order:

1. for each book `b` in order, 56 keys: those of its chunk 0 (§6.2) in each role: `Vq` (5
   contexts), `Master` (17), `Y` (17), `Class` (17); key `56b + o + x` for context `x`, `o` 0, 5,
   22 and 39;
2. for each book in order, a key for each of its chunks after the first;
3. for each kind of field in order `Type`, `Mode`, `Window`, `Nonzero`, `Y01` with `i = 0`, `Y01`
   with `i = 1` (of 1, 1, 1, 3, 33 and 33 contexts), for each bit count `n` from 1 to 8, a key
   for each context.

A key's alphabet is its chunk's (§6.2), or `2^n` for a field of `n` bits.

### 6.2 Entries and fields as symbols

A book's **ranks** order its used entries by codeword length, then by entry number; `n` is the
count of its used entries (taken as 1 for a book of none). Its **chunks** are 1 where `n ≤ 256`,
else 1 plus those of `n − 255`; chunk `c`'s alphabet is `min(n − 255c, 256)`. An entry of rank
`r` is coded as symbols: from chunk 0, while `r ≥ 255` and a later chunk follows, the symbol 255
(an escape) in this chunk, `r` less 255, on to the next chunk; then `r` in its chunk. A symbol of
chunk `c > 0` is of its book's key of that chunk. A field of `n` bits is the symbol of its value.

The symbol of chunk 0 of an entry, or of a field, is of the key of its role (or kind and bit
count) and context, from what the stream has coded before it:

- `Vq`: the magnitude `m` of the last `Vq` entry of the same vector index `min(v, C − 1)` and
  pass (of any submap) since this packet's `Mode` field: `min(m, 3)`, or 4 where there is none.
  An entry's magnitude, of a book of lookup type 1 with `q = lookup1_values(entries, dims)`, is
  `min(Σ_j (|2 e_j − (q − 1)| / 2), 254)` over its digits `e_j` (`e = Σ e_j q^j`); 0 for other
  books.
- `Master`, `Y`, `Class`, `Nonzero`, `Y01`: the value last kept in its **slot** of the packet's
  block size (the mode's, long or short; each block size has its slots apart): for `Master`, `Y`
  and `Class` `min(value, 15)` (16 where none), for `Nonzero` `min(value, 2)` (2 where none), for
  `Y01` `min(value / 8, 31)` (32 where none). Slots: channel `ch`'s `Nonzero` at `128ch`, `Y01`
  at `128ch + 1 + i`, `Master` of partition `part` at `128ch + 3 + min(part, 31)`, `Y[i]` at
  `128ch + 35 + min(i, 92)`; a `Class` of submap `s`, vector `v`, partition `part` at `32768 +
  (s × C + v) × 1024 + min(part, 1023)`. Each such value coded is then kept in its slot, as
  `min(value, 65534)`.
- `Type`, `Mode`, `Window`: context 0.

### 6.3 Tables

A key's table is its own, from the levels the header gives it, or its **default**: for a book's
chunk `c`, the weights of its symbols `i`: for the escape (`i = 255` with a later chunk), the sum
of the weights of the ranks from `255c + 255` on; else the weight of rank `255c + i`; where the
weight of an entry of a codeword of `len` bits is `2^(32 − len)`, and every symbol's weight is at
least 1. For a field of `n` bits, weight 1 for each symbol.

The weight of level `l` is 0 for `l = 0`, else `(F[(l − 1) mod 4] × 2^((l − 1) / 4)) / 256` with
`F = [65536, 77936, 92682, 110218]`.

A table's frequencies `f` (of 12 bits) from weights `w_0 … w_(m−1)` (`W` their sum): where `W =
0`, `f_i = 4096 / m`, plus 1 for `i < 4096 mod m`; else `f_i = 1 + (w_i × (4096 − m)) / W`, then,
while their sum is below 4096, one more each to the symbols in order of decreasing weight
(ties to the lower index), from the first, over and over.

## 7. Encoding *(informative)*

`aqr_vorbis::formula::packets` takes the framed packets at the start of data, up to its end or a
length of 0 (FSB5's end of a sample's packets), and the record of them; it does not take a
stream where a packet does not parse under the setup (aqr-fsb then keeps it as it is).

`aqr_vorbis::codec::encode` parses each packet to its symbols (with their keys) and tail,
counts each key's symbols, and gives a key a table of its own where the symbols' cost under it
plus its levels (the levels of counts: `63 + round(4 log2(count / max))`, at least 1; 0 for a
count of 0) and about 8 bits for its number is below their cost under the default.

Measured on The Crew's FSB5 banks (aqr-fsb's tool, one thread on Windows): of 12 large banks,
120.5 MB of framed packets, the record is 90.67%, encoded at 23 MB/s and decoded at 21 MB/s; of
950 small samples, 12.5 MB, 93.2%. Tried: `Vq` contexts of the magnitudes at the entry's position
in the previous packet and in the previous vector gained 0.01 pp for 20% of the time, dropped;
no `Vq` context costs 0.3 pp.

## 8. The setup database *(encoder)*

Streams kept without their setup headers (FMOD's FSB5 samples, Wwise's media) need the headers
rebuilt before their packets can be parsed; the crate carries a database from which the setups
libvorbis makes are assembled. Nothing here binds a decoder of the module: an archive holds the
setup a stream was coded with.

Conventions of this section, beside §1's:

- **Bit strings.** Vorbis packs bits least significant first: the first bit of a packet is bit 0
  of byte 0, the ninth is bit 0 of byte 1. An n-bit field with value v occupies the next n bits,
  bit 0 of v first. A bit string of n bits is written here as the integer whose bit i is the
  string's i-th bit, and stored as that integer in ⌈n/8⌉ bytes, little-endian, the unused high
  bits of the last byte zero.
- **u32**: 4 bytes, little-endian. **CRC-32**: zlib's (the reflected polynomial 0xEDB88320,
  initial value and final XOR 0xFFFFFFFF), over a setup header's bytes. FSB5 stores this value.

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
skeleton and other ends; §8.3.2 finds those from the CRC-32 alone.

### 8.1 The database

The database is the file `data/vorbis-setups.bin` (of this crate). In order:

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
number of its bits, then the bit string in ⌈n/8⌉ bytes (§8). Parts are numbered from 0 in their
list. Within a list no two parts are equal.

A residue part has at least 88 bits, and its bits 40 to 63 (its `end`, §8.2.1) are zero.

A **skeleton** is five id lists, in the order codebooks, floors, residues, mappings, modes. An
id list is a varint count, then that many varints, each the number of a part of that kind.
Counts are 1 to 256 for codebooks and 1 to 64 for the others; a part may occur more than once in
a list. Skeletons are numbered from 0.

A **row** is a u32 CRC-32, a varint skeleton number, then one varint `end` per id in that
skeleton's residue list, in its order. Rows are sorted by CRC-32, ascending, and no two have
the same CRC-32. A row's CRC-32 is that of the setup it assembles (§8.2); a reader may check it.

A reader rejects a database that breaks any of the above: a wrong magic or version, a varint
that does not end within the file, a part, skeleton or row that runs past the end, a part whose
last byte has nonzero unused bits, two equal parts in a list, a residue part under 88 bits or
with a nonzero `end`, an id or skeleton number out of range, counts out of range, an `end` of
2^24 or more, or bytes after the last row. Part-list, skeleton and row counts have no bound of
their own beyond what the file holds.

### 8.2 Assembling a setup

#### 8.2.1 Residue parts

A residue's bits begin (Vorbis I §8.6.1) with its type (16 bits), `begin` (24), `end` (24),
`partition size − 1` (24), then fields that end the part. So in a residue part, bits 40 to 63
hold `end` (zero in the database) and bits 64 to 87 hold the partition size minus one.

#### 8.2.2 Assembly

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

### 8.3 Finding a setup by its CRC-32

#### 8.3.1 Rows

Given a CRC-32 from an FSB5 sample, a reader first looks for a row with that CRC-32. Every
setup FMOD is known to use has one.

#### 8.3.2 Search

With no row, a reader may search the skeletons. A candidate is a skeleton s with ends e₀, e₁, …
where each eᵢ is a multiple of residue i's partition size (§8.2.1) with 0 ≤ eᵢ ≤ 2048. (FMOD's
long blocks are 2048 samples; no setup of its has a larger `end`.) The search
returns every candidate whose assembled setup has the CRC-32 sought, over all skeletons.

A match is only probable: among the up to 44 × 129² candidates some other setup can share a
CRC-32 by chance (about one sought CRC-32 in 8,000). The caller treats a found setup as a guess and confirms it by the stream, which a
wrong setup does not parse (or does not reproduce).

*Informative: searching fast.* The ends change only fixed bit positions of a setup of fixed
length. CRC-32 is affine over equal-length messages: crc(a ⊕ b) = crc(a) ⊕ crc(b) ⊕ crc(0ⁿ),
with 0ⁿ the zero message of that length. So with x₀ the skeleton assembled with all ends zero,
and Dᵢ(e) the CRC-32 of the message that is zero but for e at residue i's end field, minus
crc(0ⁿ), the CRC-32 of the setup with ends eᵢ is crc(x₀) ⊕ D₀(e₀) ⊕ D₁(e₁) ⊕ …, each Dᵢ
linear in e (the XOR of its values at e's set bits). Meet in the middle: tabulate the XOR of
the Dᵢ of some residues over their candidates in a map, and for each combination of the others
look up crc(x₀) ⊕ their Dᵢ ⊕ target. Every database skeleton has one or two residues.

### 8.4 How the database was made (informative)

The rows' setups are those of `vorbis_encode_setup_vbr(channels, rate, quality)` of libvorbis
1.3.7 changed as FMOD changed it. FMOD codes more than two channels with the mono setups, so channels
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

Then the setups of unmodified libvorbis 1.3.7 (from Xiph.Org's release) at FSB5's rates, 6000,
7000, 12000 and 96000 Hz, channels 1 and 2 and the same qualities, 268 setups, added their parts
and skeletons after those (`vorbis_setup_db.py --base`), and no rows: Wwise's media use these
(blocks of 512 and 4096 below quality 0, which FMOD's change does not make).

The database has 524 codebooks (71,648 bytes), 13 floors, 41 residues, 4 mappings and 2 modes,
44 skeletons and 260 rows, 79,861 bytes in all. Of the rows, 197 are setups unmodified
libvorbis makes and 63 only the changed one; all 161 of vgmstream's table are among them.
