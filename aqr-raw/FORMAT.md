# aqr-raw format

This document specifies the modules `pcm-recons` and `raster-recons` of entry formulas
(`aqr_raw::formula`), which give uncompressed samples back from their coded streams, and
how the encoder takes the trivial containers around such samples apart. It is meant to be
complete: a decoder, and an encoder whose output a decoder accepts, can be written from it
alone (with aqr-png's `FORMAT.md` for the rANS tables of `raster-recons`, §7.4). Sections
marked *(encoder)* describe the choices this implementation makes. A decoder does not need
them, but an encoder must follow them to produce the same streams as this implementation.

The frames, the residual model and the rANS coder of §3–§5 are also those of aqr-flac's
`flac-recons`, whose `FORMAT.md` refers to them.

Contents:

1. [Conventions](#1-conventions)
2. [The modules](#2-the-modules)
3. [Frames](#3-frames)
4. [The residual model](#4-the-residual-model)
5. [rANS](#5-rans)
6. [`pcm-recons`](#6-pcm-recons)
7. [`raster-recons`](#7-raster-recons)
8. [Containers](#8-containers-encoder) *(encoder)*
9. [Appendix A: built-in frequency tables](#appendix-a-built-in-frequency-tables)

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
- **zstd** means one Zstandard frame (RFC 8878).

## 2. The modules

Each module gives the samples of one layout, the module's params, and nothing else: the
output is exactly the samples, `len` bytes. A file that holds samples in a trivial
container is, in its formula, the concatenation of its bytes before the samples (a slice
of a stream of class `raw/header`), the module's application, and its bytes after them
(class `raw/trailer`), either of the two left out where empty (§8). A recompressor that
finds samples inside a format of its own applies the modules the same way.

| module | version | output | params | resources | inputs |
|---|---|---|---|---|---|
| `pcm-recons` | 1 | interleaved integer PCM samples | the sample format (§6.1) | the frequency tables (Appendix A) | 6 (§6.1) |
| `raster-recons` | 1 | rows of 8-bit samples | the raster layout (§7.1) | none | 2 (§7.1) |

## 3. Frames

Audio is a sequence of **frames**, using FLAC's model of a frame
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

The residuals of all frames go through the residual model (§4) into the rANS words and
the two raw bit streams. Everything else goes into byte streams.

### 3.1 Subframe parameters

The parameters of a frame's subframes are stored in channel order, in two byte streams read
in parallel, `sub` and `warm`:

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
                    if with Rice parameters:
                      sub: u8 method << 4 | partition_order     (method 0 or 1, partition_order 0..15)
                      sub: 2^partition_order × u8 param
```

All samples (constant value, verbatim samples, warm-up samples) are the subframe's coded
values: they come after the wasted bits are removed, so each fits in `bits` signed bits.
Coefficients fit in `precision` signed bits.

The Rice parameters are there only in aqr-flac's FLAC payload (its `FORMAT.md`), which
rebuilds FLAC's own residual coding. `param` gives the Rice parameter `k` of each residual partition.
`k < 15` with method 0, or `k < 31` with method 1, is a Rice parameter. `0x80 | b`, with
`b` in 0..31, marks an escaped partition whose residuals are stored as `b`-bit signed
values (`b = 0` means all residuals are zero). The partitions are those of FLAC:
`N` must be divisible by `2^partition_order`, the first partition holds
`N / 2^partition_order - order` residuals, and the others hold `N / 2^partition_order`.
The first partition must not be negative.

## 4. The residual model

The residual model turns each frame's residuals into rANS symbols and raw bits. Two
adaptive stages run per channel slot: an LMS predictor in the residual domain, and a scale
estimate that sets the coding context. Both carry their state from one frame to the next.

### 4.1 State

There are 8 channel slots. The slot of a subframe is its channel index in the frame. All
state starts at the beginning of the payload, once:

```text
per slot c in 0..8:
  Scale:  avg = 256 (u32), pend = 0 (u32)
  LMS:    w[32] = 0 (i16 weights), h[32] = 0 (i16 history, oldest first),
          u0[32] = u1[32] = 0 (i16 pending weight steps), in_shift = 0
rANS:     one symbol sequence for the whole payload (§5)
raw:      two bit streams, raw[0] and raw[1]
```

A slot's state changes only in frames where that slot is a residual channel. Constant and
verbatim subframes, anything between frames (FLAC's junk), and slots beyond a frame's channel count leave it untouched.

### 4.2 Coding order

The residuals of a frame are coded time-major, channel-minor. Let `R` be the residual
channels of the frame in increasing channel order:

```text
for t in 0..N:
  for ch in R:
    if t >= order(ch):
      code residual index t - order(ch) of channel ch      (§4.4)
```

Raw bits of channel `ch` go to `raw[ch & 1]`. The context class is `cc = 1` if `ch > 0`
and `cc = 0` if `ch = 0`.

### 4.3 Scale state

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

### 4.4 One residual: token and raw bits

The coded value of a residual is its LMS error `e` (a signed 32-bit integer, §4.5).

```text
encode(sc, cc, e):
  (b, s) = ctx(sc.avg)
  u = zz32(e)                      # u32
  next(sc, u)
  v = u >> s
  t = token(v)
  rANS symbol t in context 2 * b + cc                                    (§5)
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

### 4.5 LMS stage

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

### 4.6 Per frame

Encoder:

1. For each residual channel `ch`: `set_scale(lms[ch], scale[ch].avg)`, then replace its
   residuals in time order by `step(lms[ch], x)`.
2. Code the errors in the coding order (§4.2) with `encode` (§4.4).

Decoder:

1. Copy each residual channel's scale state (`avg`).
2. Decode the errors in the coding order with `decode`. Decoding does not depend on the
   LMS stage.
3. For each residual channel `ch`: `set_scale(lms[ch], copied avg)`, then replace its
   errors in time order by `step(lms[ch], e)`. This gives the residuals back.

The channels are independent in steps 1 and 3, so their order does not matter.

## 5. rANS

The tokens of the whole payload form one sequence of symbols `s_0 .. s_{M-1}`, each with
its context. They are coded by rANS with 64-bit states, 32-bit renormalisation, 12-bit
probabilities and **8 interleaved states**: symbol `j` uses state `j mod 8`. Each context
has a static frequency table `f[0..72]`. Every entry is at least 1 and each table sums to
4096 (Appendix A).

### 5.1 Slots: the alias layout

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

### 5.2 Decoding

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

### 5.3 Encoding

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

## 6. `pcm-recons`

### 6.1 The module

**Params**: three bytes, the sample format:

| Byte | Meaning |
|---|---|
| 0 | channels (1..8) |
| 1 | bytes per sample (1..4) |
| 2 | bit 0: big-endian; bit 1: unsigned (offset binary); other bits zero |

`len` must be a multiple of `channels × bytes`. Any other params are an error.

**Resources**: exactly one, the frequency tables: the decompressed bytes of Appendix A
(17,860 bytes), whose SHA-256 is given there. The decoder uses them as the tables of §4.4.
It MUST fail unless the resource is exactly 17,860 bytes, its layout id is the one given in
Appendix A, and every table consists of nonzero frequencies summing to 4096; it does not
need to check the SHA-256. Any other number of resources is an error.

**Inputs**: six slots, always in this order, each possibly empty:

| # | Class | Content |
|---|---|---|
| 0 | `pcm/blocks` | the channel assignment of each block (§6.2) |
| 1 | `pcm/subframes` | `sub`: subframe parameters (§3.1, §6.2) |
| 2 | `pcm/warmup` | `warm`: warm-up, constant and verbatim samples (§3.1) |
| 3 | `pcm/rans` | the W words of the rANS coder (§5) as `u32le`, so a length that is not a multiple of 4 is an error |
| 4 | `pcm/raw0` | raw bit stream 0 |
| 5 | `pcm/raw1` | raw bit stream 1 |

### 6.2 Blocks

The `len / (channels × bytes)` sample frames are cut into blocks of 4096. The last block holds the remaining
`nframes mod 4096` frames, if any. Each block is a frame (§3) with `N` = its length,
`bps = 8 × bytes per sample`, and channels = the format's channel count.

- **Blocks** (input 0): one byte per block, the channel assignment. With 2 channels it must
  be 1, 8, 9 or 10. Otherwise it must be `channels - 1`.
- **Subframes, warm-up** (inputs 1 and 2, `sub` and `warm`): the subframe parameters
  (§3.1) without Rice parameters.
- The residuals go through the residual model (§4) as in FLAC.

### 6.3 Reconstruction

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

### 6.4 Making the blocks *(encoder)*

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

### 6.5 Validation

A decoder should reject the following rather than produce output:

- Params outside §6.1, or `len` not a multiple of a sample frame; a number of inputs other
  than six; rANS words of a length that is not a multiple of 4; streams cut short (a
  varint or a byte stream).
- A bad channel assignment; a subframe type outside §3; `wasted ≥ full` or a wasted flag
  with `wasted = 0`; an order larger than `N`; a sample out of its `bits` range; an LPC
  precision outside 1..15 or a shift outside -16..15; a coefficient out of range.
- rANS words exhausted (§5.2), or a raw bit stream read past its end.

This implementation also bounds its work on corrupt input: it rejects `len > 64 × S +
2^20`, where S is the total size of the inputs.

## 7. `raster-recons`

### 7.1 The module

**Params**: the layout: `varint width`, `varint height`, `varint pad`, then one byte,
`channels`. The raster is `height` rows of `stride = width × channels + pad` bytes: the
row's samples, pixels of `channels` interleaved 8-bit samples, then `pad` bytes of
padding. It must have `width ≥ 1`, `height ≥ 1`, `channels` in 1..4,
`width × channels < 2^32` and `stride × height < 2^48`, and nothing may follow the
channels byte. `len` must be `stride × height`.

**Resources**: none.

**Inputs**: two slots:

| # | Class | Content |
|---|---|---|
| 0 | `raster/coded` | the coded samples, as chunks of rows (§7.4) |
| 1 | `raster/padding` | the padding of every row, in row order: `pad × height` bytes |

The output is, row by row, the row's samples (§7.3) then its padding.

### 7.2 The pixel model

`rb = width × channels`. The samples are coded in order, row by row, each row from its
first byte; byte `x` of a row is of channel `ch = x mod channels`. `v(y, x)` is the value
of byte `x` of row `y`, and 0 outside the raster (`x < 0` or `y < 0`): byte `x - channels`
is the same channel's sample of the pixel to the left, row `y - 1` the row above.

- `a = v(y, x - channels)`, `b = v(y - 1, x)`, `c = v(y - 1, x - channels)`.
- `med(a, b, c)` is the median of `a`, `b` and `a + b - c` (LOCO-I's predictor).
- `pred = med(a, b, c)`, except where `channels ≥ 3` and `ch` is 1 or 2:
  `pred = clamp(med(a, b, c) + v(y, x - 1) - med(a1, b1, c1), 0, 255)`, where `a1`, `b1`,
  `c1` are the neighbours of byte `x - 1` as above (the previous channel's residual is
  added).
- `e(y, x) = (v(y, x) - pred) mod 256`, the residual, and 0 outside the raster.
  `mag(e)` is the magnitude of `e` read as signed 8 bits (0..128).
- `act = |a - c| + |b - c| + mag(e(y, x - channels)) + mag(e(y - 1, x))`.
- `bucket(v)` is the bit length of `v`, capped at 11: 0 for 0, 1 for 1, 2 for 2–3, …, 11
  for 1024 and up.
- The byte is coded as the symbol `zigzag(e)` (`zigzag` as aqr-png's: 0, −1, 1, −2, …,
  127, −128 map to 0, 1, 2, 3, …, 254, 255) from table `12 × min(ch, 3) + bucket(act)`.
  When decoding, `v = (pred + unzigzag(symbol)) mod 256`.

This is aqr-png's model of a pixel byte (its `FORMAT.md`, *The model*, the *image* kind),
with the magnitudes of the residuals left and above in place of those of the filtered bytes.

### 7.3 Chunks

The rows are cut into chunks of `R = max(1, floor(2^22 / rb))` rows, the last of the
remaining `height mod R` rows if any. The model's state (the previous row's values and
residuals) carries across chunks; only the tables are the chunk's own.

### 7.4 Entropy coding

Input 0 is the chunks one after the other, nothing after the last. A chunk is:

| field | |
|---|---|
| varint | length of the tables |
| varint | length of the symbol stream |
| bytes | the tables |
| bytes | the symbol stream |

There are 48 tables, each of an alphabet of 256 symbols. The tables and the symbol stream are
coded exactly as aqr-png's (its `FORMAT.md`, *Tables* and *Symbol stream*): a bitmap of the
tables used, their levels, and four interleaved 32-bit rANS states with 12-bit
probabilities, decoding the chunk's `R × rb` symbols in coding order. After the chunk's
last symbol, its stream must have been read exactly to its end, with all four states at
`2^15`.

### 7.5 Validation

A decoder should reject params outside §7.1, a `len` other than the raster's, a number of
inputs other than two, padding of another length than `pad × height`, a chunk cut short or
not read exactly (§7.4), tables that do not parse, and bytes after the last chunk.

## 8. Containers *(encoder)*

### 8.1 Taking a container apart

The encoder recognises the containers below by their headers. The samples are the bytes
`[start, start + n)` of the file, where the header says; the formula is
`concat(slice(header), apply(module, samples), slice(trailer))` (§2), with the header the
bytes before `start` and the trailer those after the samples. Nothing in the header or the
trailer is derived from the samples, so the container's bytes come back whatever they say.
A container whose samples are not all in the file, or that holds none, is not coded.
Headerless samples of a layout given to the encoder are taken from the start of the
input; for PCM, a partial sample frame at the end is the trailer.

### 8.2 WAV and AIFF

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
- The format must have 1..8 channels and 1..4 bytes per sample (§6.1). The samples are
  cut to whole sample frames.


### 8.3 BMP

`BM` at 0, `u32le` at 10 the offset of the pixel data, `u32le` at 14 the size of the
information header. With size 12 (OS/2), the width, height, planes and bits per pixel are
`u16le` at 18, 20, 22 and 24, and the compression is 0. With size 40, 52, 56, 64, 108 or
124, the width and height are `i32le` at 18 and 22, planes and bits per pixel `u16le` at
26 and 28, and the compression `u32le` at 30. Planes must be 1, the width at least 1 and
the height not 0 (a negative height is a top-down image; the rows are coded in file order
either way). Taken are 8 bits per pixel with compression 0 (palette indices, 1 channel),
24 with compression 0 (3 channels) and 32 with compression 0 or 3 (bit fields; 4
channels). Rows are padded to a multiple of 4 bytes, and there are `|height|` of them.

### 8.4 PNM

`P5` (PGM, 1 channel), `P6` (PPM, 3 channels) or `P7` (PAM). For `P5` and `P6`, the width,
the height and the maximum value follow as decimal numbers (at most 9 digits), each after
whitespace and comments (`#` to the end of the line), then exactly one whitespace byte
before the samples. For `P7`, the header is lines up to and including `ENDHDR` (within
4096 bytes), of which `WIDTH`, `HEIGHT`, `DEPTH` (1..4: the channels) and `MAXVAL` are
read. The maximum value must be below 256 (8-bit samples) and the width and height at
least 1. The rows are not padded. Only the first image of a file is taken: any others are
in the trailer.

## Appendix A: built-in frequency tables

The 124 frequency tables (§4.4 contexts `2b + cc`, 72 frequencies each) are the resource
of `pcm-recons` (§6.1) and of aqr-flac's `flac-recons`. The reference encoder takes them
from `model/audio.bin`, a zstd frame that decompresses to 17,860 bytes:

```text
u32le layout_id = 0x0b49843e     (FNV-1a 32 over the u32 values 124, 72, 32, 1:
                                  h = 0x811c9dc5; for v: h = (h ^ v) × 0x01000193 mod 2^32)
124 × 72 × u16le frequencies     context 0 first; each context's 72 values in symbol order
```

SHA-256 of the decompressed bytes:
`007e699ed783140496b4b0e9f3585b422dbcfd99341c1e3ebef7554e442af1a2`. Contexts that never
occurred in training are flat: 57 for symbols 0..63 and 56 for symbols 64..71.

`model/audio.bin` in base64 (2306 bytes, SHA-256
`9f63b9ef15728deee88e46d3210c6199c73f7e4237b877655c2e6ccb4dc8ad81`):

```text
KLUv/WDERMVHAHp5UBgqIOzJcv9zKEX9////Pyf+fwbJoZFNcXbMd6Bn1jcQ6+/uIfXdESKNkC1T
awFxAY4BC7abwiCObKZW53qiT6qnn1rrPMfm692Gw/iH5Hdy9ax0/SuIKhZGGQG817lLvLiPmxxm
Ozva1LqK1rpWqIiq6IzaKAF44TOOXM2RvlW5HnZJ6RRTKbuLxHA/uMIjpvGS1yxpXKUrYiMrp5fK
qrjEhtAE9/144W3mMaglG+BIDeyl0b4WaYsOGduvF/5bySaOc5BDXaZlSyqFf1Wxd71RwzIu15vs
u9ccfGbFf12mSDgV8sxQTqeJ5ogPOPC3dwx4nhlrdpwK6dlKvXGWtdFpJgDCi/N29I7+GFOBtcrJ
6jRwlhr7bG732S128KJldMeVTromOTedfucnAG2PnXhcv/H0K+DS0RllGetsfr42gAt3W9Y6zr9i
WAY66Sx3Ldkiv+urK5mI5yZmrgE3r3Fe8yyL+eGsFda3PMdmPmG2yt2FCXeaaVYyl8wBjx35imWf
pQUxS+dS7IxNZ35eNoDBzERAlT+//8o+NMEQBM45wLwFR7GPuyzoSitFVFjn2fkWZ2Qtw/HVWCNd
0oRV482L+mDC/G8DX14l+t123HgHjniPtxzoVl2ApYs660w75xjsi3DuVxN+YsYfz6KoVws0QkF0
sY2F7BIMI5juCA+u4iGwUJRKB1XVyTzVKnxgufXOcOJA9nKhXfWtKGqlh1p5kp0I6g7GZTJFkDo2
+JfiN+K8KE9FcRipI5zFjo85EaCeKrBTwHlzDSeYwX1zeMVEDvOhYxVOSgMV1dkTTewGfRBI7tKr
iF37h1OM4yNzWc6M1opXDlWsjQKpI4cVQDKaVuMGV3DdEu7gOJm4nmiU+qmp9jrLWjzoAB50TM2r
BARh+CBs8IQ9/GIjl3nRsyoHpXw6qrpOseYidtvBfmfiitgmzVNPpXWOVwWT2MU9ZrKuDmiDXiiJ
tihjdUCoGot3HPmX69zoyXc1rQ4aWBNdbI42Ni/87bsBF7GMlZxmSN9CLJPaqaa6Kq8edocDnRGl
Xc6Xm81gyjd8uIyLXOU2pNbKB6NBuqRTmgVMYFllpYxq62Q78QQ/cOLPrJqgiJ0S60Q76cQqHxX9
0nNC9isRgVZFz8P+iwtsrKxbosY6BZxwywxziVxaHZZK5Wkmd6w4pj70iKwbFwpIOKX06uGxSpj7
EUrslVcL9H4MmvKEobo6A5xuTVx7YKTXDl1RHSXSI1iGE+LIzFdcZDEnimsiqvMTzdCwGldFynPs
NPCgadv50YY46oPBv/JKeOORUNGyCtfHTIOLHyezIrmi8NNS5XWO1c2ZVCsEu0XmFz7w3HsPngJM
YquUUFedYSebIN8bJKdUUFOdyzOtuV0DLGLH5F01yADSnK49xCrOcZK9TN5rsImt0R8t0mEXOy75
oV11QQ3rpHw62WAnmtiIIo6x6FhV0BJVUjv1FDa23bz2YGPIOCsi7IrmaJEeORfwg3fd4UQsiCb1
rqoVrx3Kojvq2CQXGt/CKO2kowDhcB61zrWTAdyUjNrK1xCQcSgUtXXPRSXAYdBCWbcstM5/37fC
CRbuFOwlg7xhGHrLhfmnd6yxfDVuJ0mWr5INJsmmh/fwXOv6cDaFdQp+eG5ZSqleYYBGdkaZdgnV
fmHNbsOGjWY+Q2qGtjkjnAuWlAXwkO2ucOGOd9AcPNapJDgieXk4pjSVuqli2XTUOfeWbhPA28I0
hnIdtmqYnDsLw1577LPjnntwxo8c5lntK2HnFFNxnXJnlrb3ANqQw2pgeYtlSuk0cBoOLG7ylOns
qWpt62GWogHEvHndaKtR2MREPvOmnkEpqVPtLDytMJ9ToLeFFU/ym8U+qaez7fw7ezAv80M4opn5
zGnKNkuhhu1RLa3TSm9bOhDIUV6zoDXNq2wFzPGlbcyuMIG8B8f0qoUiOXCyOKOIrX3Aw4tx0KoA
ni+at533pi0wjuQw47nSr0rWvRq2sVSapoL6mSnELMzsR1fTOqGH/dEm/VI2ZJ+xEQ5jJZ0aYEoj
nc1zwok4VqBuxkxEibB0Ms+9MwRTbk8khmylVF8LA4GtqHFkogUlE1CYsCBJoQOhORGJimiuDnIT
LlECRQwxAiIoAiL8fyL0WQMDjQCbVTNRtJHBs1KBZbdM0OiiwShx0hHMURxHUMIkObVHLNZScdNz
Kpsb7xVG+CV++7giKYa8TRRWJqLQLZAmiox2R4K/Qi0D2ZGZ3558A0HgU7OPVPnhqfuD5ac3NGQ/
GiNEk5NtWg6IbEiGXBCpeXHIbCbulESOjfDe+IKAD0I35sQFgQ8ySlIzMmQ8pyHaRZusClt2B+2U
kP8nzmrGyOsR1KnmyUM2p34PBXN7owLCTmKz5RnxQ0xQndF/NFtW8n/RQ/xxow6Iqkau+chtcFA/
T5xMv2fLrGnxkTdvmGRxwvNqOyaleufPO2GzinopT8Gne9KwiZmlbNFDBL9oGptyFA7Iha4pnE7K
zRGiOAck8ySofg8BdCifpGHCPxIQFpo/H41UeY0NmVhi8iVHZSodvjPO6pyhaVUoT2BS0hMWTkZj
Q0TxjHOLyQ8EO6kUeWk5JhwsvuIN/xE8lWqiaM52nkbKaP9+yDaHiGLaNjWfcPzOHLPzGSbhOSkv
sxMHJHcHFDb/hr/IfZ2CenuvZC6iKAakQhp3ycQwsAerunxWyhHXmEwBwVd9ooFD5Jn71iFw3hQ8
1wa7q3bOzm8MCFmQldDFxPekXk8PxvQ//IxomDCMYwx1O8CCV6FXOGUpmPNnfRdokMpGu5kmFujT
ACZgzA4KqmoPjcQWTmJkzRATDzwTdMVBTYo4wZBT8c29DqTwUGgi2mQyTDNxlcJmPwXGZUcGHeWq
SIeTRcLq4Tw6KLSfIhL+RDnhVoWXMpWPKQ1ZsGw3vDUbgU6QbF9BkBaBmUbnPZRFS4sU7qYN4G41
xJyuWkFNogONxET6ROsapyLL4N3/hw2ZqmhVkZeyOM+jqQ7ZRYKh2edTkgLKG321KGLSKkeUlYgc
3NxDkF2GypTuDkke0ngQm8MTWk/TgbYri2U=
```

Changing the tables (retraining with aqr-flac's `flr-train`) changes the encoder's
resource and needs new versions of both modules.
