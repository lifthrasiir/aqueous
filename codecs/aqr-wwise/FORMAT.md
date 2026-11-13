# aqr-wwise format

Audiokinetic Wwise's Vorbis media (`.wem`) keep a Vorbis stream without its identification and
comment headers, with a stripped setup header whose codebooks are ids into a library of Wwise's
runtime, and audio packets without some of their first bits. This document describes the media
(§1), the setup header a stripped one stands for (§2) and a medium's formula (§3).

No module of its own is involved: the packets go to aqr-vorbis's module `vorbis-recons`
(codecs/aqr-vorbis/FORMAT.md), so nothing here binds a decoder.

Contents:

1. [Media](#1-media)
2. [The setup header](#2-the-setup-header-encoder) *(encoder)*
3. [A medium's formula](#3-a-mediums-formula-encoder) *(encoder)*
4. [How the table was made](#4-how-the-table-was-made-informative) *(informative)*

## 1. Media

Integers are little-endian. A medium is a RIFF WAVE (`RIFF`, its size, `WAVE`, then chunks: a
4-byte tag, a u32 size, the contents) whose first chunk is `fmt ` with format `FFFF`: then u16
channels and u32 sample rate. Its `vorb` fields are those of a chunk `vorb`, or where it has
none and `fmt ` has 0x42 bytes, the 0x2A bytes of `fmt ` from 0x18. Only `vorb` fields of 0x2A
bytes are taken (Wwise 2012 on, as far as seen); of them:

| offset | field |
|---|---|
| 0x00 | u32 the samples |
| 0x04 | u32 `mod_signal`: the packets are **stripped** (aqr-vorbis §5) unless it is 0x4A, 0x4B, 0x69 or 0x70 |
| 0x10 | u32 where the setup packet is, from the start of `data`'s contents |
| 0x14 | u32 where the first audio packet is, likewise |
| 0x24 | u32 an id of the setup (not used here) |
| 0x28, 0x29 | a byte each: the short and long block sizes' exponents |

A packet is a u16 length, then that many bytes. The audio packets follow each other up to the
end of `data`; a medium cut short (a bank's prefetched part of a streamed one) ends inside one.
What lies between the chunks' heads and the setup packet (a seek table) is not read.

## 2. The setup header *(encoder)*

The setup packet holds a **stripped** setup header: a Vorbis setup header (Vorbis I §4.2.4)
without its packet type, `vorbis` and time-domain list, in which

- each codebook is a 10-bit **id**, after the 8-bit codebook count less one;
- a floor leaves out its 16-bit type (always 1);
- a residue's type is 2 bits, not 16;
- a mapping leaves out its 16-bit type (always 0);
- a mode leaves out its window and transform types (16 bits each, always 0);
- there is no framing flag.

Everything else is as Vorbis has it, in its order. The setup header it stands for is that of
Vorbis with each id replaced by its codebook: the packet type 5, `vorbis`, the codebook count
less one, the codebooks, a time-domain list of one placeholder (6 bits 0, 16 bits 0), then the
floors, residues, mappings and modes with the fields left out put back, and the framing flag.

An id's codebook is a codebook part of aqr-vorbis's setup database (its FORMAT.md §8.1), by the
table `src/ids.rs` (id, part number). Where a setup has an id the table has not, the encoder
tries the database's skeletons that have as many codebooks and agree with the table on the ids
it has, in order, and takes the first under which the medium's first audio packets (up to 64,
but its last) parse to their ends (none cut short, at most 7 zero bits left).

## 3. A medium's formula *(encoder)*

`aqr_wwise::formula::encode` takes a medium whose stripped setup parses, whose block sizes'
exponents are from 6 to 13 (the short not above the long) and whose setup header is found
(§2). Its formula is the concatenation of a stream of its bytes up to the first audio packet's
length (class `wem/head`); the application of `vorbis-recons` to the setup header (§2) and the
record of the audio packets, each after its length, up to the last whole one (params: the
channels, the block sizes, stripped where the packets are); and a stream of what follows
(`wem/tail`), if anything. A medium whose packets do not parse is kept as it is.

aqr-compress finds media by their header at the start of data, or by the extension `.wem`
(banks name their media so, aqr-bundle's `wwise-bnk`).

## 4. How the table was made *(informative)*

`aqr-wwise ids FILE...` groups the media of `.wem` files, banks and other packs by their
stripped setups, and tries each skeleton of the database with as many codebooks on each,
keeping those under which every packet parses to its end; a wrong one parses few (0 to 27 of
400). Every setup but one of the media of seven games had exactly one such skeleton (README),
and the ids agreed across setups. Two ids (423, 438) are the same codebook.
