# The `flr` container format

This document specifies the files written by `flr c` (`flac_recons::compress`). It is meant
to be complete: a decoder, and an encoder whose output a decoder accepts, can be written
from it alone. Sections marked *(encoder)* describe the choices this implementation makes.
A decoder does not need them, but an encoder must follow them to produce the same bytes as
this implementation.

Contents:

1. [Conventions](#1-conventions)
2. [Header](#2-header)
3. [Version 0: stored](#3-version-0-stored)
4. [Version 1: coded](#4-version-1-coded)
5. [Frames](#5-frames)
6. [The residual model](#6-the-residual-model)
7. [rANS](#7-rans)
8. [FLAC streams](#8-flac-streams)
9. [PCM streams](#9-pcm-streams)
10. [Choosing the kind and the version](#10-choosing-the-kind-and-the-version-encoder) *(encoder)*
11. [Validation](#11-validation)
12. [Appendix A: built-in frequency tables](#appendix-a-built-in-frequency-tables)
13. [Appendix B: CRCs](#appendix-b-crcs)

## 1. Conventions

- Integers are unsigned unless they are marked signed. Signed integers are two's complement.
  `>>` on a signed value is an arithmetic shift. Fixed-width arithmetic wraps unless
  stated otherwise.
- `u16le`, `u32le` and `u64le` are little-endian integers.
- **varint**: unsigned LEB128. Each byte holds 7 bits, least significant group first, and
  bit 7 is set on every byte except the last. A reader takes at most 10 bytes and drops
  bits beyond 64. If the 10th byte still has bit 7 set, the input is malformed.
- **zigzag** maps a signed value to an unsigned one: `zz(v) = (v << 1) ^ (v >> (W-1))`,
  `unzz(u) = (u >> 1) ^ -(u & 1)`, where W is the width. Byte streams use 64-bit zigzag
  (`zz64`). The residual model uses 32-bit zigzag (`zz32`).
- **Bit streams** are MSB-first: the first bit is bit 7 of byte 0. Writing an `n`-bit value
  puts its most significant bit first. A finished stream is padded with zero bits to a
  whole byte.
- `bitlen(v)` is the number of significant bits of `v > 0` (`bitlen(1) = 1`).
  `floor_log2(v) = bitlen(v) - 1`.
- **zstd** means one Zstandard frame (RFC 8878). **xxh3** means XXH3-64 with seed 0 and the
  default secret.

## 2. Header

Every file starts with an 8-byte header:

| Offset | Size | Field |
|---|---|---|
| 0 | 6 | magic: the kind of the original file |
| 6 | 2 | version, `u16le` |

| Magic (hex) | Base64 | Kind | Original file |
|---|---|---|---|
| `69 04 7e 7f 56 9c` | `aQR+f1ac` | FLAC | A native FLAC stream |
| `69 04 7e c1 ab f7` | `aQR+wav3` | WAV | WAV (RIFF), AIFF or AIFC: PCM samples with a container header |
| `69 04 7e a5 c9 bf` | `aQR+pcm/` | PCM | Headerless PCM samples of a given format, or anything not recognised |

| Version | Meaning |
|---|---|
| 0 | Stored: the original follows verbatim ([§3](#3-version-0-stored)) |
| 1 | Coded ([§4](#4-version-1-coded)). Under the FLAC magic the payload holds FLAC streams ([§8](#8-flac-streams)); under the WAV and PCM magics it holds PCM streams ([§9](#9-pcm-streams)) |

Any other magic or version is an error. Version 0 and version 1 are both valid under all
three magics. Under the WAV and PCM magics the payloads are identical, so the magic only
records where the samples came from. The PCM streams themselves describe where the samples
sit in the file (§9).

## 3. Version 0: stored

```text
header (8 bytes) | original bytes
```

The original is everything after the header. The original may be empty.

## 4. Version 1: coded

```text
header (8 bytes)
u64le  len          length of the original in bytes
u64le  hash         xxh3 of the original
stream × 6          byte streams 0..5, each:
                      varint  v = n << 1 | z
                      if z = 1:  varint raw_len, then n bytes: a zstd frame that decompresses to raw_len bytes
                      if z = 0:  n bytes: the stream itself
varint W            number of rANS words
u32le × W           rANS words (§7)
varint R0           length of raw bit stream 0 in bytes
R0 bytes            raw bit stream 0
rest of the file    raw bit stream 1
```

After decoding, the original's length must equal `len` and its xxh3 must equal `hash`.
Anything else is an error.

The six byte streams have the same slots in both payload types:

| # | FLAC (§8) | PCM (§9) |
|---|---|---|
| 0 | prefix: bytes before the first frame | prefix: bytes before the sample data |
| 1 | layout: runs of frames and junk | layout: sample format and sample frame count |
| 2 | junk: bytes between frames | junk: bytes after the sample data |
| 3 | frame headers | channel assignment per block |
| 4 | subframe parameters | subframe parameters |
| 5 | warm-up, constant and verbatim samples | warm-up, constant and verbatim samples |

*(encoder)* A stream is stored zstd'd (`z = 1`) when it is longer than 16 bytes and its
zstd frame is shorter than the stream itself. Otherwise it is stored as is. This
implementation compresses with libzstd's one-shot API (`ZSTD_compress`) at level 3. Its
frames record the content size and carry no checksum. Different zstd versions can produce
different bytes. Any valid zstd frame decodes the same way.

## 5. Frames

Both payload types describe audio as a sequence of **frames**, using FLAC's model of a frame
([RFC 9639](https://www.rfc-editor.org/rfc/rfc9639)):

- A frame has a block size `N` (samples per channel), a channel assignment `assign`
  (0..10) and a sample size `bps`. The number of channels is `assign + 1` when
  `assign < 8`. Otherwise it is 2: 8 is left/side, 9 is side/right, 10 is mid/side.
- The **side** channel is channel 1 for assignments 8 and 10, and channel 0 for assignment 9.
  A subframe's full sample size is `bps + 1` on the side channel and `bps` otherwise.
- Each channel has a subframe:

  | `typ` (FLAC's 6-bit type) | Subframe | Order |
  |---|---|---|
  | 0 | constant | 0 |
  | 1 | verbatim | 0 |
  | 8..12 | fixed predictor | `typ - 8` (0..4) |
  | 32..63 | LPC | `typ - 31` (1..32) |

  Other types are invalid. A subframe has `wasted` bits (0 if none), and its coded sample
  size is `bits = full - wasted`, which must be at least 1. Fixed and LPC subframes have
  `order` warm-up samples and `N - order` **residuals**, each fitting in a signed 32-bit
  integer. LPC subframes also have a coefficient precision (1..15), a shift (-16..15) and
  `order` coefficients of `precision` bits.
- Subframes with residuals (`typ >= 8`) are the **residual channels** of the frame.

The residuals of all frames go through the residual model (§6) into the rANS words and
the two raw bit streams. Everything else goes into the byte streams.

### 5.1 Subframe parameters (byte streams 4 and 5)

The parameters of a frame's subframes are stored in channel order. Stream 4 (`sub`) and
stream 5 (`warm`) are read in parallel:

```text
for each channel ch in 0..channels:
  sub:  u8  x = typ << 1 | (wasted > 0)
  if x & 1:  sub: u8 wasted            (must be ≥ 1)
  bits = full(ch) - wasted
  match typ:
    0 (constant):   warm: varint zz64(value)
    1 (verbatim):   warm: N × varint zz64(sample)
    fixed / LPC:    warm: order × varint zz64(warm-up sample)
                    if LPC:
                      sub: u8 precision (1..15)
                      sub: u8 shift (as i8, -16..15)
                      sub: order × varint zz64(coefficient)
                    if FLAC payload:
                      sub: u8 method << 4 | partition_order     (method 0 or 1, partition_order 0..15)
                      sub: 2^partition_order × u8 param
```

All samples (constant value, verbatim samples, warm-up samples) are the subframe's coded
values: they come after the wasted bits are removed, so each fits in `bits` signed bits.
Coefficients fit in `precision` signed bits.

`param` (FLAC payload only) gives the Rice parameter `k` of each residual partition.
`k < 15` with method 0, or `k < 31` with method 1, is a Rice parameter. `0x80 | b`, with
`b` in 0..31, marks an escaped partition whose residuals are stored as `b`-bit signed
values (`b = 0` means all residuals are zero). The partitions are those of FLAC:
`N` must be divisible by `2^partition_order`, the first partition holds
`N / 2^partition_order - order` residuals, and the others hold `N / 2^partition_order`.
The first partition must not be negative.

## 6. The residual model

The residual model turns each frame's residuals into rANS symbols and raw bits. Two
adaptive stages run per channel slot: an LMS predictor in the residual domain, and a scale
estimate that sets the coding context. Both carry their state from one frame to the next.

### 6.1 State

There are 8 channel slots. The slot of a subframe is its channel index in the frame. All
state starts at the beginning of the payload, once:

```text
per slot c in 0..8:
  Scale:  avg = 256 (u32), pend = 0 (u32)
  LMS:    w[32] = 0 (i16 weights), h[32] = 0 (i16 history, oldest first),
          u0[32] = u1[32] = 0 (i16 pending weight steps), in_shift = 0
rANS:     one symbol sequence for the whole payload (§7)
raw:      two bit streams, raw[0] and raw[1]
```

A slot's state changes only in frames where that slot is a residual channel. Constant and
verbatim subframes, junk, and slots beyond a frame's channel count leave it untouched.

### 6.2 Coding order

The residuals of a frame are coded time-major, channel-minor. Let `R` be the residual
channels of the frame in increasing channel order:

```text
for t in 0..N:
  for ch in R:
    if t >= order(ch):
      code residual index t - order(ch) of channel ch      (§6.4)
```

Raw bits of channel `ch` go to `raw[ch & 1]`. The context class is `cc = 1` if `ch > 0`
and `cc = 0` if `ch = 0`.

### 6.3 Scale state

```text
ctx(avg):                          # avg ≥ 15 always
  l    = floor_log2(avg)
  half = (avg >> (l - 1)) & 1      # the bit below the leading one
  b    = 2 * l + half              # scale bucket, < 62
  s    = max(l - 6, 0)             # low bits sent raw
  return (b, s)

next(sc, m):                       # after each residual, with m = its zigzag value
  sc.avg  = sc.avg - (sc.avg >> 4) + min(sc.pend, 2^26)
  sc.pend = m
```

`avg` is 16 times a running mean of the zigzag values, decaying by 1/16. The newest value
enters the mean one residual late.

### 6.4 One residual: token and raw bits

The coded value of a residual is its LMS error `e` (a signed 32-bit integer, §6.5).

```text
encode(sc, cc, e):
  (b, s) = ctx(sc.avg)
  u = zz32(e)                      # u32
  next(sc, u)
  v = u >> s
  t = token(v)
  rANS symbol t in context 2 * b + cc                                    (§7)
  if t >= 16:
    nb = (t - 16) / 2 + 3
    raw[ch & 1]: write (v mod 2^nb) in nb bits, then (u mod 2^s) in s bits
  else:
    raw[ch & 1]: write (u mod 2^s) in s bits

token(v):
  if v < 16: return v
  L = bitlen(v)                    # 5..32
  return 16 + 2 * (L - 5) + ((v >> (L - 2)) & 1)      # 16..71
```

Decoding inverts it:

```text
decode(sc, cc):
  (b, s) = ctx(sc.avg)
  t = rANS symbol in context 2 * b + cc
  if t >= 16:
    nb = (t - 16) / 2 + 3
    mant = read nb bits;  v = ((2 | (t & 1)) << nb) | mant
  else:
    v = t
  low = read s bits
  u = (v << s) | low               # u32 (wraps only on corrupt input)
  next(sc, u)
  return unzz32(u)
```

The rANS alphabet has 72 symbols. There are 124 contexts (62 scale buckets × 2 classes).
At most 53 raw bits are written per residual.

### 6.5 LMS stage

Each slot runs a 32-tap sign-sign LMS predictor over the slot's residuals. In the encoder,
residual `x` becomes error `e = x - p`. In the decoder, `x = e + p`. All arithmetic is
wrapping i32 unless stated otherwise.

```text
step(st, value):                       # encoder: value = x;  decoder: value = e
  acc = sum over j in 0..32 of i32(st.w[j]) * i32(st.h[j])          (wrapping i32)
  p   = (acc >> 12) << st.in_shift                                   (wrapping i32)
  if encoding: x = value;     e = x - p
  else:        e = value;     x = e + p
  sg = sign(e) * 4                                                   (-4, 0 or 4)
  for j: st.w[j] = st.w[j] + st.u0[j]                                (wrapping i16)
  st.u0 = st.u1
  for j: st.u1[j] = sign(st.h[j]) * sg                               (i16)
  shift h left by one (drop h[0]) and set h[31] = clamp(x >> st.in_shift, -32768, 32767)
  return encoding ? e : x
```

`sign` is -1, 0 or 1. A weight step is applied two samples after its sample (delayed LMS).

Before a frame's LMS pass, each residual channel `ch` of the frame sets its slot's input
shift from the slot's scale state *as it was before the frame's residuals were coded*:

```text
set_scale(st, avg):
  sh = min(max(floor_log2(avg) - 14, 0), 16)
  if sh != st.in_shift:
    for j: st.h[j] = sh > st.in_shift ? st.h[j] >> (sh - st.in_shift)
                                       : clamp(i32(st.h[j]) << (st.in_shift - sh), -32768, 32767)
    st.in_shift = sh
```

Only the history is rescaled. The weights and pending steps keep their values.

### 6.6 Per frame

Encoder:

1. For each residual channel `ch`: `set_scale(lms[ch], scale[ch].avg)`, then replace its
   residuals in time order by `step(lms[ch], x)`.
2. Code the errors in the coding order (§6.2) with `encode` (§6.4).

Decoder:

1. Copy each residual channel's scale state (`avg`).
2. Decode the errors in the coding order with `decode`. Decoding does not depend on the
   LMS stage.
3. For each residual channel `ch`: `set_scale(lms[ch], copied avg)`, then replace its
   errors in time order by `step(lms[ch], e)`. This gives the residuals back.

The channels are independent in steps 1 and 3, so their order does not matter.

## 7. rANS

The tokens of the whole payload form one sequence of symbols `s_0 .. s_{M-1}`, each with
its context. They are coded by rANS with 64-bit states, 32-bit renormalisation, 12-bit
probabilities and **8 interleaved states**: symbol `j` uses state `j mod 8`. Each context
has a static frequency table `f[0..72]`. Every entry is at least 1 and each table sums to
4096 (Appendix A).

### 7.1 Slots: the alias layout

Symbols map to the 4096 slots `0..4096` of a context through an alias table. The mapping
is part of the format: a symbol's state update depends on which slot it lands in.

```text
build(f):                         # f: 72 frequencies
  A  = 128                        # buckets: alphabet size rounded up to a power of two (at least 4)
  bs = 4096 / A = 32              # slots per bucket
  rem[i]    = f[i] for i < 72, 0 for 72 ≤ i < 128
  cutoff[i] = bs;  alias[i] = 0;  rank_start[i] = 0     for all i < A
  small = [i in 0..A ascending with rem[i] < bs]        # used as stacks: the top is the last element
  large = [i in 0..A ascending with rem[i] > bs]
  while small and large are both non-empty:
    s = pop(small)
    l = top(large)                                      # not popped
    cutoff[s] = rem[s];  alias[s] = l
    rem[l] -= bs - rem[s]
    if rem[l] < bs:   pop(large);  push(small, l)
    elif rem[l] == bs: pop(large)
  # ranks: a symbol's own slots first, then its alias slots in bucket order
  next[i] = cutoff[i] for all i
  for i in 0..A ascending:
    if cutoff[i] < bs:
      rank_start[i] = next[alias[i]];  next[alias[i]] += bs - cutoff[i]
```

Bucket `i` covers slots `i*bs .. i*bs + bs`. Its first `cutoff[i]` slots belong to symbol
`i`, with ranks `0 .. cutoff[i]`. The remaining `bs - cutoff[i]` slots belong to symbol
`alias[i]`: slot `i*bs + o`, with `o ≥ cutoff[i]`, has rank `rank_start[i] + o - cutoff[i]`. Each symbol `k` then has exactly `f[k]` slots,
ranked `0 .. f[k]`. Write `slot(k, r)` for the slot of rank `r`, and `sym_rank(slot)` for
the inverse.

### 7.2 Decoding

```text
words: w[0..W]; reading past the end gives 0
x[k] = w[2k] << 32 | w[2k + 1]   for k in 0..8;   pos = 16;   lane = 0
for each symbol, in context c:
  slot   = x[lane] & 4095
  (k, r) = sym_rank_c(slot)
  x[lane] = f_c[k] * (x[lane] >> 12) + r
  if x[lane] < 2^31:  x[lane] = x[lane] << 32 | w[pos];  pos += 1
  lane = (lane + 1) mod 8
  → symbol k
```

At the end of the payload, `pos ≤ W` must hold. Otherwise the words were exhausted and the
file is corrupt.

### 7.3 Encoding

```text
x[k] = 2^31 for k in 0..8;   out = empty list of u32 (built back to front)
for j = M-1 down to 0:
  symbol k in context c, lane = j mod 8, f = f_c[k]
  if x[lane] ≥ 2^51 * f:  prepend (x[lane] mod 2^32) to out;  x[lane] >>= 32
  q = x[lane] div f;  r = x[lane] mod f            (integer division)
  x[lane] = (q << 12) + slot_c(k, r)
for k = 7 down to 0:  prepend (x[k] mod 2^32), then prepend (x[k] >> 32)
```

The result is `x0_hi, x0_lo, x1_hi, …, x7_lo` followed by the renormalisation words in
decoding order. `W = len(out)` is always at least 16, even with no symbols.

## 8. FLAC streams

The FLAC payload reproduces the original file byte for byte. The file splits into a prefix
(everything up to the end of the metadata), then a sequence of FLAC frames and junk.

### 8.1 Prefix (stream 0)

The prefix is the original's bytes up to the first byte after the last metadata block:

1. If the file starts with `ID3` and is at least 10 bytes long, skip an ID3v2 tag:
   `p = 10 + size + (d[5] & 0x10 ? 10 : 0)`, where `size` is the syncsafe integer in bytes
   6..9 (7 bits per byte, big-endian).
2. `fLaC` must follow at `p`.
3. Metadata blocks follow, each with a 4-byte header: bit 7 means last, bits 0..6 give the
   type, then a 24-bit big-endian length. The prefix ends after the last block.

At least one STREAMINFO block (type 0, length ≥ 18) must be present. If there are several,
the last one counts. Its sample size, `((u64be at body offset 10) >> 36 & 31) + 1`, is the
stream's `bps`, used by frame headers whose sample size code is 0.

A decoder must parse its prefix stream this way and reach exactly the stream's end.

### 8.2 Layout (stream 1) and junk (stream 2)

The rest of the file alternates between runs of frames and pieces of junk. The layout
stream is a sequence of varints, starting and ending with a run:

```text
run_0, junk_len_0, run_1, junk_len_1, …, run_K
```

```text
decode:
  loop:
    run = varint;  decode `run` frames (§8.5)
    if the layout stream is exhausted: stop
    n = varint;  copy the next n bytes of the junk stream to the output
```

Runs can be 0, for example when the file starts with junk. The junk stream is all junk
bytes concatenated.

*(encoder)* Segmentation: from the end of the prefix, at each position `p`, try to parse a
frame (§8.6). If one parses, it is the next frame and `p` moves to its end. If not, the
junk extends from `p` to the first position `q > p` holding a `0xFF` byte where a frame
parses, or to the end of the file. The layout then records the run of frames before it and
its length.

### 8.3 Frame headers (stream 3)

A FLAC frame header is: 2 bytes of sync code and blocking strategy, 1 byte of block size
and sample rate codes, 1 byte of channel assignment and sample size code, the coded
frame or sample number (1..7 bytes), an optional block size (1 or 2 bytes, §8.6), an
optional sample rate (1 or 2 bytes), then the CRC-8.

A frame header here means the header bytes from the sync code up to the CRC-8, excluding
the CRC-8 (5..15 bytes). Per frame, stream 3 holds either:

- `0x00`: the header is the prediction from the previous frame's header, or
- `L` (1..255) followed by `L` header bytes.

The prediction from the previous header `P` (none at the first frame, where the prediction
is empty and never matches):

```text
predict(P, prev_block_size):
  if len(P) < 5: return empty
  (v, n) = coded_number(P)                  # P[4 .. 4+n]
  step = (P[1] & 1) ? prev_block_size : 1   # variable block size: sample number; else frame number
  return P[0..4] ++ put_coded_number(v + step) ++ P[4+n ..]

coded_number(P):                            # P is a valid header (§8.6), so n is 1..7
  b = P[4];  n = max(leading_ones(b), 1)
  v = (n == 1) ? b : b & (0x7f >> n)
  for c in P[5 .. 4+n]:  v = v << 6 | (c & 0x3f)
  return (v, n)

put_coded_number(v):                        # v as u64
  if v < 0x80: return [v]
  n = 2;  while n < 7 and v ≥ 2^(5n+1): n += 1
  lead = (n == 7) ? 0xfe : (0xff00 >> n) & 0xff
  return [ (lead | (v >> 6(n-1))) & 0xff ] ++ [ 0x80 | ((v >> 6i) & 0x3f) for i = n-2 down to 0 ]
```

The encoder emits `0x00` exactly when the prediction equals the header. The decoder
appends the CRC-8 (Appendix B) to the header bytes and parses the result as a FLAC frame
header (§8.6, header rules), using STREAMINFO's `bps` for sample size code 0. This gives
the frame's block size, channel assignment and sample size. It then remembers the header
and block size for the next prediction. The prediction carries across junk.

### 8.4 Subframes (streams 4 and 5)

As in §5.1, with the Rice parameters.

### 8.5 Decoding a frame

1. Read the header (§8.3) and the subframe parameters (§8.4).
2. Decode the residuals of the residual channels (§6.6), `N - order` per channel.
3. For each escaped partition, every residual must fit in `b` signed bits (`b = 0`: the
   residual must be 0).
4. Write the FLAC frame:
   - the header bytes, then their CRC-8;
   - per channel, the subframe header byte `typ << 1 | (wasted > 0)` (its top bit is zero).
     If `wasted > 0`, it is followed by `wasted - 1` zero bits and a one bit;
   - constant: the value in `bits` bits. Verbatim: `N` samples in `bits` bits.
     Fixed / LPC: `order` warm-up samples in `bits` bits. LPC then adds `precision - 1` in
     4 bits, the shift in 5 bits (two's complement) and the coefficients in `precision`
     bits each;
   - fixed / LPC: `method` in 2 bits, `partition_order` in 4 bits, then each partition:
     - escaped: all ones in `4 + method` bits, `b` in 5 bits, the residuals in `b` bits
       each;
     - Rice: `k` in `4 + method` bits, then each residual as `u = zz32(r)`: `u >> k` zero
       bits, a one bit, then the low `k` bits of `u`;
   - zero bits to a byte boundary, then the CRC-16 (big-endian) of all the frame's bytes
     from the sync code.

Rice codes are unique given `k`, so the bytes are those of the original frame.

### 8.6 What counts as a frame *(encoder)*

A FLAC frame at position `p` is accepted only if all of the following hold. Anything else
is junk. A decoder's output does not depend on these rules, but the encoder's
segmentation does.

Header:

- `d[p] = 0xFF` and `d[p+1] & 0xFE = 0xF8`.
- Block size code ≠ 0, sample rate code ≠ 15, channel assignment ≤ 10, sample size
  code ≠ 3, and the reserved bit (bit 0 of byte 3) is 0.
- The sample size (code 0 means STREAMINFO's; codes 1, 2, 4, 5, 6, 7 mean 8, 12, 16, 20,
  24, 32) is in 4..32.
- The coded number is 1..7 bytes. Its first byte has 0 or 2..7 leading ones, and the
  continuation bytes match `10xxxxxx`.
- Block size: code 1 → 192, 2..5 → `576 << (code-2)`, 6 → next byte + 1,
  7 → next u16be + 1, 8..15 → `256 << (code-8)`. Sample rate code 12 adds 1 byte;
  13 and 14 add 2.
- The CRC-8 byte matches.

Subframes:

- The padding bit is 0. The type is valid (§5). If the wasted flag is set, the unary
  count has fewer than 32 leading zeros, and `wasted < full`.
- `order ≤ N`. The LPC precision field is not 15. The method is 0 or 1. `N` is divisible
  by the partition count, and `N / 2^partition_order ≥ order`.
- Every Rice-coded residual has `u < 2^32` (it fits in i32 after unzigzag).
- The frame lies within the file (padding included). The bits up to the byte boundary are
  zero, and the CRC-16 matches.

## 9. PCM streams

The PCM payload holds a byte range of interleaved integer samples, cut into blocks that are
coded like FLAC frames. All other bytes are kept verbatim.

### 9.1 Streams 0..2

- **Prefix** (stream 0): the bytes before the sample data. This is the WAV / AIFF header,
  and is empty for headerless PCM.
- **Layout** (stream 1): 3 format bytes, then `varint nframes`:

  | Byte | Meaning |
  |---|---|
  | 0 | channels (1..8) |
  | 1 | bytes per sample (1..4) |
  | 2 | bit 0: big-endian; bit 1: unsigned (offset binary); other bits zero |

  `nframes` is the number of sample frames (one sample per channel).
- **Junk** (stream 2): the bytes after the sample data. This includes a trailing partial
  sample frame and any chunks after the data.

The original is `prefix ++ samples (nframes × channels × bytes) ++ junk`. Its length must
equal the container's `len`.

### 9.2 Blocks

The sample frames are cut into blocks of 4096. The last block holds the remaining
`nframes mod 4096` frames, if any. Each block is a frame (§5) with `N` = its length,
`bps = 8 × bytes per sample`, and channels = the format's channel count.

- **Stream 3**: one byte per block, the channel assignment. With 2 channels it must be
  1, 8, 9 or 10. Otherwise it must be `channels - 1`.
- **Streams 4, 5**: the subframe parameters (§5.1) without Rice parameters.
- The residuals go through the residual model (§6) as in FLAC.

### 9.3 Reconstruction

Per channel, apply FLAC's synthesis in 64-bit integers (wrapping):

- constant: `N` copies of the value. Verbatim: the samples.
- fixed, order `o`: the warm-up samples, then `s[t] = pred(t) + res[t - o]` with the
  predictors 0, `s[t-1]`, `2s[t-1] - s[t-2]`, `3(s[t-1] - s[t-2]) + s[t-3]` and
  `4(s[t-1] + s[t-3]) - 6s[t-2] - s[t-4]`.
- LPC: the warm-up samples, then
  `s[t] = (Σ_{j<order} coef[j] × s[t-1-j]) >> shift + res[t - order]`. With a negative
  shift, the sum is shifted left by `-shift` instead.
- Then shift every sample left by `wasted`.

Undo the stereo decorrelation (`a` = channel 0, `b` = channel 1):

| assign | channel 0 | channel 1 |
|---|---|---|
| 8 (left/side) | `a` | `a - b` |
| 9 (side/right) | `a + b` | `b` |
| 10 (mid/side) | `(m + b) >> 1` | `(m - b) >> 1`, with `m = (a << 1) \| (b & 1)` |

Write the samples interleaved, as the low `8 × bytes` bits of each value. XOR the top bit
for unsigned formats, and use the format's byte order.

### 9.4 Finding the samples *(encoder)*

Headerless PCM (PCM magic) uses the whole input as sample data, in the format the user
gives. For WAV / AIFF (WAV magic):

- **WAV**: `RIFF` at 0 and `WAVE` at 8, then chunks from offset 12 (id, `u32le` length,
  body padded to an even length). A `fmt ` chunk (length ≥ 16) must be PCM: format tag 1,
  or tag `0xFFFE` with length ≥ 40 and sub-format GUID starting with `u16le` 1. It must
  have channels ≥ 1, block align divisible by channels, and 1 ≤ bits ≤
  `8 × (align / channels)`. Bytes per sample = `align / channels`. Samples are
  little-endian, and unsigned for 1-byte samples. The first `data` chunk after `fmt `
  holds the samples. Its length is taken to be the rest of the file if it is 0 or
  `0xFFFFFFFF`, and is otherwise clipped to the file.
- **AIFF / AIFC**: `FORM` at 0, `AIFF` or `AIFC` at 8, then big-endian chunks from
  offset 12. `COMM` (length ≥ 18) gives channels at body+0 and bits at body+6 (1..32).
  Bytes per sample is `ceil(bits / 8)`, and samples are signed big-endian. For AIFC, the
  compression type at body+18 must be `NONE` or `twos` (big-endian) or `sowt`
  (little-endian). Samples start in `SSND` at `body + 8 + offset` (offset = `u32be` at the
  body start). They run for `length - 8 - offset` bytes, clipped to the file.
- The format must have 1..8 channels and 1..4 bytes per sample.

`nframes = data length / (channels × bytes)`.

### 9.5 Making the blocks *(encoder)*

Samples are read as signed integers (XOR the top bit first for unsigned formats). Then,
per block:

**Stereo** (2 channels and `N ≥ 3`, otherwise `assign = channels - 1`): compute
`side = l - r` and `mid = (l + r) >> 1`. Let `cost(x)` be the sum of
`|x[t] - 2x[t-1] + x[t-2]|`. Pick the first minimum among
`(cost(l) + cost(r), 1)`, `(cost(l) + cost(side), 8)`, `(cost(side) + cost(r), 9)`,
`(cost(mid) + cost(side), 10)`. Replace the channels accordingly.

**Per channel** (full sample size `F = bps + side bit`):

1. `wasted` = trailing zero bits of the OR of all samples (0 if all are zero), at most
   `F - 1`. Shift the samples right by it.
2. If all samples are equal: a constant subframe with `wasted = 0` and the original
   (unshifted) value.
3. Otherwise, if LPC analysis (below) gives a predictor: an LPC subframe with
   precision 14, warm-up = the first `order` samples, and residuals
   `x[t] - ((Σ coef[j] x[t-1-j]) >> shift)` in 64 bits.
   Otherwise: fixed order 1, with residuals `x[t] - x[t-1]`.
4. If a residual does not fit in i32: a verbatim subframe of the shifted samples, keeping
   `wasted`.

**LPC analysis** (`x`: the shifted samples, `n` of them; `bits = F - wasted`), in IEEE
double precision:

```text
max_order = min(16, n / 4);  if 0: none
window: T = max(n / 4, 1); for i: k = min(i, n-1-i); win[i] = k < T ? 0.5 - 0.5 cos(π k / T) : 1
y[i] = x[i] × win[i]
r[lag] for lag in 0..=max_order:
  eight partial sums: acc[m] += y[lag + 8q + m] × y[8q + m] over whole chunks of 8 (q ascending);
  r[lag] = (acc[0] + … + acc[7], summed left to right) + (the leftover products summed left to right)
if r[0] ≤ 0: none
r[0] *= 1 + 1e-9
Levinson–Durbin: a = 0[33]; err = r[0]; best = (+∞, 0, a); per_order = (14 + bits) × 4
for i in 0..max_order:
  k = -r[i+1]; for j < i: k -= a[j] × r[i-j]; k /= err
  for j < i: a[j] = a_prev[j] + k × a_prev[i-1-j];  a[i] = k
  err *= 1 - k²;  if err ≤ 0: break
  est = 0.5 × n × log2(err) + per_order × (i+1)
  if est < best.est: best = (est, i+1, a)
order = best.order; if 0: none
cmax = max |a[j]| (j < order); if cmax ≤ 0: none
shift = clamp(13 - (floor(log2 cmax) + 1), 0, 15)
quantise with error feedback: e = 0; for j < order:
  v = -a[j] × 2^shift + e;  q[j] = clamp(round_half_away(v), -8192, 8191);  e = v - q[j]
```

The output bytes depend on these floating-point results, so a byte-identical encoder must
evaluate them in the same order with correctly rounded `cos` and `log2`.

## 10. Choosing the kind and the version *(encoder)*

1. With a raw sample format given: kind PCM, the whole input is the sample data.
2. Otherwise, if the input has FLAC metadata (§8.1): kind FLAC.
3. Otherwise, if WAV / AIFF sample data is found (§9.4): kind WAV.
4. Otherwise: kind PCM, version 0.

The coded file (version 1) is written unless it would be at least as long as the stored
one (input length + 8), or coding fails. In those cases the input is stored (version 0)
under the same magic.

## 11. Validation

A decoder should reject the following rather than produce output:

- Unknown magic or version, or a header or payload cut short (a varint, a byte stream, the
  rANS words or raw stream 0).
- A zstd stream that does not decompress to exactly `raw_len` bytes.
- FLAC: a prefix that does not parse to its own end (§8.1); a header that is not valid
  (§8.6, header rules) or a prediction with no previous header; a subframe type outside
  §5; `wasted ≥ full` or a wasted flag with `wasted = 0`; an order larger than `N`; a
  sample out of its `bits` range; an LPC precision outside 1..15 or a shift outside
  -16..15; a coefficient out of range; method > 1; bad partitions; a Rice parameter ≥ 15
  (method 0) or ≥ 31 (method 1); an escape byte with bits 5..6 set; escaped residuals out
  of range (§8.5).
- PCM: format bytes outside §9.1; `nframes × channels × bytes + len(prefix) + len(junk) ≠ len`;
  a bad channel assignment.
- rANS words exhausted (§7.2), or a raw bit stream read past its end.
- An output whose length or xxh3 differs from the header.

This implementation also bounds its work on corrupt input. It rejects `len > 64 ×
file size + 2^20` and `raw_len > len + 2^20`, and stops writing when the output would
exceed `len`. A Rice code can be up to 2^32 bits long.

## Appendix A: built-in frequency tables

The 124 frequency tables (§6.4 contexts `2b + cc`, 72 frequencies each) are fixed by the
format. They are built into the implementation as `model/builtin.bin`: a zstd frame that
decompresses to 17,860 bytes:

```text
u32le layout_id = 0x0b49843e     (FNV-1a 32 over the u32 values 124, 72, 32, 1:
                                  h = 0x811c9dc5; for v: h = (h ^ v) × 0x01000193 mod 2^32)
124 × 72 × u16le frequencies     context 0 first; each context's 72 values in symbol order
```

SHA-256 of the decompressed bytes:
`007e699ed783140496b4b0e9f3585b422dbcfd99341c1e3ebef7554e442af1a2`. Contexts that never
occurred in training are flat: 57 for symbols 0..63 and 56 for symbols 64..71.

`model/builtin.bin` in base64 (2306 bytes, SHA-256
`9f63b9ef15728deee88e46d3210c6199c73f7e4237b877655c2e6ccb4dc8ad81`):

```text
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
3NxDkF2GypTuDkke0ngQm8MTWk/TgbYri2U=
```

Changing the tables (retraining with `train`) changes the format and needs a new version
number.

## Appendix B: CRCs

- **CRC-8** (frame header): polynomial `0x07`, initial value 0, MSB-first, no reflection,
  no final XOR. It covers the header bytes from the sync code.
- **CRC-16** (frame): polynomial `0x8005`, initial value 0, MSB-first, no reflection, no
  final XOR. It covers every byte of the frame from the sync code up to the CRC itself.
