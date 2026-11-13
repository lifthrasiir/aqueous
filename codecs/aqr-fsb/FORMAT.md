# aqr-fsb format

FMOD's FSB5 sound banks store Vorbis streams without their setup headers: a sample keeps only the
CRC-32 of the setup it was encoded with, and FMOD's runtime supplies the header. Recompressing
such a stream needs the header: aqr-vorbis's setup database has every setup FMOD is known to use,
looked up by its CRC-32 (codecs/aqr-vorbis/FORMAT.md §8). This document describes how a bank is
split into its samples (§2) and how those are recoded (§3, §4).

No module is involved: the samples go to aqr-vorbis's and aqr-ima's modules, so nothing here
binds a decoder.

Contents:

1. [Conventions](#1-conventions)
2. [FSB5 banks](#2-fsb5-banks-encoder) *(encoder)*
3. [Vorbis samples](#3-vorbis-samples-encoder) *(encoder)*
4. [IMA ADPCM samples](#4-ima-adpcm-samples-encoder) *(encoder)*

## 1. Conventions

- Integers in headers are little-endian.

## 2. FSB5 banks (encoder)

How `aqr_fsb::formula::encode` splits a bank; no module is involved, so nothing here binds a
decoder.

A bank starts with a header: the magic `FSB5`, then little-endian 32-bit words: the version (0
or 1), the number of samples, the bytes of the samples' headers, of the names, and of the
samples' data, and the codec (`mode`: 2 16-bit PCM, 7 IMA ADPCM, 11 MPEG, 15 Vorbis, among
others). The header is 0x40 bytes in version 0 and 0x3C in version 1; the samples' headers,
the names and the data follow it in that order, and the bank ends with the data.

A sample's header is a little-endian 64-bit word, of which bit 0 tells that chunks follow and
bits 7 to 33 are the sample's offset in the data in units of 32 bytes; each chunk is a 32-bit
word, of which bit 0 tells that another follows and bits 1 to 24 are the bytes of the chunk
after the word. A sample's data runs from its offset to the next sample's, or to the end of the
data, padding included.

`encode` takes a bank whose version is 0 or 1, with at least one sample, whose samples' headers
take exactly their declared bytes, whose first offset is 0, whose offsets are in order within
the data, and whose data is within the input. Its formula is the concatenation of a stream of
the bytes before the data (class `fsb5/head`), a stream per sample's data (class `fsb5/vorbis`,
`fsb5/ima-adpcm`, `fsb5/pcm16`, `fsb5/mpeg` or `fsb5/data` by the codec), and a stream of what
follows the bank in the input (`fsb5/tail`), if anything.

aqr-compress finds banks by their header, anywhere in data (and by the extension `.fsb`), takes a
bank to the end of its data, and puts banks that share samples of 16 KiB or more (by their
CRC-32 and size) in one batch, where the samples' streams they share are stored once. A bank's
formula is taken only where it is smaller than the bank, so not for banks of no samples taken
(§3, §4).

## 3. Vorbis samples (encoder)

A Vorbis sample's data is its packets, each after its 16-bit length (little-endian), then a
length of 0 and padding (or nothing, where the packets fill it). Its Vorbis chunk (type 11)
starts with the CRC-32 of its setup header; its block sizes are 256 and 2048 samples; its channels
are its header's (FMOD codes more than two with a mono setup).

`encode` finds the setup by the CRC-32 (a row, else each candidate of the search in turn: codecs/aqr-vorbis/FORMAT.md §8.3)
and gives the packets to aqr-vorbis's module `vorbis-recons` (codecs/aqr-vorbis/FORMAT.md),
which takes them up to the length of 0: the sample is then that module's output and a stream of
what follows (class `fsb5/vorbis-end`). A sample whose setup is not found, or whose packets do not
parse under it, is kept as it is.

## 4. IMA ADPCM samples (encoder)

A sample of codec 7 is Xbox ADPCM: blocks of 36 bytes per channel (its header's channels), then
its padding. `encode` gives its whole blocks to aqr-ima's module `ima-recons`
(codecs/aqr-ima/FORMAT.md): the sample is then that module's output and a stream of what follows
(class `fsb5/ima-end`). A sample of no whole block is kept as it is.
