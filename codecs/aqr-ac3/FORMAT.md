# aqr-ac3 format

aqr-ac3 codes a byte stream made of AC-3 syncframes and the bytes between them. Each syncframe is
walked as A/52 describes it and its elements coded under contexts; the other bytes are kept. From
the coded stream the byte stream is rebuilt bit for bit by the module `ac3-recons` (§6).

The coding of elements (the range coder, probabilities, hashing, the `vlc`, `flag`, `field` and
`raw` primitives) is that of [aqr-walk](../../crates/aqr-walk/README.md), *Coding*, and is not
repeated here.

External reference: ATSC A/52 (*Digital Audio Compression (AC-3) Standard*), referred to as A/52
with its section numbers. This document is normative for the coding; the syntax and the bit
allocation are A/52's, with the restrictions of §3.

Contents:

1. [Conventions](#1-conventions)
2. [The byte stream](#2-the-byte-stream)
3. [Syncframes](#3-syncframes)
4. [Contexts](#4-contexts)
5. [The coded stream](#5-the-coded-stream)
6. [Formula module](#6-formula-module)
7. [The encoder](#7-the-encoder-informative) *(informative)*

## 1. Conventions

- Integers are unsigned. A **varint** is unsigned LEB128, least significant group first.
- Bits of a frame are read most significant bit first.
- `FRAME_MAX` is 3840 bytes (the longest frame: 640 kbps at 32 kHz); `RUN_PIECE` is 4096.

## 2. The byte stream

The byte stream is a list of **records**, each a byte range of it:

- a **frame**: a syncframe, starting at the syncword `0x0B77`, whose length `n` (from `fscod`
  and `frmsizecod`, A/52 table 5.18) is at most `FRAME_MAX`, which lies wholly in the stream,
  whose checksums are right (§3.2) and which parses by §3 to exactly `n` bytes;
- a **run**: the bytes between frames, never empty.

The encoder scans from the start: at each position a frame if one starts there (the test above),
else the byte joins the run. The records tile the stream.

## 3. Syncframes

A frame is walked once for both directions. Elements are in A/52 5.3–5.4 order; the notation is
that of aqr-walk's `Io`: `field(n, id)` is a field of `n` bits coded with the context of §4.1
for the field number `id`; `expect(v, n)` an element that must be `v` and costs nothing;
`derived(n)` bits that follow from the others.

### 3.1 Restrictions

A frame is a frame of this format only if all of these hold, else it is a part of a run:

- `bsid` is 8; `fscod` is not 3; `frmsizecod` is below 38.
- `crc1` and `crc2` are right (§3.2); `auxdatae` is 0 and `crcrsv` is 0; the bits between the
  last block and the auxiliary data field are zero and number at most 32.
- every constraint the walk checks holds: exponents between 0 and 24 (and group values below
  125), `chbwcod` at most 60, a coupling band count above 0, the coupling range inside the
  transform (`cplendmant ≤ 256`, above `cplstrtmant`).

### 3.2 Checksums

`syncword` is `expect(0x0B77, 16)`, `crc1` is `derived(16)`, and so is `crc2` at the end: they are
not coded, and the decoder sets them. They are the CRC-16 of polynomial `x^16 + x^15 + x^2 + 1`
(`0x8005`), zero register, over the frame after the syncword: `crc2` makes the CRC of
bytes `2..n` zero; `crc1` makes the CRC of bytes `2..n58` zero, where `n58 = 2 × (⌊w/2⌋ +
⌊w/8⌋)` and `w = n/2` (the first five eighths of the frame, in words). The decoder writes the
frame with zeros for both, then sets `crc1` first (it is the 16-bit word that brings the register
at `n58` to zero, solved from the register stepped back from zero over bytes `n58..4`), then
`crc2` over bytes `2..n − 2` (which includes `crc1`).

### 3.3 Syntax

The header's elements, with their field numbers (`id`) and widths; a field present only on a
condition is skipped otherwise.

| id | field | bits | when |
|---|---|---|---|
| 0 | fscod | 2 | |
| 1 | frmsizecod | 6 | |
| 2 | bsid | 5 | must be 8 |
| 3 | bsmod | 3 | |
| 4 | acmod | 3 | |
| 5 | cmixlev | 2 | `acmod & 1` and `acmod ≠ 1` |
| 6 | surmixlev | 2 | `acmod & 4` |
| 7 | dsurmod | 2 | `acmod = 2` |
| 8 | lfeon | 1 | |
| 9 | dialnorm | 5 | |
| 10, 11 | compre, compr | 1, 8 | compr if compre |
| 12, 13 | langcode, langcod | 1, 8 | langcod if langcode |
| 14, 15, 16 | audprodie, mixlevel, roomtyp | 1, 5, 2 | the last two if audprodie |
| 17–24 | dialnorm2, compr2e, compr2, langcod2e, langcod2, audprodi2e, mixlevel2, roomtyp2 | 5, 1, 8, 1, 8, 1, 5, 2 | `acmod = 0`; the pairs as above |
| 25 | copyrightb, origbs | 2 | |
| 26, 27 | timecod1e, timecod1 | 1, 14 | |
| 28, 29 | timecod2e, timecod2 | 1, 14 | |
| 30, 31 | addbsie, addbsil | 1, 6 | then `addbsil + 1` bytes of `field(8)` with context `0x3000` |

Then six blocks. In block `blk`, `b0` is true for the first block; `nf` is the number of full
bandwidth channels of `acmod` (2, 1, 2, 3, 3, 4, 4, 5 for acmod 0..7). Fields and their ids:

| id | field | bits | when |
|---|---|---|---|
| 40 + ch | blksw | 1 | each channel |
| 45 + ch | dithflag | 1 | each channel |
| 50, 51 | dynrnge, dynrng | 1, 8 | dynrng if dynrnge |
| 52, 53 | dynrng2e, dynrng2 | 1, 8 | `acmod = 0`; dynrng2 if the flag |
| 54 | cplstre | 1 | |
| 55 | cplinu | 1 | if cplstre |
| 56 + ch | chincpl | 1 | if cplinu, each channel |
| 61 | phsflginu | 1 | if cplinu and `acmod = 2` |
| 62, 63 | cplbegf, cplendf | 4, 4 | if cplinu; `ncplsubnd = 3 + cplendf − cplbegf` must be above 0 |
| 64 | cplbndstrc | 1 | `ncplsubnd − 1` times; `ncplbnd = ncplsubnd − (their sum)` |
| 65 | cplcoe | 1 | if cplinu, each channel in coupling |
| 66 | mstrcplco | 2 | if cplcoe |
| 67, 68 | cplcoexp, cplcomant | 4, 4 | `ncplbnd` times each, if cplcoe |
| 69 | phsflg | 1 | `ncplbnd` times, if `acmod = 2`, phsflginu and cplcoe of channel 0 or 1 |
| 70 | rematstr | 1 | `acmod = 2` |
| 71 | rematflg | 1 | 4 times if `cplbegf > 2` or no coupling; 3 if `cplbegf > 0`; else 2 |
| 72 | cplexpstr | 2 | if cplinu |
| 73 + ch | chexpstr | 2 | each channel |
| 78 | lfeexpstr | 1 | if lfeon |
| 79 | chbwcod | 6 | each channel with `chexpstr ≠ 0` and not in coupling; at most 60 |
| 80 | cplabsexp | 4 | if cplinu and `cplexpstr ≠ 0` |
| 81 + ch | absexp | 4 | each channel with `chexpstr ≠ 0` |
| 86 + ch | gainrng | 2 | after the exponents of that channel |
| 91 | lfeabsexp | 4 | if lfeon and `lfeexpstr ≠ 0` |
| 92 | baie | 1 | |
| 93–97 | sdcycod, fdcycod, sgaincod, dbpbcod, floorcod | 2, 2, 2, 2, 3 | if baie |
| 98 | snroffste | 1 | |
| 99 | csnroffst | 6 | if snroffste |
| 100, 101 | cplfsnroffst, cplfgaincod | 4, 3 | if snroffste and cplinu |
| 102 + ch, 107 + ch | fsnroffst, fgaincod | 4, 3 | each channel, if snroffste |
| 112, 113 | lfefsnroffst, lfefgaincod | 4, 3 | if snroffste and lfeon |
| 114 | cplleake | 1 | if cplinu |
| 115, 116 | cplfleak, cplsleak | 3, 3 | if cplleake |
| 117 | deltbaie | 1 | |
| 118 | cpldeltbae | 2 | if deltbaie and cplinu |
| 119 | deltbae | 2 | each channel, if deltbaie |
| 120–123 | cpldeltnseg, cpldeltoffst, cpldeltlen, cpldeltba | 3, 5, 4, 3 | if cpldeltbae = 1: `cpldeltnseg + 1` segments of the last three |
| 124–127 | deltnseg, deltoffst, deltlen, deltba | 3, 5, 4, 3 | for each channel with deltbae = 1: `deltnseg + 1` segments |
| 128, 129 | skiple, skipl | 1, 9 | then `skipl` bytes of `field(8)` with context `0x3001` |

The values that persist across blocks (A/52's: coupling parameters, strategies, bit-allocation
parameters, the exponents, delta bit allocation) are kept in the frame's state and updated when
their flags say so.

After the fields of a block come, in this order, the **exponents** and the **mantissas** that
A/52 puts in the block:

- Exponents (5.4.3.17–5.4.3.22, strategies reuse/D15/D25/D45 as `expstr` 0..3, with group sizes
  0, 1, 2, 4): for the coupling channel, `ngrps = (cplendmant − cplstrtmant) / (3 × grpsize)`
  groups, its absolute exponent being `cplabsexp << 1`; for each channel with `chexpstr ≠ 0`,
  with `endmant = cplstrtmant` if in coupling, else `37 + 3 × (chbwcod + 12)`: `ngrps = (endmant
  − 1) / 3`, `(endmant − 1 + 3) / 6` or `(endmant − 1 + 9) / 12` for D15, D25, D45; for lfe, 2
  groups of size 1. Each group is `field(7)` with the context of §4.2 and a value `v` below 125
  whose three exponent differences are `⌊v/25⌋ − 2`, `⌊v mod 25 / 5⌋ − 2`, `v mod 5 − 2`, each
  applied to the previous exponent (every exponent between 0 and 24) and repeated `grpsize`
  times. `cplstrtmant = 37 + 12 × cplbegf`, `cplendmant = 37 + 12 × (cplendf + 3)`.
- Bit allocation (A/52 7.2.2) is run for each channel, the coupling channel and lfe unless all of
  `csnroffst`, `cplfsnroffst`, `lfefsnroffst` and `fsnroffst` are 0 (then every `bap` is 0). It
  is integer arithmetic with the standard's tables (`src/ac3_tables.rs`): `snroffset = (((csnroffst −
  15) << 4) + fsnroffst) << 2`, fast gain `FASTGAIN[fgaincod]`, the coupling channel with the
  leak initial values `(cplfleak << 8) + 768` and `(cplsleak << 8) + 768`, the lfe channel over
  bins 0..7. `bap` is the result of the standard's table.
- Mantissas (5.4.3.61, 7.3): in channel order, for each `bin` below `endmant` of the channel the
  mantissa with the width of `bap`, and, after the first channel in coupling, the coupling
  channel's mantissas from `cplstrtmant` to `cplendmant`; then lfe's bins 0..7. A mantissa is
  `field(MANT_BITS[bap])` with `MANT_BITS = [0, 5, 7, 3, 7, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14, 16]`.
  `bap` 1, 2 and 4 are **grouped**: the first of a group of 3, 3 and 2 mantissas is the bits of
  the whole group (5, 7, 7 bits); the others of the group (2, 2 and 1 more of that `bap`) cost
  nothing. A group is shared across channels: it is counted per `bap` within the block, not per channel. `bap` 0 has no
  bits.

At the end: `used` is the bits consumed, `pad = n × 8 − used − 18`; `pad` must be between 0 and
32. `expect(0, pad)` (padding), `expect(0, 1)` (`auxdatae`), `expect(0, 1)` (`crcrsv`),
`derived(16)` (`crc2`).

## 4. Contexts

### 4.1 Fields

`ctx = (id << 20) ^ (last[id] × 0x9E3779B1) ^ b0` (32-bit wrapping; `b0` is 1 in the first block
and 0 elsewhere, and 0 in the header), where `last[id]` is the value that field had when last
coded (initially 0), kept across blocks and frames of the chunk. Then `field(n, ctx)`.

### 4.2 Exponent groups

`ctx = 0x4000 + kind × 64 + min(⌊pg / 25⌋, 4) + 5 × min(⌊above / 25⌋, 4)`, where `kind` is the
channel (0..4), 5 for the coupling channel and 6 for lfe; `pg` is the value of the previous group
of this channel in this exponent set (0 for the first); `above` the value that group number
`min(g, 63)` of `kind` had when last coded. Then `field(7, ctx)`.

### 4.3 Mantissas

Let `m[bap]` be the last mantissa value coded with that `bap` (initially 0; for a group, the
group's bits). For `bap` 1, 2 and 4, `ctx = 0x5000 + bap × 64 + (m[bap] & 63)`; for the others,
`ctx = 0x5000 + bap × 64 + (m[bap] >> max(MANT_BITS[bap] − 3, 0))`. Then `field(MANT_BITS[bap], ctx)`.

(The state of §4.1–§4.3 is reset at the start of a chunk, §5.)

## 5. The coded stream

The stream `ac3/frames`:

```
varint  n                     the number of chunks (at most 2^24)
n ×  { varint bytes, varint coded }
the n chunks, of `coded` bytes each, one after the other
```

A chunk is a range coder stream (aqr-walk) that decodes to `bytes` bytes of the byte stream, with
a new model of `2^21` probabilities and a new state. The sum of `bytes` is the length of the
byte stream. In a chunk, each record, in order:

```
flag K = slot(0xFFFF0001, p)        1 if a frame, 0 if a run; p the previous record's K (0 at first)
frame:   the walk of §3, as coded
run:     the length, 32 flat bits (most significant first); then the bytes, each bit flat
```

A run decodes in pieces of at most `RUN_PIECE` bytes; a run of length 0 is a corrupt stream.

## 6. Formula module

`ac3-recons` version 1: no params; one input, the coded stream (§5); the output length that of the
byte stream. A **part module**: the input and the output go through windows of 4 pages, and the
working memory is constant: a record buffer, a 16 KiB input buffer, the model (8 MiB), and the
state of a block (`aqr_ac3::BLK_BYTES`). The chunks are decoded one after the other in Wasm;
natively they are decoded at once on threads, with the same result. The stream is corrupt when a
count is cut, a record would pass the chunk's length, a frame fails to parse, or the lengths
do not add up.

## 7. The encoder *(informative)*

`formula::encode_with(stream, threads)` finds the records (§2), closes a chunk at the first record
that brings it to 16 MiB or more, codes the chunks on `threads` threads, and returns the coded
stream only if it comes to at most 97% of the stream. Chunking costs 0.2% on the audio of
`mari.avi` (87.56% against 87.37%).
