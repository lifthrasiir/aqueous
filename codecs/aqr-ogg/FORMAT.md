# aqr-ogg format

This document specifies the module `ogg-recons` of aqr-formula entry formulas, which rebuilds an
Ogg file (its pages one after another) from its logical streams' packets and a **layout record**
of its pages. The lacing values of the pages come from the packets, the CRCs from the pages, and
the header types, sequence numbers and granule positions are stored as they differ from what a
walk over the pages predicts.

The document is normative and complete: the module can be implemented from it alone. Sections
marked *informative* describe this implementation's encoder; a decoder needs none of them.

External references:

- RFC 3533 (the Ogg encapsulation format), for the page and its fields.
- The Vorbis I specification (Xiph.Org) §4.2.2 (the identification header) and §4.3.1 (the
  start of an audio packet), and RFC 6716 §3.1 (an Opus packet's TOC byte) with RFC 7845
  (Ogg Opus), for the predictions of granule positions (§5.2); they change no output, only how
  much the layout holds.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [The packets](#3-the-packets)
4. [The layout record](#4-the-layout-record)
5. [The walk](#5-the-walk)
6. [Encoding](#6-encoding-informative) *(informative)*

## 1. Conventions

- Integers are exact; `a / b` is the quotient rounded down. Arithmetic on sequence numbers is
  modulo 2^32 and on granule positions modulo 2^64 (*wrapping*). `−1` as a granule position is
  2^64 − 1.
- **varint**: unsigned LEB128, least significant group first, at most 10 bytes.
  `zz(d)` of a signed `d` is `2d` for `d ≥ 0`, `−2d − 1` for `d < 0`; **zvarint** is the
  varint of `zz`.
- Little-endian fields: u16 and u32 of 2 and 4 bytes, least significant byte first.
- A **page** (RFC 3533 §6) is: `OggS`; version 0; header type; granule position (u64); serial
  number (u32); sequence number (u32); CRC (u32); `n`, the lacing values' count (a byte); `n`
  lacing values (bytes); the body, as many bytes as the lacing values' sum. Its CRC is that of
  the whole page with the CRC field taken as zeros: polynomial 0x04c11db7, most significant bit
  first, initial value 0, no final XOR. Header type flags: 1 *continued*, 2 *first* (BOS), 4
  *last* (EOS).

## 2. The module

- **Module**: name `ogg-recons`, version 1.
- **Params**: none (empty).
- **Resources**: none.
- **Inputs**: exactly 2: `layout` (class `ogg/layout`), the layout record (§4); `packets`, the
  streams' packets (§3).
- **Output**: the pages, `len` bytes (the declared length).

The decoder reads the layout, cuts the packets input into the streams' packets, and makes each
page in the layout's order by the walk (§5), writing it with its CRC. It fails where:

- the layout does not read as §4 has it (ending early, a value out of range, bytes after it);
- the packets input does not cut into the streams' bytes exactly, or a stream's bytes do not
  read as packets (§3), or a stream left open has no packets or its last is not a multiple of
  255 bytes, more than none;
- a stream has no page, or a page takes more lacing values than its stream has left, or a
  stream's lacing values are not all taken by its pages;
- a stream of the Vorbis predictor has no first packet that is an identification header (§5.2);
- the pages' bytes pass `len`, or end short of it.

## 3. The packets

The packets input is the streams' packets, the streams in the layout's order, each stream's
packets in order, each **framed**: while 65535 bytes or more of it are left, the u16 65535 and
65535 bytes (a **chunk**); then the u16 `r` (below 65535, perhaps 0) and its `r` bytes left. A
stream's bytes (as the layout gives them) must be whole packets framed.

A stream's **lacing values** are, for each packet of `m` bytes, `m / 255` values 255, then the
value `m mod 255`; but for the last packet of a stream the layout gives as **open**, the values
255 alone (its packet ends on no page). Its bytes are those of its packets one after another.

## 4. The layout record

```
varint S              streams (1 to len / 27)
S × stream:
  u32 serial          the serial number of its pages
  varint bytes        its packets framed, in the packets input
  byte flags          bit 0: open (§3); bits 1–7: its predictor, 0 none, 1 Vorbis, 2 Opus
  (Vorbis) varint M   its modes (1 to 64), then (M + 7) / 8 bytes of their block flags:
                      mode i at bit (i mod 8) of byte i / 8, the bits past M zero
varint N              pages (at most len / 27)
N × varint stream     each page's stream (below S)
N × byte segments     each page's lacing values' count
N × byte type         each page's header type XOR its prediction
N × zvarint seq       each page's sequence number less its prediction, as a signed 32-bit
                      difference (−2^31 to 2^31 − 1)
N × zvarint granule   each page's granule position less its prediction, as a signed 64-bit
                      difference
```

The record ends there.

## 5. The walk

### 5.1 Pages

Each stream keeps, from its start: the lacing values and bytes taken (none), the packets ended
(none), its **position** (0), its last sequence number (none), whether its last page ended in a
packet (no), and its pages left (all of the layout's pages of it).

Page `k` of the layout, of stream `s` and `segments` = `c`:

1. It takes the next `c` lacing values of `s`, and the next bytes of their sum: its lacing values
   and body.
2. For each of its lacing values below 255 in turn, a packet of `s` ends: the position grows by
   that packet's **duration** (§5.2).
3. Its predicted header type: *continued* where the last page of `s` ended in a packet, *first*
   where it is the first page of `s`, *last* where it is the last (its pages left then none).
   Its header type is that XOR the layout's.
4. Its predicted sequence number: 0 for the first page of `s`, else the last one plus 1. Its
   sequence number is that plus the layout's (wrapping).
5. Its predicted granule position: where a packet ended on it, the position; else −1. Its
   granule position `g` is that plus the layout's (wrapping). Where a packet ended on it and `g`
   is not −1, the position becomes `g`.
6. The page ends in a packet where its last lacing value is 255 (a page of no lacing values
   leaves that as it was).

The page is written with version 0, the serial number of `s`, and its CRC.

### 5.2 Durations

A packet's duration is a whole number from the stream's predictor; all are 0 for none.

**Vorbis.** The stream's first packet must be of 30 bytes at least, starting with the byte 1
and `vorbis`; its byte 28 holds the block sizes' exponents, the short one in its low 4 bits and
the long one in its high 4, each from 6 to 13 and the short not above the long. With `b = ilog(M
− 1)` bits for a mode (0 for `M = 1`): a packet of no bytes, or whose first byte has bit 0 set
(a header), or whose mode (the `b` bits of its first byte from bit 1, least significant first)
is `M` or more, lasts 0 and changes nothing. Else its block size `n` is the long one where its
mode's block flag is set, else the short one; it lasts `prev / 4 + n / 4` where an earlier
packet of the stream had a block size `prev` (the last such), else 0; and its `n` becomes `prev`.

**Opus.** The first two packets last 0, and so does a packet of no bytes. Else, of its first
byte `t` (the TOC), with `config = t / 8`: its frames last, in samples at 48 kHz, for `config`
0 to 11 480, 960, 1920 or 2880 by `config mod 4`; for 12 to 15 480 or 960 by `config mod 2`;
for 16 to 31 120, 240, 480 or 960 by `config mod 4`. Its frames are 1 for `t mod 4 = 0`, 2 for 1
or 2, and for 3 its second byte mod 64 (0 where it has none). It lasts the frames' count times a
frame's samples.

## 6. Encoding *(informative)*

`aqr_ogg::formula::encode` takes the pages one after another from the start of the data, each
whole with its CRC right, up to the first place where none is; what follows is kept as it is. A
page with the first-page flag, or of a serial number no stream has, starts a stream; any other
page is of the last stream of its serial number. A stream's packets are its pages' bodies cut by
their lacing values, the last left open where the stream's last lacing value is 255.

A stream is Vorbis where its first packet is an identification header and its third a setup
header that aqr-vorbis parses (the predictor's block flags are the setup's modes'), Opus where
its first packet starts with `OpusHead`. The packets of a Vorbis stream are given as: its first
two packets framed and the u16 length of its setup header (class `ogg/headers`), the setup
header (class `vorbis/setup`, the same stream that aqr-vorbis's applications take), then its
audio packets framed, in pieces: one starts at the first packet at or past each multiple of 2
MiB of them, so that the pieces are the same whatever the threads. Each piece is an application
of aqr-vorbis's `vorbis-recons` as far as aqr-vorbis takes it, the rest as it is (class
`ogg/packets`); so are packets from one of a chunk or more on. The packets of other streams are
framed as they are (class `ogg/packets`). A file none of whose streams aqr-vorbis takes is not
taken: its layout once compressed is no smaller than its pages' headers, and its packets framed
are larger than as laced.

Measured on an M4 (`aqr`, 10 threads) on 12 Ogg Vorbis files of 0.45 to 9.0 MB made from the
FLAC corpus's live recordings, 8 by libvorbisenc and libogg (quality 1 to 8) and 4 by ffmpeg's
muxer (quality 0 to 5), 49.14 MB: 45.78 MB archived (0.932; zstd -3 0.999), compressed in 0.90 s
and tested at 158 MB/s natively and 117 MB/s as Wasm (`aqr -t --wasm`). The layout is about
5 bytes a page before the archive compresses it, a page of libogg's being about 4 KB; every
page's header type and sequence number are as predicted, and so is every granule position but
the last page's of 10 of the 12 streams (cut short at the end of the samples). Pieces of 1 MiB made the records 0.15% of the files larger than the
streams whole, of 2 MiB 0.05%, of 4 MiB 0.02%; aqr-vorbis's encoder holds about 35 bytes a byte
of a piece.
