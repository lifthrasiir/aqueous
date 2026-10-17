# mp3-recons container format, version 0

This document specifies the files written by `mp3r c` / `mp3_recons::compress` and read by
`mp3r d` / `mp3_recons::decompress`, completely enough to write an independent decoder and a
compatible encoder. A decoder must reproduce the original input bit for bit. The encoder has
some freedom (section 12); everything the decoder does is normative.

The key words MUST, SHOULD and MAY are used as in RFC 2119.

## Contents

1. [Conventions](#1-conventions)
2. [Container](#2-container)
3. [Decoding overview](#3-decoding-overview)
4. [MPEG audio Layer III definitions](#4-mpeg-audio-layer-iii-definitions)
5. [File structure streams](#5-file-structure-streams)
6. [Binary range coder](#6-binary-range-coder)
7. [Side information model](#7-side-information-model)
8. [rANS coder and frequency tables](#8-rans-coder-and-frequency-tables)
9. [Coefficient model](#9-coefficient-model)
10. [Reconstruction](#10-reconstruction)
11. [Validation](#11-validation)
12. [Encoder](#12-encoder)
13. [Formula module](#13-formula-module)
- [Appendix A: tables](#appendix-a-tables)

## 1. Conventions

- **Integers** in the container are little endian unless stated otherwise. MPEG headers, CRCs
  and everything inside the MP3 bitstream are big endian, as in MPEG.
- **Bit strings** are MSB first: the first bit of a byte is its most significant bit. A bit
  string of `n` bits stored as bytes takes `ceil(n / 8)` bytes, padded with zero bits.
  Reading `k` bits as a number gives the first bit read the weight `2^(k-1)`. Reading 0 bits
  gives 0.
- **varint**: unsigned LEB128. Seven bits per byte, least significant group first; a byte
  below 0x80 ends the number. At most 10 bytes; bits beyond 64 are dropped. A longer
  sequence, or one cut off by the end of its data, is an error.
- `/` on integers truncates towards zero, `>>` is a logical shift on unsigned values,
  `ceil(a / b)` rounds up, `min`, `max` and `clamp(v, lo, hi)` are the usual functions,
  and `popcount` counts set bits.
- **xxh3** is XXH3-64 (`XXH3_64bits`, seed 0, default secret) from xxHash.
- **zstd** is a Zstandard stream (RFC 8878): one or more frames, decompressed and concatenated.

External references:

- ISO/IEC 11172-3 (MPEG-1 audio) and ISO/IEC 13818-3 (MPEG-2 audio, low sampling
  frequencies) define Layer III: header, side information and Huffman code tables. MPEG-2.5
  is the common extension of MPEG-2 to 8, 11.025 and 12 kHz. Section 4 restates what is
  used here, except the Huffman code tables (Table B.7 of ISO/IEC 11172-3).
- `model/builtin.bin` in this repository holds the frequency tables of the rANS coder
  (section 8.4). Version 0 is defined with exactly that file, whose SHA-256 is
  `42d9c7cd358dccbcf9e1715597197e82adf9a0cd86e876adee21067c325b9276`.

## 2. Container

| Offset | Size | Field |
|---|---|---|
| 0 | 6 | magic `69 04 7e 30 fd da` (`aQR+MP3a` when base64 encoded) |
| 6 | 2 | version, u16 LE: 0 |
| 8 | 1 | mode: 0 stored, 1 coded |
| 9 | | payload |

A decoder MUST reject a different magic, an unknown version or a mode other than 0 and 1.
A later version may change anything after the version field.

**Mode 0 (stored).** The payload is the original file, verbatim.

**Mode 1 (coded).**

| Field | Encoding |
|---|---|
| `len` | u64 LE: length of the original file in bytes |
| `hash` | u64 LE: xxh3 of the original file |
| byte streams 0..7 | per stream, see below |
| `tail_bits`, `rawgr_bits`, `gap_bits` | 3 varints |
| `n_words` | varint: number of rANS words |
| `raw_bits` | varint |
| rANS words | `n_words` × u32 LE |
| raw bit stream | all remaining bytes |

Each byte stream is a varint `v`, then

- if `v & 1 == 0`: `v >> 1` bytes, the stream itself;
- if `v & 1 == 1`: a varint `size`, then `v >> 1` bytes of zstd data that decompress to
  exactly `size` bytes (a different size is an error).

The eight byte streams, in this order:

| # | Name | Contents | Section |
|---|---|---|---|
| 0 | SEGS | junk lengths and frame runs | 5.1 |
| 1 | JUNK | the bytes that are not frames | 5.2 |
| 2 | HDR | frame headers | 5.3 |
| 3 | CRC | CRC flags and stored CRCs | 5.4 |
| 4 | TAIL | bit string: granule tails | 10.3 |
| 5 | RAWGR | bit string: granules stored verbatim | 10.3 |
| 6 | GAPS | bit string: reservoir bits outside placed frames | 10.2 |
| 7 | SIDE | range coded side information | 6, 7 |

TAIL, RAWGR and GAPS are bit strings packed into bytes (section 1). `tail_bits`,
`rawgr_bits` and `gap_bits` give their lengths in bits, and `raw_bits` the length in bits of
the raw bit stream. These four are informational: a decoder does not need them. The rANS
words are read by the rANS decoder (section 8), and the raw bit stream is read MSB first by
the coefficient model (section 9).

Decoding ends by checking that the output is `len` bytes long and that its xxh3 equals
`hash`. A decoder MUST NOT return output that fails this check.

## 3. Decoding overview

The original file is a sequence of *junk* (any bytes) and *runs* of MP3 frames. A frame
consists of a 4-byte header, a CRC-16 if the header says so, the side information
(9 to 32 bytes) and the *payload*: the rest of the frame.

The payloads of all frames, concatenated in file order across all runs, form the
*reservoir* `R`. In Layer III a frame's *main data* (scalefactors and Huffman coded
spectral values of its granules) starts `main_data_begin` bytes before the frame's own
payload in `R` and may extend into later payloads. A frame whose main data lies where
expected is *placed* (section 10.1). Its granules are coded through the coefficient model or
stored verbatim; all other bits of `R` are *gap* bits, stored verbatim.

To decode a coded container:

1. Read the header and the byte streams (section 2).
2. Parse SEGS and HDR into the list of frames, with their headers and lengths (section 5).
3. Run the *main pass* (section 10) over the frames in order. For each frame it decodes the
   side information from SIDE (sections 6, 7) and, for placed frames, rebuilds the frame's
   main data from the rANS and raw bit streams (sections 8, 9) or from RAWGR and TAIL. The
   result is the reservoir `R` and the complete side information of every frame.
4. Assemble the output from JUNK, the headers, CRCs, side information and `R` (section 10.4).
5. Check length and xxh3.

The rANS decoder, the raw bit reader, the range decoder and the bit readers of TAIL, RAWGR
and GAPS each have one position that only advances, over the whole file.

## 4. MPEG audio Layer III definitions

### 4.1 Header

The header is a big endian u32 `h`. Fields, by bit position (bit 0 the least significant):

| Field | Bits | Values |
|---|---|---|
| sync | 31..21 | all ones |
| version | 20..19 | 3 MPEG-1, 2 MPEG-2, 0 MPEG-2.5, 1 reserved |
| layer | 18..17 | 1 for Layer III |
| protection | 16 | 0: a CRC follows the header |
| bitrate_index | 15..12 | 0 free format, 15 invalid |
| sr_index | 11..10 | 3 invalid |
| padding | 9 | |
| private | 8 | |
| mode | 7..6 | 3: single channel |
| mode_ext | 5..4 | |
| copyright, original, emphasis | 3..0 | |

Derived values:

- `lsf` = version ≠ 3. `granules` = 1 if `lsf`, else 2. `channels` = 1 if mode = 3, else 2.
- `i_stereo` = mode = 1 and `mode_ext & 1`.
- A header is *valid* if sync is all ones, version ≠ 1, layer = 1, bitrate_index ≠ 15 and
  sr_index ≠ 3. Free format headers (bitrate_index 0) are valid.
- `sample_rate` = `[44100, 48000, 32000][sr_index] >> s`, with `s` = 0 for MPEG-1, 1 for
  MPEG-2 and 2 for MPEG-2.5.
- Frame length, not free format: `144000 × bitrate / sample_rate + padding` (MPEG-1) or
  `72000 × bitrate / sample_rate + padding` (otherwise), with `bitrate` in kbit/s from
  table A.1. Free format: `free + padding`, where `free` is given per run (section 5.1).
- `side_info_len` = 17 (MPEG-1, one channel), 32 (MPEG-1, two), 9 (LSF, one), 17 (LSF, two).
- `prefix_len` = 4 + (2 if protection = 0, else 0) + `side_info_len`. The payload is the
  frame without its first `prefix_len` bytes.
- `sfb_row`, the row of the scalefactor band tables (A.3):

  | | sr_index 0 | 1 | 2 |
  |---|---|---|---|
  | MPEG-1 | 5 (44.1 kHz) | 6 (48 kHz) | 7 (32 kHz) |
  | MPEG-2 | 2 (22.05 kHz) | 3 (24 kHz) | 4 (16 kHz) |
  | MPEG-2.5 | 0 (11.025 kHz) | 0 (12 kHz) | 1 (8 kHz) |

### 4.2 Side information

The side information is read and written with exactly this syntax (fields MSB first, widths
in bits):

```text
if not lsf:
    main_data_begin 9
    private_bits    5 if channels = 1 else 3
    scfsi[ch]       4    for ch in 0..channels
else:
    main_data_begin 8
    private_bits    channels
for gr in 0..granules, for ch in 0..channels, granule (gr, ch):
    part2_3_length      12
    big_values           9
    global_gain          8
    scalefac_compress    4 if not lsf else 9
    window_switching     1
    if window_switching:
        block_type       2
        mixed            1
        table_select[0]  5
        table_select[1]  5
        subblock_gain[s] 3   for s in 0..3
    else:
        table_select[t]  5   for t in 0..3
        region0_count    4
        region1_count    3
    preflag              1   (MPEG-1 only)
    scalefac_scale       1
    count1table          1
```

Fields that the syntax does not contain are 0 (for example `table_select[2]`,
`region0_count`, `region1_count` with window switching; `block_type`, `mixed`,
`subblock_gain` without). In LSF, `preflag` = (`scalefac_compress` ≥ 500). The total length
is `side_info_len` bytes.

A granule is *short* if `window_switching` = 1 and `block_type` = 2.

### 4.3 CRC

The CRC of a protected frame is CRC-16 with polynomial 0x8005, initial value 0xffff, MSB
first, no reflection and no final XOR, over header bytes 2 and 3 followed by the side
information bytes. It is stored big endian.

```text
crc = 0xffff
for each byte b:
    crc ^= b << 8
    repeat 8 times: crc = (crc << 1) ^ 0x8005 if crc & 0x8000 else crc << 1   (16 bits)
```

### 4.4 Scalefactor band widths

`widths(h, g)`, the band widths of a granule in coding order:

- not short: `SFB_LONG[sfb_row]` (22 entries, summing to 576);
- short, not mixed: `SFB_SHORT[sfb_row]` (39 entries: each short band three times, once per
  window);
- short and mixed: `SFB_MIXED[sfb_row]` (long bands up to line 36, then short bands; a 0
  entry ends the list).

The tables are in A.3. They are normative for this format even where they differ from a
particular MP3 decoder's (mixed blocks, MPEG-2.5 8 kHz short blocks): they decide region
boundaries and contexts, not the correctness of the round trip.

### 4.5 Scalefactor layout

`scf_layout(h, si, gr, ch)` gives four groups `k` = 0..3 of `count[k]` scalefactors of
`size[k]` bits each, in coding order, and `copy[k]`: whether the group is shared with granule
0 (MPEG-1 scfsi) and absent from the bitstream. Let `kind` = 0 if the granule is not short,
1 if short and mixed, 2 if short and not mixed, and `P = SCF_PARTITIONS[kind]` (A.4).

MPEG-1:

```text
p = SCFC[scalefac_compress]                 (A.4)
size  = [p >> 2, p >> 2, p & 3, p & 3]
count = P[0..4]
copy[k] = gr = 1 and kind = 0 and (scfsi[ch] & (8 >> k)) ≠ 0
```

LSF (`MOD` in A.4):

```text
ist = 1 if i_stereo and ch = 1 else 0
sfc = scalefac_compress >> ist
j = 12 × ist
while sfc ≥ 0:
    m = 1
    for i = 3 down to 0:
        size[i] = (sfc / m) mod MOD[j + i]
        m = m × MOD[j + i]
    sfc = sfc − m
    j = j + 4
count = P[j .. j + 4]
copy = [false; 4]
```

The *coded scalefactors* of a granule are those of the groups with `copy[k]` false, in
order, including groups of size 0 (whose values are 0 and take no bits). The bitstream holds
`size[k]` bits for each, MSB first. `scf_bits` = Σ over non-copied groups of
`size[k] × count[k]`.

`full_scalefactors(layout, coded, gr0)` gives all scalefactors in coding order (39 entries,
zero filled):

```text
i = 0; c = 0
for k in 0..4:
    for n in 0..count[k]:
        out[i] = gr0[i] if copy[k] else coded[c++]
        i = i + 1
```

### 4.6 Big values regions

`regions(h, g)` gives the number of pairs coded with `table_select[0..3]`:

```text
w = widths(h, g)
cnt = [8 if short and not mixed else 7, 255, 255] if window_switching
      else [region0_count, region1_count, 255]
L = 2 × big_values; line = 0; b = 0
for r in 0..3:
    start = line
    n = cnt[r] + 1
    while n > 0 and b < len(w) and w[b] ≠ 0 and line < L:
        line += w[b]; b += 1; n −= 1
    line = min(line, L)
    pairs[r] = (line − start) / 2
```

### 4.7 Huffman coding of part 3

Part 3 of a granule codes 576 quantized values: `big_values` pairs, then count1
quadruples, then zeros (the remaining bits of the granule, if any, are its *tail*).

A pair `(x, y)` with magnitudes `|x|`, `|y|` coded with table `ts` (1..31, not 4 or 14) is
written as:

```text
hcod_ts(min(|x|,15), min(|y|,15))
if LINBITS[ts] > 0 and |x| ≥ 15: |x| − 15 in LINBITS[ts] bits
if x ≠ 0: sign of x (1 = negative)
if LINBITS[ts] > 0 and |y| ≥ 15: |y| − 15 in LINBITS[ts] bits
if y ≠ 0: sign of y
```

`hcod_ts` is the code of table `ts` in ISO/IEC 11172-3 Table B.7; tables 16..23 share the
codes of table 16 and tables 24..31 those of table 24. `DIM[ts]` (A.5) is the number of
magnitudes per coordinate of table `ts`; `LINBITS` is in A.5. Table 0 codes no bits: all its
values are 0.

A quadruple `(v, w, x, y)` with values in {−1, 0, 1} is written as the count1 code of
`q = 8|v| + 4|w| + 2|x| + |y|` (table A or B per `count1table`, A.6), followed by the sign
bits of the nonzero values among v, w, x, y in that order.

## 5. File structure streams

### 5.1 SEGS

A sequence of varints: `junk[0]`, then zero or more times `n[k]`, `free[k]`, `junk[k+1]`.
It ends after a junk length that is the last byte of the stream. Run `k` has `n[k]` frames;
`free[k]` is 0 for a run that is not free format, else the free format frame length without
padding. The file is `junk[0]` bytes of junk, run 0, `junk[1]` bytes of junk, run 1, ...,
ending with junk. `free[k]` above 65536 is an error.

### 5.2 JUNK

The concatenation of all junk, in order.

### 5.3 HDR

Four bytes per frame, all runs in order. The header of a frame is the big endian u32 read
there, XOR the header of the previous frame (0 for the first frame of the file, across runs).
Each header MUST be valid, and free format exactly if its run has `free` > 0.

Frame `i` has length `flen[i]` (section 4.1), `flen[i]` ≥ `prefix_len`, payload length
`len[i] = flen[i] − prefix_len`, and payload offset `pay[i]` = Σ `len[j]` for `j < i`, its
byte offset in `R`. `r_len` = Σ `len[i]`.

### 5.4 CRC

For each frame with protection = 0, in order: one flag byte. A nonzero flag (encoders write
1) means the CRC equals the CRC of the reconstructed header and side information (4.3).
A flag of 0 is followed by the two CRC bytes as stored in the original.

## 6. Binary range coder

SIDE is coded with an adaptive binary range coder in the style of LZMA. Probabilities are
`u16` values `p` (probability of a 0 bit, in units of 1/2048), initially 1024.

### 6.1 Decoder

```text
init:   range = 0xffffffff; code = first 4 bytes of SIDE, big endian; pos = 4
next(): the byte at pos, or 0 if pos ≥ len(SIDE); pos += 1

bit(p):
    bound = (range >> 11) × p                  (32-bit)
    if code < bound: range = bound; p += (2048 − p) >> 5; b = 0
    else: code −= bound; range −= bound; p −= p >> 5; b = 1
    while range < 2^24: range <<= 8; code = (code << 8) | next()     (32-bit)
    return b
```

When the main pass is over, `pos` MUST be at most `len(SIDE) + 4`.

### 6.2 Composite values

`tree(P, n)` reads an `n`-bit value MSB first through `2^n` probabilities `P[1..2^n − 1]`:

```text
m = 1
repeat n times: m = 2m + bit(P[m])
return m − 2^n
```

`uint(U)` reads an unsigned value with probabilities `U.len[0..33]` and `U.bits[0..33][0..3]`:

```text
n = 0
while n < 32 and bit(U.len[n]) = 1: n += 1
if n ≤ 1: return n
r = 1
for i = n − 2 down to 0:
    r = 2r + bit(U.bits[n][min(n − 1 − i, 2)])
return r
```

`n` is the bit length of the value: a 1 bit per position while the value is longer, a 0 bit
after the last one, except after 32. Below the top bit, the first bit uses
`U.bits[n][1]` and the others `U.bits[n][2]`.

`sint(S)` reads a signed value with probabilities `S.zero`, `S.sign` and `S.mag` (a uint
set):

```text
if bit(S.zero) = 0: return 0
neg = bit(S.sign)
m = uint(S.mag) + 1
return −m if neg else m
```

### 6.3 Encoder

The encoder mirrors the decoder bit for bit (LZMA's range encoder with a carry cache):

```text
init: low = 0 (64-bit); range = 0xffffffff; cache = 0; cache_size = 1; out = []

bit(p, b):
    bound = (range >> 11) × p
    if b = 0: range = bound; p += (2048 − p) >> 5
    else: low += bound; range −= bound; p −= p >> 5
    while range < 2^24: range <<= 8; shift_low()

shift_low():
    if (low mod 2^32) < 0xff000000 or low ≥ 2^32:
        carry = low >> 32
        c = cache
        repeat: out.push((c + carry) mod 256); c = 0xff; cache_size −= 1; until cache_size = 0
        cache = (low >> 24) mod 256
    cache_size += 1
    low = (low mod 2^24) << 8

finish: shift_low() 5 times, then drop the first byte of out (always 0).
```

`tree`, `uint` and `sint` encode the same bits that their decoders read.

## 7. Side information model

All side information except `part2_3_length` of coded granules is range coded in SIDE,
with one model whose state persists over the whole file.

### 7.1 State

Probabilities (all initially 1024), with `[a][b]` denoting arrays:

| Name | Shape | Use |
|---|---|---|
| `placed` | 1 | frame placed |
| `mdb` | sint set | main_data_begin residual |
| `mdb_raw` | uint set | main_data_begin of unplaced frames |
| `private` | [6][32] | private bits, by their width |
| `scfsi` | [2][16][16] | by channel and previous scfsi |
| `raw` | 1 | granule stored verbatim |
| `ws` | [2][2] | window_switching, by channel and previous value |
| `block_type` | [4][4] | by previous block_type |
| `mixed` | 1 | |
| `big_values` | [2] sint sets | by channel |
| `table` | [96][32] | by region t and previous table_select[t]: index 32t + prev |
| `r0` | [24][16] | region0_count, by band |
| `r1` | [384][8] | region1_count, by band and region0_count: index 16 band + region0_count |
| `sbg` | [3][8] | subblock_gain, by window |
| `gain` | [2] sint sets | global_gain, by channel |
| `sfc1` | [16][16] | MPEG-1 scalefac_compress, by previous value |
| `sfc2` | [512] | LSF scalefac_compress |
| `preflag` | [2] | by previous value |
| `scale` | [2] | scalefac_scale, by previous value |
| `c1t` | [2] | count1table, by previous value |
| `nquads` | [2] sint sets | by channel |
| `tail` | uint set | |
| `p23` | uint set | part2_3_length of raw granules |

A uint set holds 33 + 33 × 3 probabilities, a sint set two probabilities and a uint set.

Previous values per channel `prev[ch]`, initially all 0 / false: `ws`, `block_type`,
`table[3]`, `gain`, `big_values`, `sfc`, `preflag`, `scale`, `c1t`, `nquads`, `scfsi`.
They carry over between runs, also when the MPEG version changes.

### 7.2 Frame fields

Once per frame, before its granules:

```text
placed = bit(placed)
if placed: resid = sint(mdb)
else:      main_data_begin = uint(mdb_raw) & 0x1ff
nb = channels if lsf else (5 if channels = 1 else 3)
private_bits = tree(private[nb], nb)
if not lsf:
    for ch in 0..channels:
        scfsi[ch] = tree(scfsi[ch][prev[ch].scfsi], 4)
        prev[ch].scfsi = scfsi[ch]
```

### 7.3 Granule fields

For granule (gr, ch), with `pv = prev[ch]` as it was before the granule:

```text
raw = bit(raw) if placed else true
window_switching = bit(ws[ch][pv.ws])
if window_switching:
    block_type = tree(block_type[pv.block_type], 2)
    mixed = bit(mixed)
    nt = 2
else:
    block_type = 0; mixed = false; nt = 3
big_values = (pv.big_values + sint(big_values[ch])) & 0x1ff
for t in 0..nt: table_select[t] = tree(table[32t + pv.table[t]], 5)
if window_switching:
    table_select[2] = 0; region0_count = 0; region1_count = 0
    for s in 0..3: subblock_gain[s] = tree(sbg[s], 3)
else:
    subblock_gain = [0, 0, 0]
    band = the number of leading entries of SFB_LONG[sfb_row] whose sum first reaches
           2 × big_values or more (0 if big_values = 0; 22 if never)
    region0_count = tree(r0[band], 4)
    region1_count = tree(r1[16 band + region0_count], 3)
global_gain = (pv.gain + sint(gain[ch])) mod 256
if lsf:
    scalefac_compress = tree(sfc2, 9); preflag = scalefac_compress ≥ 500
else:
    scalefac_compress = tree(sfc1[pv.sfc], 4); preflag = bit(preflag[pv.preflag])
scalefac_scale = bit(scale[pv.scale])
count1table = bit(c1t[pv.c1t])
if raw: part2_3_length = uint(p23) & 0xfff
prev[ch]: ws, block_type, table = table_select (all 3), gain = global_gain, big_values,
          sfc = scalefac_compress & 15, preflag, scale = scalefac_scale, c1t = count1table
```

`band` in pseudocode: `band = 0; line = 0; for w in SFB_LONG[sfb_row]: if line ≥ 2 ×
big_values: stop; line += w; band += 1`.

### 7.4 Part 3 fields

For a coded granule (placed frame, `raw` false), right after its granule fields:

```text
nquads = clamp(prev[ch].nquads + sint(nquads[ch]), 0, 144)
prev[ch].nquads = nquads
tail = uint(tail)
```

### 7.5 Order

SIDE holds, for each frame in file order: the frame fields, then for gr in 0..granules,
for ch in 0..channels: the granule fields, followed by the part 3 fields if the granule is
coded. The encoder codes the values that the decoder reads, with `resid`, `big_values`,
`gain` and `nquads` as differences: `resid` = main_data_begin − prediction (10.2),
`big_values − pv.big_values`, `global_gain − pv.gain`, `nquads − pv.nquads`.

## 8. rANS coder and frequency tables

Scalefactors and spectral values of coded granules are coded with static per-context
frequency tables through one rANS stream: eight interleaved 64-bit states, 12-bit
probabilities, 32-bit renormalisation.

### 8.1 Frequency tables and slots

Every context `c` (section 9.1) has an alphabet of `n` symbols (1 ≤ n ≤ 256) and
frequencies `f[0..n]`, each at least 1, summing to 4096. The 4096 *slots* of the context are
mapped to pairs (symbol, rank), rank in `0..f[s]`, through an alias table built as follows
(Vose's method; stacks are arrays used from the end):

```text
a = max(4, the smallest power of two ≥ n);  bs = 4096 / a     (bucket size)
rem[i] = f[i] for i < n, 0 for n ≤ i < a
cutoff[i] = bs, alias[i] = 0 for all i < a
small = [i in 0..a with rem[i] < bs], ascending;  large = [i with rem[i] > bs], ascending
while small and large are both nonempty:
    s = pop(small)                    (the last element)
    l = last(large)                   (not removed)
    cutoff[s] = rem[s]; alias[s] = l
    rem[l] −= bs − rem[s]
    if rem[l] < bs: pop(large); push(small, l)
    else if rem[l] = bs: pop(large)
next[i] = cutoff[i] for i < a
for i in 0..a in ascending order, if cutoff[i] < bs:
    rank_start[i] = next[alias[i]]
    next[alias[i]] += bs − cutoff[i]
```

Slot `z` lies in bucket `i = z / bs` at position `j = z mod bs`:

- if `j < cutoff[i]`: symbol `i`, rank `j`;
- else: symbol `alias[i]`, rank `rank_start[i] + j − cutoff[i]`.

Every symbol `s` gets exactly `f[s]` slots with ranks `0..f[s]`. `slot(s, r)` denotes the
inverse mapping.

### 8.2 Decoder

```text
w[k] = rANS word k, for k < n_words; 0 for k ≥ n_words
x[k] = w[2k] << 32 | w[2k + 1]   for k in 0..8
pos = 16; lane = 0

get(c):
    z = x[lane] & 4095
    (s, r) = symbol and rank of slot z in context c
    v = f_c[s] × (x[lane] >> 12) + r
    if v < 2^31: v = (v << 32) | w[pos]; pos += 1
    x[lane] = v
    lane = (lane + 1) mod 8
    return s
```

Symbol number `j` of the file (counting from 0 over the whole file) uses state `j mod 8`.
When the main pass is over, `pos` MUST be at most `n_words`.

### 8.3 Encoder

The encoder collects the sequence of (context, symbol) of the whole file and encodes it in
reverse:

```text
x[k] = 2^31 for k in 0..8; out = []          (words, in emission order)
for j = (number of symbols − 1) down to 0:
    (c, s) = symbol j; k = j mod 8; f = f_c[s]
    if x[k] ≥ f × 2^51: out.push(x[k] mod 2^32); x[k] = x[k] >> 32
    x[k] = ((x[k] / f) << 12) + slot(s, x[k] mod f)
words = [x[0] >> 32, x[0] mod 2^32, x[1] >> 32, ..., x[7] mod 2^32] ++ reverse(out)
```

With no symbols at all the words are the 16 words of eight states equal to 2^31.

### 8.4 Built-in tables

The frequencies of all 6537 contexts come from `model/builtin.bin`: a zstd stream that
decompresses to 1 540 828 bytes (SHA-256 of the decompressed bytes:
`bb28688b446550d0e2a88c971e07de2410a9c029b9ac979f5b329b8abf245189`):

- u32 LE layout id: `0xe6253d87`;
- for each context `c` = 0..6537 in order, its `n` frequencies as u16 LE.

The layout id is a 32-bit FNV-1a over the alphabet sizes as whole u32 values (`h =
0x811c9dc5`; per value `h = (h XOR n) × 0x01000193 mod 2^32`). It guards against tables
for a different context layout. Version 0 is defined with these tables; any change to them
or to the context layout needs a new version number.

## 9. Coefficient model

### 9.1 Contexts

| Contexts | Count | Alphabet | Codes |
|---|---|---|---|
| `PAIR(ts, B)` = 150 ts + B, ts 0..31, B 0..149 | 4800 | max(1, DIM[ts]²) | pair magnitudes |
| `ESC(ts, b)` = 4800 + 16 (ts − 16) + b, ts 16..31, b 0..15 | 256 | 28 | escape lengths |
| `QUAD(t, g, o)` = 5056 + 18 t + 3 g + o, t 0..1, g 0..5, o 0..2 | 36 | 16 | quadruples |
| `SCF(s, l, p)` = 5092 + 289 (s − 1) + 17 l + p, s 1..5, l, p 0..16 | 1445 | 2^s | scalefactors |

Contexts of tables 0, 4 and 14 exist (alphabet 1) but are never used.

### 9.2 Model state

Per channel `ch` (0, 1), persisting over the whole file and updated only by coded granules:

- `gr0_scf[ch][39]`: full scalefactors of the last coded granule 0 of the channel; 0.
- `exp[ch][22]`: band step exponents of the last coded granule of the channel; 0.
- `prev[ch][576]`: magnitudes of the last coded granule of the channel; 0.
- `prev_ok[ch]`: that granule exists and is not short; false.
- `scf_prev[ch][39]`: last coded value of each scalefactor position, capped at 15; 16
  (none).

and `cur0[576]`: magnitudes of the last coded granule of channel 0; 0.

The *magnitude* of a value is `min(|value|, 65535)` for big values (so 15 + escape for
escaped values) and 0 or 1 for quadruple values; lines past the coded values are 0.

### 9.3 Scalefactors

For a coded granule the coded scalefactors (4.5) come first, one symbol each, in order.
With `left = 16` and `i` the position in coding order, starting at 0:

```text
for k in 0..4:
    if copy[k]: i += count[k]; continue
    if size[k] = 0: count[k] values 0, no symbols; i += count[k]; continue
    repeat count[k] times:
        v = get(SCF(size[k], left, scf_prev[ch][min(i, 38)]))
        scf_prev[ch][min(i, 38)] = min(v, 15); left = min(v, 15); i += 1
```

### 9.4 Contexts of a granule

Before the values of a coded granule are decoded, from the state before the granule:

```text
short = window_switching and block_type = 2
use_prev = not short and prev_ok[ch]
full = full_scalefactors(layout, coded scalefactors, gr0_scf[ch])
if short: e[b] = 0 for b in 0..22
else: e[b] = global_gain − (4 if scalefac_scale else 2) × (full[b] + (PRETAB[b] if preflag else 0))
npairs = big_values
```

Neighbourhood bucket of pair `i` (lines 2i, 2i + 1), 24 meaning none:

```text
bucket[i] = 24 for all i
if use_prev:
    m(k) = prev[ch][k] for 0 ≤ k < 576, else 0
    line = 0; i = 0
    for b in 0..22 while i < npairs:
        fac = step_factor(e[b] − exp[ch][b])
        line += SFB_LONG[sfb_row][b]
        while i < min(line / 2, npairs):
            k = 2i
            s = 4 (m(k) + m(k+1)) + 2 (m(k−1) + m(k+2)) + m(k−2) + m(k+3)
            nb = min((s × fac) >> 9, 2^24)
            bucket[i] = LB((nb × 149797) >> 16)
            i += 1
```

`step_factor(d)`, in units of 1/256 (2^(−3d/16) × 256):

```text
t = 128 − 3 clamp(d, −80, 80)
q = floor(t / 16); r = t − 16 q
step_factor = POW16[r] << (q − 16) if q ≥ 16 else POW16[r] >> (16 − q)     (POW16 in A.7)
```

`LB(v)` = `v` if `v < 4`; else with `L = floor(log2 v)`: `min(2L + ((v >> (L − 1)) & 1), 23)`.

Other channel class, for `ch` = 1 only: `oc[i] = 1 + [o > 0] + [o > 2] + [o > 6] + [o > 14]`
with `o = cur0[2i] + cur0[2i+1]`; for `ch` = 0, `oc[i] = 0`.

The pair context index is `B[i] = 6 bucket[i] + oc[i]`.

### 9.5 Big values

For `t` in 0..3, `ts = table_select[t]`, the next `pairs[t]` pairs (4.6) at `line`
(starting at 0, advancing by 2 per pair):

- `ts` = 0: the values are 0; no symbols, no bits.
- otherwise, per pair `i = line / 2`, with `d = DIM[ts]`, `lb = LINBITS[ts]`:

```text
q = get(PAIR(ts, B[i])); x = q / d; y = q mod d       (magnitudes, up to 15)
ex = ey = 0
if lb > 0:
    for each of x, y (in that order) that equals 15:
        z = get(ESC(ts, min(B[i] / 6, 15))); n = z >> 1; top = z & 1
        e = 0 if n = 0; 1 if n = 1;
            else ((1 << (n−1)) | (top << (n−2)) | RAW(n − 2)) & (2^lb − 1)
        (ex or ey) = e
signs = RAW([x ≠ 0] + [y ≠ 0])        (sign of x in the higher bit, if both)
write the pair (4.7): magnitudes x + ex, y + ey with these signs
magnitudes: x + ex, y + ey
```

`RAW(k)` reads the next `k` bits of the raw bit stream (as zeros past its end). The escape
symbol is the bit length `n` of `e` = |value| − 15 and the bit below its top bit; the raw
bits are the `n − 2` low bits of `e`.

### 9.6 Quadruples

Then `nquads` quadruples (7.4), at `line` continuing from the big values:

```text
g = the number of k in 0..4 with prev[ch][line + k] ≠ 0 if use_prev, else 5
o = 0 if ch = 0, else 1 + [any of cur0[line .. line + 4] ≠ 0]
q = get(QUAD(count1table, g, o))           (bit 3: first value nonzero, ..., bit 0: fourth)
signs = RAW(popcount(q))                   (first nonzero value in the highest bit)
write the count1 code of q and the sign bits (4.7)
magnitudes of lines line..line+4: the bits of q; line += 4
```

### 9.7 State update

After a coded granule: if gr = 0, `gr0_scf[ch] = full`; `exp[ch] = e`; `prev[ch]` = its
magnitudes; `prev_ok[ch] = not short`; if ch = 0, `cur0` = its magnitudes.

### 9.8 Order

Per coded granule the rANS stream holds the scalefactor symbols, then per pair its pair
symbol and escape symbols (x before y), then the quadruple symbols. The raw bit stream holds
per pair the escape bits of x, of y, then the sign bits, and per quadruple its sign bits.

## 10. Reconstruction

### 10.1 Placement

With `end` the bit position in `R` where the main data of the previous placed frame ended
(0 at the start of the file), a frame with payload offset `pay` and length `len` is placed
if

```text
main_data_begin ≤ pay and start ≥ end and start + Σ part2_3_length ≤ (pay + len) × 8,
where start = (pay − main_data_begin) × 8
```

the sum running over all its granules.

### 10.2 Main pass

The output `R` is a bit string built front to back; `end` as above.

```text
end = 0
for each frame (header h, offset pay, length len), in file order:
    frame fields (7.2)
    if not placed:
        for gr, ch: granule fields (7.3) with placed = false
        if the frame is placed by 10.1: error
        continue
    main_data_begin = pay − ceil(end / 8) + resid
    require 0 ≤ main_data_begin ≤ (255 if lsf else 511), main_data_begin ≤ pay,
            (pay − main_data_begin) × 8 ≥ end
    start = (pay − main_data_begin) × 8
    append the next start − end bits of GAPS to R
    for gr in 0..granules, for ch in 0..channels:
        granule fields (7.3) with placed = true
        if raw: append the next part2_3_length bits of RAWGR; continue
        granule_start = |R|
        part 3 fields (7.4)
        layout = scf_layout(h, si, gr, ch)
        scalefactors (9.3); append each coded scalefactor in size[k] bits
        require big_values ≤ 288, 2 big_values + 4 nquads ≤ 576,
                no table_select[0..3] equal to 4 or 14
        contexts (9.4), big values (9.5), quadruples (9.6), appending their Huffman bits
        require tail ≤ 4095; append the next tail bits of TAIL
        part2_3_length = |R| − granule_start; require ≤ 4095
        state update (9.7)
    end = |R|; require end ≤ (pay + len) × 8
range decoder in bounds (6.1), rANS decoder in bounds (8.2)
append the next r_len × 8 − end bits of GAPS
```

Unplaced frames leave `end` unchanged and add nothing to `R` themselves: their payload bits
are gap bits. Granule fields (7.3) update `prev[ch]` for raw granules too, while the model
state of 9.2 changes only with coded granules.

The scalefactors use `scfsi` of the current frame and `gr0_scf` (4.5, 9.4) for copied groups.

### 10.3 Bit streams

GAPS, RAWGR and TAIL are read front to back, each from its own position, across the whole
file. Bits past their end read as 0 (an encoder never produces such files).

### 10.4 Assembly

```text
output = []
for k in 0..number of runs + 1:
    append the next junk[k] bytes of JUNK
    if k is a run: for each of its frames:
        append the header (4 bytes, big endian)
        side = the side information written per 4.2 (side_info_len bytes)
        if protection = 0:
            flag = next CRC byte
            append CRC(header bytes 2..4, side) if flag ≠ 0 else the next 2 CRC bytes
        append side
        append R[pay .. pay + len] (bytes)
```

## 11. Validation

A decoder MUST fail on an error named in the sections above, and MUST NOT return output
that fails the length and xxh3 check (section 2). The reference decoder also rejects, and a
decoder SHOULD reject:

- a frame count Σ `n[k]` above `len(HDR) / 4`;
- a frame with `flen < prefix_len`, or with `r + flen > len` where `r` is the payload length
  of the frames before it (a sanity bound);
- a zstd stream declaring a `size` above `len + 2^20`;
- `n_words × 4` larger than what remains of the container.

Any input, including a corrupted container, MUST result in an error or the original output,
never in undefined behaviour; sizes read from the container SHOULD be bounded before
allocating.

## 12. Encoder

An encoder MAY write any file that decodes to its input as specified above. This section
describes the reference encoder; its choices are not normative except where stated.

### 12.1 Stored fallback

The reference encoder writes mode 0 if coding fails for any reason, or if the coded
container would be at least as long as the input plus 9 bytes.

### 12.2 Segmentation

The decoder accepts any split into junk and runs whose frames satisfy 5.3 and fit in the
file. The reference encoder:

- treats an ID3v2 tag at offset 0 (`"ID3"`, size bytes 6..10 all below 0x80) as junk as a
  whole: 10 + the syncsafe size + 10 more if flag bit 4 (footer) of byte 5 is set;
- scans for 0xff bytes; a run starts at `p` if the header there is valid and the frame fits
  in the file, and the next two frames follow it directly with the same version and
  sr_index, or the file ends before them;
- for a free format header, looks for the next valid free format header with the same
  version and sr_index within 4096 bytes; the distance minus padding is the candidate `free`,
  accepted if at least `prefix_len` and the run condition above holds with it;
- extends a run while the next frame is valid, of the same free format length, fits in the
  file, and has the version and sr_index of the run's first frame.

### 12.3 Frames

Per frame, in order: the header XOR the previous one into HDR; the CRC flag (1 if the
stored CRC equals the computed one, else 0 and the stored bytes); the side information as
parsed by 4.2; its payload appended to `R`.

### 12.4 Main data

For each frame the reference encoder computes placement by 10.1 (this is normative: the
decoder rejects an unplaced frame that would be placed, and a placed frame codes
main_data_begin through the prediction), then:

- *unplaced*: frame and granule fields with `raw` true; the frame's main data stays in `R`
  and becomes gap bits.
- *placed*: the bits of `R` from `end` to `start` go to GAPS. Each granule is coded if it
  decodes: `scf_bits ≤ part2_3_length`, `big_values ≤ 288`, no table_select of 4 or 14, and
  the big values end within the granule. Quadruples are then decoded greedily while the
  next one (code and sign bits) ends within the granule and stays below line 576; what is
  left is the tail, copied to TAIL. Otherwise the granule is raw: its `part2_3_length` bits
  go to RAWGR and nothing of it enters the rANS or raw bit streams or the model state.
  Re-encoding a decoded granule reproduces its bits exactly because the Huffman codes are
  complete prefix codes.
- After the last frame, the bits of `R` from `end` to its end go to GAPS.

The encoder MUST ensure that every requirement of 10.2 holds for what it writes.

### 12.5 Byte streams

The reference encoder compresses a byte stream with zstd (level 1) if it is longer than 16
bytes and the compressed data is strictly shorter; otherwise it stores it plain.

## 13. Formula module

The coded form can also be given as an application of a module of entry formulas
(recons-formula), without the container around it: `mp3_recons::formula::encode` gives the
formula, `formula::decode` is the module.

- **Module**: name `mp3-recons`, version 1. Version 1 uses the format of this document
  (version 0) and `model/builtin.bin`, which stays compiled in.
- **Params**: none (empty). A decoder MUST reject non-empty params.
- **Output length**: the length `len` of the application is the length of the original file.
  The decoder MUST fail if its output is not `len` bytes.
- **Inputs**: exactly ten slots, each stored raw (the archive compresses them):

| Slot | Class | Contents | Container field |
|---|---|---|---|
| 0 | `mp3/segs` | SEGS (5.1) | byte stream 0 |
| 1 | `mp3/junk` | JUNK (5.2) | byte stream 1 |
| 2 | `mp3/headers` | HDR (5.3) | byte stream 2 |
| 3 | `mp3/crc` | CRC (5.4) | byte stream 3 |
| 4 | `mp3/tailbits` | TAIL bit string | byte stream 4 |
| 5 | `mp3/rawgr` | RAWGR bit string | byte stream 5 |
| 6 | `mp3/gaps` | GAPS bit string | byte stream 6 |
| 7 | `mp3/side` | SIDE, range coded | byte stream 7 |
| 8 | `mp3/rans` | rANS words, u32 LE each; the length is a multiple of 4 | rANS words |
| 9 | `mp3/rawbits` | raw bit stream | raw bit stream |

Decoding is sections 3 to 10 on these streams. Not carried over from the container: magic,
version and mode; `hash` (the check of section 11 is left to the evaluator); `len` (the
application's length); the zstd layer of the byte streams (slots are plain); `tail_bits`,
`rawgr_bits`, `gap_bits` and `raw_bits` (informational, section 2); `n_words` (the length of
slot 8 divided by 4). Slot 9 is read as in the container, where it is all remaining bytes.

The encoder gives no formula (the container would be stored, 12.1) where the coded
container, with its zstd layer as in 12.5, would be at least as long as the input plus 9
bytes, or if coding fails.

## Appendix A: tables

### A.1 Bitrates (kbit/s) by bitrate_index 0..14

```text
MPEG-1:          0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320
MPEG-2, 2.5:     0,  8, 16, 24, 32, 40, 48, 56,  64,  80,  96, 112, 128, 144, 160
```

### A.2 Pre-emphasis

```text
PRETAB = 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 3, 2, 0
```

### A.3 Scalefactor band widths, by sfb_row

```text
SFB_LONG:
0: 6 6 6 6 6 6 8 10 12 14 16 20 24 28 32 38 46 52 60 68 58 54
1: 12 12 12 12 12 12 16 20 24 28 32 40 48 56 64 76 90 2 2 2 2 2
2: 6 6 6 6 6 6 8 10 12 14 16 20 24 28 32 38 46 52 60 68 58 54
3: 6 6 6 6 6 6 8 10 12 14 16 18 22 26 32 38 46 54 62 70 76 36
4: 6 6 6 6 6 6 8 10 12 14 16 20 24 28 32 38 46 52 60 68 58 54
5: 4 4 4 4 4 4 6 6 8 8 10 12 16 20 24 28 34 42 50 54 76 158
6: 4 4 4 4 4 4 6 6 6 8 10 12 16 18 22 28 34 40 46 54 54 192
7: 4 4 4 4 4 4 6 6 8 10 12 16 20 24 30 38 46 56 68 84 102 26

SFB_SHORT (coding order):
0: 4 4 4 4 4 4 4 4 4 6 6 6 8 8 8 10 10 10 12 12 12 14 14 14 18 18 18 24 24 24 30 30 30 40 40 40 18 18 18
1: 8 8 8 8 8 8 8 8 8 12 12 12 16 16 16 20 20 20 24 24 24 28 28 28 36 36 36 2 2 2 2 2 2 2 2 2 26 26 26
2: 4 4 4 4 4 4 4 4 4 6 6 6 6 6 6 8 8 8 10 10 10 14 14 14 18 18 18 26 26 26 32 32 32 42 42 42 18 18 18
3: 4 4 4 4 4 4 4 4 4 6 6 6 8 8 8 10 10 10 12 12 12 14 14 14 18 18 18 24 24 24 32 32 32 44 44 44 12 12 12
4: 4 4 4 4 4 4 4 4 4 6 6 6 8 8 8 10 10 10 12 12 12 14 14 14 18 18 18 24 24 24 30 30 30 40 40 40 18 18 18
5: 4 4 4 4 4 4 4 4 4 4 4 4 6 6 6 8 8 8 10 10 10 12 12 12 14 14 14 18 18 18 22 22 22 30 30 30 56 56 56
6: 4 4 4 4 4 4 4 4 4 4 4 4 6 6 6 6 6 6 10 10 10 12 12 12 14 14 14 16 16 16 20 20 20 26 26 26 66 66 66
7: 4 4 4 4 4 4 4 4 4 4 4 4 6 6 6 8 8 8 12 12 12 16 16 16 20 20 20 26 26 26 34 34 34 42 42 42 12 12 12

SFB_MIXED (coding order, 0 ends the list):
0: 6 6 6 6 6 6 6 6 6 8 8 8 10 10 10 12 12 12 14 14 14 18 18 18 24 24 24 30 30 30 40 40 40 18 18 18 0 0 0
1: 12 12 12 4 4 4 8 8 8 12 12 12 16 16 16 20 20 20 24 24 24 28 28 28 36 36 36 2 2 2 2 2 2 2 2 2 26 26 26
2: 6 6 6 6 6 6 6 6 6 6 6 6 8 8 8 10 10 10 14 14 14 18 18 18 26 26 26 32 32 32 42 42 42 18 18 18 0 0 0
3: 6 6 6 6 6 6 6 6 6 8 8 8 10 10 10 12 12 12 14 14 14 18 18 18 24 24 24 32 32 32 44 44 44 12 12 12 0 0 0
4: 6 6 6 6 6 6 6 6 6 8 8 8 10 10 10 12 12 12 14 14 14 18 18 18 24 24 24 30 30 30 40 40 40 18 18 18 0 0 0
5: 4 4 4 4 4 4 6 6 4 4 4 6 6 6 8 8 8 10 10 10 12 12 12 14 14 14 18 18 18 22 22 22 30 30 30 56 56 56 0
6: 4 4 4 4 4 4 6 6 4 4 4 6 6 6 6 6 6 10 10 10 12 12 12 14 14 14 16 16 16 20 20 20 26 26 26 66 66 66 0
7: 4 4 4 4 4 4 6 6 4 4 4 6 6 6 8 8 8 12 12 12 16 16 16 20 20 20 26 26 26 34 34 34 42 42 42 12 12 12 0
```

### A.4 Scalefactor layouts

```text
SCFC (MPEG-1, slen1 << 2 | slen2 by scalefac_compress):
    0, 1, 2, 3, 12, 5, 6, 7, 9, 10, 11, 13, 14, 15, 18, 19

SCF_PARTITIONS[kind] (kind 0 long, 1 mixed, 2 short), 28 entries each: 0..4 MPEG-1,
4..16 LSF in three blocks of four, 16..28 LSF intensity stereo (channel 1) in three blocks:
    0: 6 5 5 5  6 5 5 5  6 5 7 3  11 10 0 0  7 7 7 0  6 6 6 3  8 8 5 0
    1: 8 9 6 12  6 9 9 9  6 9 12 6  15 18 0 0  6 15 12 0  6 12 9 6  6 18 9 0
    2: 9 9 6 12  9 9 9 9  9 9 12 6  18 18 0 0  12 12 12 0  12 9 9 6  15 12 9 0

MOD (LSF), 24 entries:
    5 5 4 4  5 5 4 1  4 3 1 1  5 6 6 1  4 4 4 1  4 3 1 1
```

### A.5 Pair tables

```text
ts:       0  1  2  3  4  5  6  7  8  9 10 11 12 13 14 15 16..23  24..31
DIM:      0  2  3  3  0  4  4  6  6  6  8  8  8 16  0 16   16      16
LINBITS:  0 for ts < 16;  16..23: 1 2 3 4 6 8 10 13;  24..31: 4 5 6 7 8 9 11 13
```

### A.6 Count1 tables

Table A (`count1table` = 0), code by `q = 8|v| + 4|w| + 2|x| + |y|`:

```text
q:    0     1     2     3      4     5       6      7       8     9      10     11
code: 1     0101  0100  00101  0110  000101  00100  000100  0111  00011  00110  000000
q:    12     13      14      15
code: 00111  000010  000011  000001
```

Table B (`count1table` = 1): the 4 bits of `15 − q`.

### A.7 POW16

2^(r/16) in units of 1/65536, r = 0..15:

```text
65536, 68438, 71468, 74632, 77936, 81386, 84990, 88752,
92682, 96785, 101070, 105545, 110218, 115098, 120194, 125515
```
