# psd-recons format

psd-recons stores a Photoshop file (PSD or PSB), the *original*, in a smaller form from which
it is rebuilt bit for bit. This document specifies that form completely enough to reimplement
both directions.

The 8-bit pixel data of layer channels and of the composite image (PackBits or raw) is taken
out of the file and coded by a pixel model with rANS. The PackBits rows are rebuilt by an
encoder identified for each channel, with the rows it does not reproduce stored verbatim. Files
embedded in smart objects are coded by their own recompressors. The rest of the file, the
*skeleton*, is compressed with zstd.

Sections 1–9 are normative for decoders. Section 10 describes the reference compressor: what it
chooses is free within the constraints of sections 1–9, and a decoder relies only on those and
on the checksum.

Other formats this one embeds are specified elsewhere:

- zstd frames: RFC 8878.
- png-recons containers: png-recons' `FORMAT.md`.
- jpeg-recons containers: jpeg-recons' `FORMAT.md`.
- DEFLATE (ZIP channels, read when rendering the hint): RFC 1951.
- XXH3-64: the xxHash specification.

## Contents

1. [Conventions](#1-conventions)
2. [Container](#2-container)
3. [Coded containers](#3-coded-containers-version-1)
4. [Pixel data of spans](#4-pixel-data-of-spans)
5. [Model stream](#5-model-stream)
6. [The pixel model](#6-the-pixel-model)
7. [PSD structure](#7-psd-structure)
8. [The hint](#8-the-hint)
9. [ZIP channels](#9-zip-channels)
10. [Compressor](#10-compressor)
11. [Appendix: level weights](#appendix-level-weights)

## 1. Conventions

- Integers are unsigned unless stated. `u16` and `u64` fields of this format are little
  endian. Fields of the PSD file itself are big endian (BE).
- **varint**: unsigned LEB128. 7 bits per byte, least significant group first, bit 7 set on
  every byte but the last. Readers accept at most 10 bytes (a 10th byte with bit 7 set is an
  error) and drop bits beyond bit 63. Writers emit the shortest form.
- **opt** is a varint holding *none* as 0 and a value `v` as `v + 1`.
- **Bit streams** are read least significant bit first: bit `i` of a byte string is
  `(byte[i div 8] >> (i mod 8)) & 1`. An `n`-bit field read at bit position `p` has the value
  `Σ bit(p + j) << j` for `j` in `0 .. n`. Writers pad the last byte with zero bits.
- **gamma** (in a bit stream) is Elias gamma of `v + 1` for a value `v ≥ 0`: with
  `k = floor(log2(v + 1))`, `k` zero bits, a one bit, then the low `k` bits of `v + 1` as a
  `k`-bit field. For example, gamma(5) is the bits 0, 0, 1, 0, 1 in that order, the byte
  `0x14` on its own. A reader counts the zero bits `k`; more than 32 is an error. The decoded
  value `2^k + field − 1` must be below 2^32.
- `x div y` and `x mod y` are integer division and remainder of non-negative integers, and
  `ceil(x / y)` rounds up. `bitlen(x)` is the number of significant bits of `x ≥ 0`
  (`bitlen(0) = 0`, `bitlen(5) = 3`).
- Arithmetic on samples, residuals and symbols of the pixel model (section 6) is mod 256.
  `sx(v)` reads a byte `v` as a signed 8-bit value (−128 .. 127), and `mag(v) = |sx(v)|`
  (0 .. 128).
- **xxh3** is XXH3-64 (`XXH3_64bits`) with seed 0 and the default secret.
- `a[i .. j]` is the elements `i` to `j − 1`. Ranges `[a, b)` are empty when `a ≥ b`.
- *Invalid* means that decompression fails. A decoder must not output anything for an invalid
  container. Which error is reported when there are several is not specified.

## 2. Container

```text
offset 0   6 bytes   magic 69 04 7f fe 9b 1d        ("aQR//psd" in base64)
offset 6   u16       version
offset 8   streams:  stream 0, stream 1, ..., stream N-1
stream  =  varint length, then `length` bytes
```

| version | layout |
|---|---|
| 0 | stored (below) |
| 1 | coded (section 3) |
| other | invalid |

Stream 0 is the main stream. The number of streams and the meaning of stream *n* follow from
the version and the contents of streams 0 .. *n* − 1. A container ends exactly after its last
stream. Further bytes, a missing stream, or a stream extending past the end make the container
invalid.

**Stored** (version 0): one stream, the original compressed as one or more zstd frames (RFC
8878; skippable frames are allowed). The original is the concatenated content of the frames.

**zstd frames** (here and in the skeleton): the reference decoder is libzstd with its default
limits, which reject frames whose window size exceeds 2^27 bytes. Writers must not produce such
frames.

## 3. Coded containers (version 1)

### 3.1 Streams

| # | stream | |
|---|---|---|
| 0 | head (3.2) | always |
| 1 | skeleton (3.5) | always |
| 2 … | embedded file (3.6) | one per span of kind 16, 17 or 18, in span order |
| last | model stream (section 5) | always |

### 3.2 Head

```text
u64     xxh3 of the original
varint  total: length of the original, at most 2^42
varint  number of spans
per span (3.3)
varint  number of groups
per group (3.4)
```

The fields must use up the head stream exactly.

### 3.3 Spans

A *span* is a byte range of the original that is not in the skeleton: the pixel data of a layer
channel or of the composite image (without its 2-byte compression field), or an embedded file.

```text
varint  gap      bytes of the skeleton between the previous span (or the file start) and this one
varint  len      length of the span
u8      kind
kind 0 (raw):          varint planes, varint w, varint h
kind 2, 4 (PackBits):  varint planes, varint w, varint h, encoder, varint plane_len × planes,
                       varint n_exceptions, exceptions
kind 16, 17, 18:       nothing (an embedded PSD/PSB, PNG or JPEG file)
kind 32 (copy):        varint of
```

The spans lie in file order and do not overlap: span `i` is `[start_i, end_i)` with
`start_i = end_(i−1) + gap` (`end_(−1) = 0`) and `end_i = start_i + len`, and
`end_i ≤ total` (arithmetic overflow is invalid).

A *pixel span* (kind 0, 2 or 4) holds `planes` planes of `h` rows of `w` samples. It must have
`1 ≤ planes ≤ 64` and `w, h ≥ 1` (so `size = planes · w · h > 0`, and it must not overflow).

- **Raw** (kind 0): `len = size`. The planes follow each other, each `h` rows of `w` bytes.
- **PackBits** (kind 2 or 4): the kind is the size of the row byte counts (2 in PSD, 4 in PSB),
  `cb`. With `rows = planes · h`, `len = rows · cb + Σ plane_len` (no overflow). Section 4
  gives the layout and the encoder field.

  Exceptions are rows stored verbatim, listed by row index `r = p · h + y` (plane `p`, row
  `y`), increasing:

  ```text
  per exception:  varint dr, varint n, n bytes (the encoded row)
  ```

  The first exception's index is `dr`, each further one's the previous index plus `dr`, and
  `dr` must be at least 1 after the first. Every index must be below `rows`.

- **Embedded file** (kinds 16, 17, 18): an embedded PSD/PSB (16), PNG (17) or JPEG (18) file,
  coded in a stream of its own (3.6).
- **Copy** (kind 32): the same bytes as span `of`, which must be an earlier span (`of < i`) of
  kind 0, 2 or 4 with one plane and `len` equal to this span's. The copy has one plane and the
  `w` and `h` of its source.

### 3.4 Groups

A *group* is a set of planes coded together, row by row (section 6).

```text
varint  n            number of planes, 1 .. 64
u8      flags        bit 0: has_alpha; bit 1: hinted; other bits must be 0
per plane k = 0 .. n-1:
  varint  s          span index
  varint  p          plane index in the span
  opt     same_as    a plane index j of this group, or none
```

With `has_alpha`, `n ≥ 2` and plane `n − 1` is the alpha plane. The *color planes* are planes
`0 .. colors − 1`, `colors = n − has_alpha`. Within a row, planes are coded in *coding order*:
the alpha plane first (if any), then the color planes in order. `rank(k)` is the position of
plane `k` in that order.

Constraints:

- Span `s` exists, is a pixel span or a copy, and `p < planes` of span `s`. All planes of a
  group have the `w` and `h` of the first plane's span.
- No `(s, p)` appears twice in a group.
- `same_as = j` requires `j < n`, `rank(j) < rank(k)`, span `s` a copy, and its `of` equal to
  the span of plane `j`. (`j > k` is possible: the alpha plane is last by index but first in
  coding order.)
- `hinted` requires `n ≥ 3` and all planes in one span.
- Over all groups, every plane `(s, p)` of every pixel span appears in exactly one group. Copies
  may appear in any number of groups, or none.

A plane is

- *known* if its span is a copy and it has no `same_as`: its rows are read from the copy's
  source (which must be complete, see 3.7);
- a *repeat* if it has `same_as = j`: its rows are those of plane `j`;
- *coded* otherwise.

### 3.5 Skeleton

The skeleton stream is one or more zstd frames. Their concatenated content must be exactly
`total − Σ len` bytes: the bytes of the original outside the spans, in order. It fills the
gaps between the spans.

### 3.6 Embedded files

The stream of a span of kind 16 holds a complete psd-recons container (magic, version,
streams; any version) at depth + 1, kind 17 a png-recons container, kind 18 a jpeg-recons
container. Decompressing it must give exactly `len` bytes, the span's bytes.

*Depth*: the outermost container has depth 0, and a container in a kind 16 stream its parent's
depth plus 1. A container of depth 3 or more must have no spans of kinds 16, 17 or 18.

### 3.7 Decompression

1. Read the head and check the constraints of 3.2–3.4. Read the skeleton stream, one stream per
   embedded file and the model stream, and check that the container ends there.
2. Allocate the output of `total` bytes. Fill the gaps with the skeleton (3.5). Decode the
   embedded files into their spans (3.6). Embedded files and pixel spans are *done* when their
   bytes are written; copies are done when their source is (step 3).
3. Open the model stream (section 5). For each group in order:
   1. `split` is the smallest `start` of the spans of its planes that are not copies (`total`
      if there is none). A known plane's source span must be done and end at or before
      `split`.
   2. If the group is hinted, every span with an index below that of the group's span must be
      done. The hint renderer is built from the output's first `split` bytes (section 8).
   3. Decode the group's rows (section 6). After each row, write the rows of the planes that
      are not copies into their spans (section 4). For PackBits spans, the rows of each plane
      must fill its `plane_len` bytes exactly.
   4. A pixel span is done when all of its planes are written. Then every copy whose source is
      done is done: its bytes are the source's.
4. Every span must be done, and the model stream used up exactly (section 5).
5. If xxh3 of the output differs from the head's, the container is invalid.

Steps 2 and 3 may overlap, and embedded files may be decoded on other threads, as long as each
step sees what it reads complete. The output does not depend on the order.

## 4. Pixel data of spans

### 4.1 Layout

A raw span holds its planes one after the other, each `h` rows of `w` bytes; row `y` of plane
`p` is at `p · w · h + y · w`.

A PackBits span holds the row byte counts of all its rows, `rows · cb` bytes, then the encoded
rows, all in row index order `r = p · h + y`. Each count is a `cb`-byte BE integer, the length
of the encoded row. Plane `p`'s encoded rows take `plane_len[p]` bytes, starting
`rows · cb + Σ_{q<p} plane_len[q]` bytes into the span.

Writing row `y` of plane `p` (with decoded samples `row`, `w` bytes):

- raw: the samples.
- PackBits: if `r` is the index of an exception, its bytes, otherwise `encode(encoder, row)`
  (4.3). The row's count must be below `2^(8·cb)`. The bytes must not pass the end of the
  plane's `plane_len` bytes.

### 4.2 PackBits

An encoded row is a sequence of tokens. Token header byte `t`:

- `t ≤ 127`: a literal of the next `t + 1` bytes.
- `t = 128`: no operation (one byte).
- `t ≥ 129`: a run of `257 − t` (2 .. 128) copies of the next byte.

*Decoding* a row (where a known plane's source or the hint renderer reads one) must give
exactly `w` bytes; a token cut short by the end of the row is an error.

The encoders below produce *literal(s)* (header `len(s) − 1`, then the bytes, `1 ≤ len(s) ≤
128`) and *run(k, v)* (header `257 − k`, then `v`, `2 ≤ k ≤ 128`).

### 4.3 Encoders

```text
encoder:
  u8 0:  opt phase, opt period      Photoshop; period must not be 0 (none means infinity)
  u8 1:  varint min_run             Greedy, split_rest = false
  u8 2:  varint min_run             Greedy, split_rest = true
```

`min_run` must be in 1 .. 128. Other leading bytes are invalid.

Helpers on a byte string `b` of length `n`:

- `runlen(b, i)`: the number of bytes from `i` on equal to `b[i]` (at least 1).
- `next_run(b, i, end, k)`: the smallest `j` with `i ≤ j < end`, `j + k ≤ n` and
  `b[j] = b[j+1] = … = b[j+k−1]`; `end` if there is none.

**Photoshop** `{phase, period}`: the row is cut into *pieces* at the positions `phase`,
`phase + period`, `phase + 2·period`, … (an infinite value cuts nowhere; `phase = 0` gives an
empty first piece; that is `opt` value 1, while `opt` 0 is infinity). Each piece is cut into *chunks* of 128 bytes from its start (the last one
shorter). Each chunk `c` (`n = len(c)`) is encoded on its own:

```text
i = 0
while i < n:
  r = runlen(c, i)
  if r >= 2:  run(r, c[i]);  i += r
  else:       s = i;  i = next_run(c, i + 1, n, 3);  literal(c[s .. i])
```

**Greedy** `{min_run = m, split_rest}` over the whole row `b` (`n = len(b)`), with
`starts_run(i) = (i + m ≤ n and b[i] = b[i+1] = … = b[i+m−1])`:

```text
i = 0
while i < n:
  if starts_run(i):
    v = b[i];  r = runlen(b, i);  if not split_rest: r = min(r, 128)
    while r > 0:
      k = min(r, 128)
      if k == 1: literal([v]) else: run(k, v)
      i += k;  r -= k
  else:
    s = i
    i = the smallest j with s < j < min(s + 128, n) and starts_run(j), else min(s + 128, n)
    literal(b[s .. i])
```

(Without `split_rest`, the rest of a run longer than 128 is coded from scratch: a run if it
starts one, else the start of a literal.)

## 5. Model stream

The model stream is a sequence of *chunks* that ends with the stream (an empty stream has no
chunks):

```text
varint  n             number of symbols, at least 1
varint  head length
bytes   head          table section (5.1)
varint  body length
bytes   body          rANS data (5.2)
```

The pixel model (section 6) decodes symbols one at a time, each with a *table index*
`t ∈ 0 .. 401`. The first `n` symbols come from the first chunk, the next from the second, and
so on. Before a chunk's successor is opened, its rANS data must be *finished* (5.2). At the end
(3.7 step 4) the last chunk must be finished and no chunk may remain. A symbol needed after the
last chunk makes the container invalid.

### 5.1 Tables

The head is a bit stream (section 1). For each table `t = 0 .. 401` in order:

- 1 bit *used*.
- If used, a table of levels for the 256 symbols:

  ```text
  prev = 0;  i = 0
  while i < 256:
    z = gamma
    if z == 0:  run = gamma + 1;  i + run must be <= 256;  levels[i .. i+run] = 0;  i += run
    else:       z -= 1;  d = (z odd) ? (z + 1) / 2 : −(z / 2)
                l = prev + d, which must be in 1 .. 63;  levels[i] = l;  prev = l;  i += 1
  ```

Reading past the end of the head is an error. Bits after the last table are ignored.

**Frequencies** from levels (`W` is the [level weight table](#appendix-level-weights),
`SCALE = 2048`):

```text
if no level is nonzero:  f[0] = 2048, all other f = 0
else:
  sum = Σ W[levels[s]]                             (64-bit)
  f[s] = max(1, (W[levels[s]] · 2048) div sum)     for levels[s] > 0;  f[s] = 0 otherwise
  tot = Σ f
  while tot != 2048:
    m = the symbol with the largest f (the smallest such symbol on ties)
    if tot < 2048:  f[m] += 2048 − tot
    else:           d = min(tot − 2048, f[m] − 1);  if d == 0: invalid;  f[m] -= d
    tot = Σ f
```

An unused table has `f[0] = 2048` and all other `f = 0`. `c[s] = Σ_{u<s} f[u]` are the
cumulative frequencies.

### 5.2 rANS

The body is read as 16-bit little-endian words `w[0], w[1], …`. Reading a word past the end of
the body gives 0 (and still advances the position). The decoder has four 32-bit states:

```text
st[k] = w[2k] << 16 | w[2k+1]   for k = 0 .. 3;   pos = 8
```

To decode a symbol of table `t`:

```text
x    = st[0]
slot = x & 2047
s    = the symbol with c[s] <= slot < c[s] + f[s]
x    = f[s] · (x >> 11) + slot − c[s]
if x < 2^15:  x = x << 16 | w[pos];  pos += 1
st   = [st[1], st[2], st[3], x]
```

The chunk is *finished* when `pos` is exactly the number of words in the body (an odd body
length can never finish) and all four states equal `2^15`.

**Encoding** (reference; any stream that decodes the same is valid): symbols are encoded from
the last to the first, symbol `j` (counting from 0 in the chunk) with state `j mod 4`. All
states start at `2^15`. To encode a symbol with frequency `f` and cumulative frequency `c` into
state `x`:

```text
if x >= f << 20:  emit the word x & 0xffff;  x >>= 16
x = (x div f) · 2048 + (x mod f) + c
```

The body is `st[0] >> 16, st[0] & 0xffff, st[1] >> 16, …, st[3] & 0xffff` (the final states),
then the emitted words in reverse order of emission. The encoder must give every coded symbol a
nonzero frequency.

## 6. The pixel model

### 6.1 Group state

A group of `n` planes of `w` samples per row and `h` rows is coded row by row, `y = 0 .. h−1`.
Per plane `k` it keeps the previous row `up_k`, the previous row's residuals `rup_k` and the
previous row of the hint `hup_k`, all initially zero. `top` is true for row 0 only.

Per row:

- Known planes have their row read from their source (decoding PackBits rows, 4.2).
- If the group is hinted, the hint row `hc_k` of every plane: the renderer's row (section 8),
  R, G, B and alpha for planes 0, 1, 2, 3 of the group (by plane index in the group, whatever
  the planes are; a group of 4 planes without alpha still gets the alpha hint in plane 3);
  zero for planes 4 and up.
- The row is coded (6.6 if `colors = 3`, else 6.5), filling the rows `cur_k` of the coded and
  repeat planes and the residual rows `res_k`.
- Then for every plane: `up_k = cur_k`, `rup_k = res_k`, `hup_k = hc_k`.

Residual rows `res_k` are overwritten only where the procedures below write them. A plane whose
residuals are not written in a row keeps the values of an earlier row; such values are never
read.

Below, `cur` is the plane's row being coded, `res` its residual row, `up`, `rup` its previous
rows, and for a hinted group `hc`, `hp` its hint's current and previous rows. In a group with
alpha, `al` and `alp` are the alpha plane's current row (complete before the color planes are
coded) and previous row.

### 6.2 Tables

| tables | index |
|---|---|
| main | `kind · 64 + tr · 32 + ctx` (0 .. 255) |
| run | `256 + kind · 16 + tr · 8 + rc` (256 .. 319) |
| match | `320 + kind · 4 + mm` (320 .. 335) |
| hint run | `336 + u` (336, 337) |
| joint | `338 + tr · 32 + ctx` (338 .. 401) |

`kind` is the plane's kind: 0 the first color plane, 1 another color plane, 2 the alpha plane,
3 a lone plane (a group of one plane without alpha). `tr` is 1 for a transparent pixel (6.3),
`ctx` 0 .. 31 and `rc` 0 .. 7 are contexts.

### 6.3 One sample

A *coder* either decodes a symbol of a table (the decoder) or gives the symbol of the known
sample (when a plane is *walked*: a known or repeat plane whose residuals are needed, or the
encoder). Walking does not use the model stream: a walked plane goes through exactly the
procedure below with its samples known, as the encoder does, which gives its residuals
(`res[x] = cur[x] − pred` outside runs) and its runs.

Sample `x` of a row is coded with the flags `L = (x > 0)`, `R = (x + 1 < w)`, `T = top`, and,
for color planes of a group with alpha, `vis`:

```text
vis = 1  if al[x] == 0                                         (transparent)
      2  else if (L and al[x−1] == 0) or (not T and (alp[x] == 0 or (L and alp[x−1] == 0)))
      0  otherwise
```

Without alpha (and for the alpha plane itself) `vis = 0`. `tr = 1` if `vis = 1`.

**Neighbours and prediction:**

```text
a = L ? cur[x−1] : (not T ? up[x] : 0)
b = not T ? up[x] : a
c = (L and not T) ? up[x−1] : b
d = (R and not T) ? up[x+1] : b
if vis == 2:
  jw  = cur[x−1]  if L and al[x−1] > 0             (else absent)
  jn  = up[x]     if not T and alp[x] > 0
  jnw = up[x−1]   if L and not T and alp[x−1] > 0
  if any of jw, jn, jnw is present:
    fb = the first present of jw, jn, jnw
    a = jw if present else fb;  b = jn if present else fb;  c = jnw if present else fb
pred = med(a, b, c)
med(a, b, c) = min(a, b) if c >= max(a, b);  max(a, b) if c <= min(a, b);  a + b − c otherwise
```

(`d` is not replaced. `med` compares the bytes as unsigned values.)

**Hint match**: in a hinted group, if `L and not T and cur[x−1] == hc[x−1] and up[x] == hp[x]`:

```text
mm  = (up[x−1] == hp[x−1]) + 2 · (R and up[x+1] == hp[x+1])
sym = code(match table (kind, mm), cur[x] − hc[x])
cur[x] = hc[x] + sym;  res[x] = cur[x] − pred
```

(`pred` is the prediction above, with the alpha replacement.) Otherwise:

```text
rw   = L ? mag(res[x−1]) : 0
rn   = not T ? mag(rup[x]) : 0
act  = |a − c| + |b − c| + |b − d|               (differences of the bytes as integers)
e    = act + 2 · (rw + rn)
off  = f (the first color plane's residual res_0[x]) for kind 1, else 0
cross = kind 1 ? bucket(mag(f), 4) : 0
ctx  = bucket(e, 8) · 4 + cross
sym  = code(main table (kind, tr, ctx), cur[x] − pred − off)
res[x] = sym + off;  cur[x] = pred + res[x]
bucket(v, n) = min(bitlen(v), n − 1)
```

`code(table, s)` is the symbol: the decoder's decoded symbol, or `s` when walking or encoding.

### 6.4 Runs

A run codes a length `N` of samples from `x` on in chunks: `N = 0`, then repeatedly

```text
s = min(code(table, avail), w − x − N);  N += s;  stop unless s == 255
```

where the encoder's `avail` is the number of matching samples from `x + N`, at most 255 and at
most `w − x − N` (so the encoder codes exactly the length). A walked run uses `avail` too: its
length is the full number of matching samples from `x` (up to `w − x`). Decoded symbols above the room left
are cut to it, and a cut symbol ends the run.

The positions `x = 1 .. w − 2` (`x + 1 < w`) of a row may start runs; position 0 and the last
position never do. A run is not tried on the first row, nor at the position right after a run
(even a run of length 0). After a run of length `N`, its samples get residual 0 in `res` of
the planes involved, and coding resumes at `x + N`.

`rc(m) = min(floor(log2 m), 7)` for `m ≥ 1` (0 for `m = 0`), and `stretch(up, x, v)` is the
number of samples of `up[x .. min(x + 128, w)]` equal to `v`, counted from `x` until the first
that differs.

### 6.5 Rows of groups without three color planes

The planes are visited in coding order. For each plane `k`:

1. If `same_as = j`, its row is `cur_j` (complete).
2. `kind` = 3 if the group is a single plane without alpha; else 2 for the alpha plane, 0 for
   plane 0 and 1 for the other color planes.
3. A coded plane is coded with the model stream. Plane 0 of a group with `colors > 1` that is
   not coded is walked. Other planes are skipped.

A row of a plane (coded or walked) is coded as:

```text
x = 0
code sample 0
x = 1;  after_run = false
while x + 1 < w:
  if not top and not after_run and up[x−1] == up[x] == up[x+1] == up[x+2] == up[x+3] == cur[x−1]
     (all five exist: x + 4 <= w):
    v = cur[x−1]
    table = run table (kind, tr = (alpha present and al[x] == 0), rc(stretch(up, x, v)))
    code a run (6.4) of samples equal to v; the decoder sets them to v
    res[x .. x+N] = 0;  x += N;  after_run = true;  continue
  after_run = false
  code sample x;  x += 1
if x + 1 == w:  code sample x
```

For `w = 1` that is sample 0 alone. The alpha (`al`, `alp`) of a color plane is present when the
group has alpha; the alpha plane itself and the planes of groups without alpha have no alpha
(`vis = 0`, and `tr = 0` for runs). The hint applies to every plane through the hint match of 6.3 (there are no
hint runs here).

### 6.6 Rows of groups with three color planes

`colors = 3` (`n = 3`, or `n = 4` with alpha). First the alpha plane, if any, is coded (if
coded, else walked) as in 6.5 with kind 2. Then, if plane 0 is a repeat (of the alpha plane),
its row is copied. Each color plane gets `how`:

- 1 if coded;
- 2 if plane 0 (when not coded), or a known plane;
- 0 for a repeat plane 1 or 2 (its row is copied after the color planes).

Plane 0 has kind 0, planes 1 and 2 kind 1. The color planes are coded pixel by pixel:

```text
pixel(x):
  vis from al, alp (6.3), shared by the three planes
  plane 0: code sample x, with the model if how[0] == 1, else walked
  f = res_0[x]
  if not hinted and how[1] == 1 and how[2] == 1:  pair(x)
  else:  for k in 1, 2 with how[k] == 1: code sample x of plane k (kind 1, f)

pixel(0)
x = 1;  after_run = false
while x + 1 < w:
  P = the planes k in 0 .. 2 with k == 0 or how[k] != 0
  if hinted and not top and not after_run
     and for every k in P: cur_k[x−1] == hc_k[x−1] and up_k[x] == hp_k[x]:
    table = hint run table 336 + (up_0[x+1] == hp_0[x+1])
    code a run (6.4) of pixels where every plane of P equals its hint;
      the decoder copies hc_k into the planes with how[k] == 1
    res_0, res_1, res_2 [x .. x+N] = 0;  x += N;  after_run = true;  continue
  if not top and not after_run and x + 4 <= w
     and for every k in P: up_k[x−1] == up_k[x] == up_k[x+1] == up_k[x+2] == up_k[x+3]
                           and cur_k[x−1] == up_k[x−1]:
    v_k = cur_k[x−1]
    table = run table (kind 0, tr = (alpha and al[x] == 0), rc(stretch(up_0, x, v_0)))
    code a run (6.4) of pixels where every plane of P equals v_k;
      the decoder sets the planes with how[k] == 1 to v_k
    res_0, res_1, res_2 [x .. x+N] = 0;  x += N;  after_run = true;  continue
  after_run = false
  pixel(x);  x += 1
if x + 1 == w:  pixel(x)
```

If no color plane has `how = 1`, the color planes are walked and use no symbols (nothing
needs them then, so a decoder may skip them). Finally the
repeat planes 1 and 2 copy the rows of their `same_as` planes.

**Pair** (planes 1 and 2 at `x`, with `f = res_0[x]` and `cross = bucket(mag(f), 4)`):

```text
for i in 1, 2:  (pred_i, e_i) as in 6.3 for plane i (with the alpha replacement)
t_i = main table (kind 1, tr, bucket(e_i, 8) · 4 + cross)
tj  = joint table (tr, bucket((e_1 + e_2) div 2, 8) · 4 + cross)
s_i = cur_i[x] − pred_i − f
j   = (sx(s_1) + 2) · 5 + (sx(s_2) + 2)   if both sx(s_i) in −2 .. 2,  else 25
j   = code(tj, j)
if j < 25:  s_1 = j div 5 − 2;  s_2 = j mod 5 − 2
else:       s_1 = code(t_1, s_1);  s_2 = code(t_2, s_2)
res_i[x] = s_i + f;  cur_i[x] = pred_i + res_i[x]
```

Decoded joint symbols 25 .. 255 are escapes.

## 7. PSD structure

A hinted group renders the composite from the layers of the output's first `split` bytes,
the *prefix*. This section gives how the prefix is parsed; any error makes the container
invalid. Fields are BE. A *window* is a byte range of the prefix; reading past its end is an
error.

### 7.1 File

```text
4   "8BPS"
u16 version: 1 (PSD) or 2 (PSB); psb = (version == 2)
6   reserved
u16 channels
u32 height, u32 width
u16 depth, u16 mode
u32 n, then n bytes          color mode data
u32 n, then n bytes          image resources: window R (7.2)
len n, then n bytes          layer and mask information: window M (7.3)
at least 2 bytes             the composite's compression field (and data)
```

`len` is u32 in PSD and u64 in PSB. The hint requires `depth = 8`, `mode = 3` (RGB), `width = w`
and `height = h` of the group, and `split − (end of M) ≥ 2`.

### 7.2 Image resources

While at least 12 bytes of R remain, at position `p`:

```text
4 bytes signature, u16 id
u8 nl, then nl + ((nl + 1) mod 2) bytes
u32 dl
```

With `q0` the position after `dl` and `next = q0 + dl + (dl mod 2)`, the resource ends at the
first `e` of `next, next − 1, next − 2, next + 1, next + 2` with `e ≥ q0` and either `e` = the
end of R or the four bytes of the prefix at `e` are "8BIM"; if there is none, at `next`. The
reader moves to `e`, which must not be past the end of R.

### 7.3 Layer and mask information

If M is empty there are no layers. Otherwise:

```text
len ln, then window LI of ln bytes      layer info (7.4)
if at least 4 bytes of M remain:  u32 gl, then min(gl, rest of M) bytes
tagged blocks (7.5) to the end of M     the global blocks
```

If the layer info has no layers and a global block has the key "Layr", "Lr16" or "Lr32", the
layers are the layer info (7.4) in the data of the first such block (that data is the
window).

### 7.4 Layer info

If the window is empty there are no layers. Otherwise:

```text
i16 count;  |count| layer records, then the channel data
layer record:
  i32 top, left, bottom, right
  u16 nc;  nc × (i16 channel id, len channel data length)
  4   "8BIM" (otherwise an error)
  4   blend mode key
  u8  opacity, u8 clipping, u8 flags, u8 filler
  u32 el, then window E of el bytes:
    u32 ml, then window K of ml bytes (the mask data):
      if ml >= 18:  i32 top, left, bottom, right; u8 default_color; u8 flags  (the user mask)
        and if ml >= 36 and a channel has id −3: 2 bytes skipped, then i32 × 4 (the real mask rect)
    u32 bl, then bl bytes
    u8 nl, nl bytes (the name), then min(pad, rest of E) bytes,
      pad = (the multiple of 4 at or above 1 + nl) − 1 − nl
    tagged blocks (7.5) to the end of E
channel data: for each layer, for each channel in order, its length in bytes (must fit LI)
```

A layer's width and height are `max(0, right − left)` and `max(0, bottom − top)` of its
rectangle. Each channel has the layer's size, except channel −2 (the user mask) which has the
user mask's size, when there is a user mask, and channel −3, which has the real mask rect's
size when that was read. A channel's *compression* is the BE u16 of its first two data bytes
(none if it has fewer than 2 bytes): 0 raw, 1 PackBits, 2 ZIP, 3 ZIP with prediction.

A layer is *hidden* if `flags & 2`. Its user mask is *enabled* if `mask flags & 2 = 0`.

### 7.5 Tagged blocks

Blocks are read in a window `[base, end)`, starting at a position `p` in it. The window is
E for a layer's blocks (`base` = the first byte after `el`, `p` = the first byte after the
name's padding) and M for the global blocks (`base` = the first byte after M's length, `p` =
the first byte after the global mask). Alignment is relative to `base`, not to `p`.

At position `p`, look for the next block at the first of `p` rounded up (relative to `base`)
to a multiple of 1, 2 and 4 (so `p` itself first), where the four bytes are "8BIM" or "8B64"
within the window. If there is none, the blocks end. Otherwise:

```text
4 signature, 4 key
length: u64 if psb and the key is one of LMsk Lr16 Lr32 Layr Mt16 Mt32 Mtrn Alph FMsk lnk2
        FEid FXid PxSD; u32 otherwise
data (must fit the window)
```

then continue after the data. Blocks the renderer reads:

- `lsct`: the section type, the BE u32 of the four prefix bytes at the start of the data
  (0 if they are not all in the prefix). 1 and 2 are a group's folder record, 3 its divider.
- `iOpa`: the fill opacity, the prefix byte at the start of the data (255 if there is none).

When a key occurs more than once, the first block counts.

## 8. The hint

The renderer composites the layers row by row on canvases of `w` (the document width) RGBA
pixels of 8 bits, *straight* (not premultiplied) color. Floating point is IEEE 754 binary32
with round-to-nearest-even, each operation rounded on its own (no fused multiply-add),
evaluated in the order written; `round` rounds half away from zero; `min` and `max` of a NaN
and a number give the number; converting a float to a byte clamps to 0 .. 255 after rounding,
NaN giving 0. Float literals are binary32 (`0.3` is `0x3e99999a`).

### 8.1 Layer rows

Planes of a layer channel are read row by row, in order; rows can be skipped without decoding
them:

- PackBits (compression 1): the counts (`height · cb` bytes, `cb = 4` in PSB, else 2) must be
  there when the renderer is built. A row is decoded (4.2) to exactly `width` bytes when read;
  skipping a row only advances by its count. A row past the data is an error when read.
- Raw (0): the channel's data past its compression field must have at least `width · height`
  bytes when the renderer is built.
- ZIP (2, 3): decoded whole when the renderer is built (section 9).
- No compression field: all samples 0.
- Other compressions: an error when the renderer is built.

### 8.2 Operations

Layers are listed bottom to top. Groups are matched first: walking the layers in order, a
divider (section type 3) is pushed on a stack; a folder record (1 or 2) pops the top divider,
if any, and is its folder. A folder is *direct* if it is not hidden, its blend key is "pass"
and its opacity is 255.

```text
depth = 0
for each layer i:
  if divider:
    if it has a direct folder: mark the folder skipped
    else: emit Push; depth += 1
    emit ClearClip; next layer
  if folder record:
    emit ClearClip
    if skipped or depth == 0: next layer
    depth −= 1
    emit Pop(opacity, mode(blend key), hidden); next layer
  if no channel with id 0, 1 or 2 (any missing): next layer
  if the layer's width or height is 0: next layer
  build the layer's rows (8.1): channels 0, 1, 2 (R, G, B), −1 (alpha) if present, and −2 if
    the layer has an enabled user mask and a −2 channel (the first channel of each id counts)
  emit Layer(i)
emit depth × Pop(255, normal, false)
```

`mode(key)`: "norm" and "pass" are normal; the keys of 8.4 their blend functions; any other
key is normal. A Layer's opacity is `round(opacity · fill)` with `fill = iOpa / 255` (1 without
the block), as floats. A Pop takes the folder record's opacity byte as it is (its fill is not
used). A layer *clips* if `clipping ≠ 0`.

*Clipping bases*: walking the operations, a Layer that does not clip becomes the current base,
a Push, Pop or ClearClip clears it, and a clipping Layer leaves it as it is (several clipping
layers in a row share one base). A clipping Layer marks the current base (if any) as a base,
and if the base is hidden, the clipping layer becomes hidden. Then Layers that are hidden and not
bases are dropped.

### 8.3 One row

For row `y`: canvas 0 is transparent (all bytes 0). `clip_range = [0, 0)`. Operations in
order:

- **Push**: the next canvas, transparent.
- **Pop(opacity, mode, hidden)**: unless hidden, for every `x`: with `s` the top canvas's pixel
  and `a = (s.alpha · opacity + 127) div 255`, `put(dst[x], s.rgb, a, mode)` on the canvas
  below. The top canvas is dropped.
- **ClearClip**: `clip_range = [0, 0)`.
- **Layer(i)**, with the layer's rectangle `top`, `left` and size `lw × lh`, `ly = y − top`:
  - If `ly` is outside `0 .. lh−1`: if the layer does not clip, `clip_range = [0, 0)`. Done.
  - Columns: `x0 = clamp(left, 0, w)`, `x1 = clamp(left + lw, x0, w)`; layer column of `x` is
    `x − left`. Coverage `cov` (per layer column): row `ly` of the alpha channel, or all 255.
  - If the layer clips and `max(c0, x0) ≥ min(c1, x1)` (`clip_range = [c0, c1)`): `cov = 0` on
    `x0 .. x1−1`.
  - `seen` = some `cov` on `x0 .. x1−1` is nonzero.
  - With an enabled user mask (rect `mtop`, `mleft`, size `mw × mh`, `default_color`):
    `my = y − mtop`. If `0 ≤ my < mh`, read row `my` of the mask channel (even if not `seen`).
    If `seen`, for `x` in `x0 .. x1−1`: `v` = the mask sample at column `x − mleft` if the row
    was read and `mleft ≤ x < mleft + mw`, else `default_color`; `cov = (cov · v + 127) div 255`.
  - If the layer clips and `seen`: for `x` in `x0 .. x1−1`,
    `cov = (cov · v + 127) div 255` with `v = clip[x]` if `x` is in `clip_range`, else 0.
    Otherwise, if the layer is a base: if `seen`, `clip[x] = cov` for `x` in `x0 .. x1−1` and
    `clip_range = [x0, x1)`; else `clip_range = [0, 0)`.
  - If the layer is hidden, or not `seen`, or `cov` is 0 on all of `x0 .. x1−1`: done (the
    color rows are not read).
  - Read row `ly` of channels 0, 1, 2. For `x` in `x0 .. x1−1` with `cov ≠ 0`:
    `a = cov` if the layer opacity `op` is 255, else `(cov · op + 127) div 255`;
    `put(canvas[x], (r, g, b), a, mode)` on the top canvas.

Rows of a channel before the one read are skipped (8.1); every row of the layer's alpha within
`0 .. lh−1` is read when the row is rendered, and of the mask within `0 .. mh−1` while the
layer's row is in range.

The output row, from canvas 0's pixel `(r, g, b, A)`: each color is `r` if `A = 255`, 255 if
`A = 0`, else `(r · A + 255 · (255 − A) + 127) div 255`; plane 3 is `A`. (Colors are matted on
white.)

**put(dst, s, a, mode)**, `a` 0 .. 255:

```text
if a == 0: nothing
da = dst.alpha
if (mode is normal and a == 255) or da == 0:  dst = (s, a);  done
if mode is not normal:  blend (below);  done
ws = a · 255;  wd = da · (255 − a);  tot = ws + wd;  oa = (tot + 127) div 255
if oa == 0:  dst = (0, 0, 0, 0);  done
dst.color_k = (s_k · ws + dst.color_k · wd + tot div 2) div tot;  dst.alpha = oa
```

**blend** with blend function `B`:

```text
ab = da / 255.0;  cb_k = dst.color_k / 255.0;  sf_k = s_k / 255.0
m  = B(cb, sf)
sm_k = (1.0 − ab) · sf_k + ab · m_k
af = a / 255.0
oa = af + ab · (1.0 − af);  oa8 = round(oa · 255.0)
if oa8 == 0:  dst = (0, 0, 0, 0);  done
c_k = (sm_k · af + cb_k · ab · (1.0 − af)) / oa
dst.color_k = byte(round(c_k · 255.0));  dst.alpha = oa8
```

### 8.4 Blend functions

`B(b, s)` on colors in 0 .. 1. The separable ones apply per component:

| key | `B(b, s)` |
|---|---|
| `dark` | `min(b, s)` |
| `lite` | `max(b, s)` |
| `mul ` | `b · s` |
| `scrn` | `b + s − b · s` |
| `over` | `b ≤ 0.5 ? 2.0 · b · s : 1.0 − 2.0 · (1.0 − b) · (1.0 − s)` |
| `hLit` | `s ≤ 0.5 ? 2.0 · b · s : 1.0 − 2.0 · (1.0 − b) · (1.0 − s)` |
| `sLit` | `s ≤ 0.5 ? 2.0 · b · s + b · b · (1.0 − 2.0 · s) : 2.0 · b · (1.0 − s) + sqrt(b) · (2.0 · s − 1.0)` |
| `div ` | `b == 0.0 ? 0.0 : s ≥ 1.0 ? 1.0 : min(b / (1.0 − s), 1.0)` |
| `idiv` | `b ≥ 1.0 ? 1.0 : s ≤ 0.0 ? 0.0 : 1.0 − min((1.0 − b) / s, 1.0)` |
| `lddg` | `min(b + s, 1.0)` |
| `lbrn` | `max(b + s − 1.0, 0.0)` |
| `diff` | `|b − s|` |
| `smud` | `b + s − 2.0 · b · s` |
| `fsub` | `max(b − s, 0.0)` |
| `fdiv` | `s ≤ 0.0 ? (b > 0.0 ? 1.0 : 0.0) : min(b / s, 1.0)` |
| `lLit` | `clamp(b + 2.0 · s − 1.0, 0.0, 1.0)` |
| `vLit` | `s ≤ 0.5 ? (s ≤ 0.0 ? 0.0 : 1.0 − min((1.0 − b) / (2.0 · s), 1.0)) : s ≥ 1.0 ? 1.0 : min(b / (2.0 · (1.0 − s)), 1.0)` |
| `pLit` | `s ≤ 0.5 ? min(b, 2.0 · s) : max(b, 2.0 · s − 1.0)` |
| `hMix` | `b + s ≥ 1.0 ? 1.0 : 0.0` |

Products and sums associate to the left (`2.0 · b · s` is `(2.0 · b) · s`). The others work on
the three components:

| key | `B(b, s)` |
|---|---|
| `dkCl` | `lum(s) < lum(b) ? s : b` |
| `lgCl` | `lum(s) > lum(b) ? s : b` |
| `hue ` | `setlum(setsat(s, sat(b)), lum(b))` |
| `sat ` | `setlum(setsat(b, sat(s)), lum(b))` |
| `colr` | `setlum(s, lum(b))` |
| `lum ` | `setlum(b, lum(s))` |

```text
lum(c)       = 0.3 · c0 + 0.59 · c1 + 0.11 · c2
sat(c)       = max(c0, c1, c2) − min(c0, c1, c2)          (max and min left to right)
setlum(c, l) = clipcolor(c + (l − lum(c)))                (the difference added to each)
clipcolor(c):
  l = lum(c);  n = min(c0, c1, c2);  x = max(c0, c1, c2)
  for each component v:
    if n < 0.0:  v = l + (v − l) · l / (l − n)
    if x > 1.0:  v = l + (v − l) · (1.0 − l) / (x − l)
setsat(c, s):
  lo, mid, hi = 0, 1, 2
  if c[lo] > c[mid]: swap lo, mid
  if c[mid] > c[hi]: swap mid, hi
  if c[lo] > c[mid]: swap lo, mid
  out = (0, 0, 0)
  if c[hi] > c[lo]:  out[mid] = (c[mid] − c[lo]) · s / (c[hi] − c[lo]);  out[hi] = s
```

(`(v − l) · l / (l − n)` is `((v − l) · l) / (l − n)`.)

## 9. ZIP channels

A ZIP channel (compression 2 or 3) holds a zlib stream: the first 2 bytes (the zlib header,
not checked; fewer than 2 bytes is an error) are skipped and the rest is decoded as DEFLATE
(RFC 1951) up to the end of its final block. Bytes after that (the Adler-32) are ignored.
Errors: a reserved block type; a stored block whose `NLEN` is not the complement of `LEN`; an
over-subscribed code (incomplete codes are allowed, but decoding a bit pattern no code has is
an error); a dynamic header without a code for 256, a repeat (16) with no previous length, or
code lengths running past `HLIT + HDIST`; literal/length symbols 286, 287 and distance symbols
30, 31 (the fixed code has 32 five-bit distance codes, of which 30 and 31 are these; length
symbol 284 with extra bits 31 is length 258, as in zlib); a distance before the start of the data; and the data ending before the final block
does. The output must be exactly `rowbytes · height` bytes, `rowbytes = ceil(width · depth / 8)`.

For compression 3 with depth 8, each row (of `width` bytes) is undone as
`row[i] += row[i − 1]` for `i = 1 .. width−1`, in order.

## 10. Compressor

Everything here is the reference compressor's choice. Its output is the same bytes whatever
the number of threads.

**Stored or coded.** The file is stored (version 0, zstd at the level `L(n, n)` below, with
the content size in the frame header) if it does not parse as a PSD/PSB (section 7, on the
whole file), if it has no span, or if coding fails (a PackBits row of a coded span that does
not decode to its width, a panic, …). Otherwise it is coded, even if the result is larger.

**Pixel spans.** Only in 8-bit documents:

- Layer channels, for all layers in order, each channel in order, with nonzero width and height
  and compression raw or PackBits: the channel's data past its compression field, one plane of
  `width × height`. Raw requires exactly `width · height` bytes. PackBits requires the counts
  to add up to the data exactly, and every 8th row (`r mod 8 = 0`) to decode to `width` bytes;
  otherwise the channel stays in the skeleton.
- The composite (`channels` planes, `1 ≤ channels ≤ 64`, nonzero size, raw or PackBits),
  checked the same way, after the layers and the embedded files.

**Groups.** Per layer, in RGB documents (mode 3), the spans of channels 0, 1, 2 (and −1 if
present), all of the same size, form a group (alpha last); every other span of the layer is a
group of its own, in channel order. The composite in an RGB document with at least 3 channels
forms a group of its first 3 planes, plus the fourth if `channels ≥ 4` and the layer count is
negative (merged alpha); the remaining planes are groups of their own.

**Hint.** The composite group is hinted if there are layers and the rendering of the layers
(section 8, from the file before the composite span) matches the stored composite (all of the
group's planes) for at least half of the pixels of the rows `y` with `y mod 16 = 8`.

**Copies.** A layer span with the same bytes and the same `w × h` as an earlier layer span (the
first such) becomes a copy if both are one plane and the earlier one is coded before it: in an
earlier group, or earlier in the coding order of the same group (then the plane gets
`same_as`). A group of one plane that became a copy of an earlier group's plane is dropped.

**Embedded files** (in containers of depth below 3): the files of the global `lnk2`, `lnkD` and
`lnk3` blocks with file type `8BPB` or `8BPS` (PSD/PSB, coded by this compressor at depth + 1;
one it would store is not embedded),
`png ` or `PNGf` (png-recons) and `JPEG` (jpeg-recons). Each entry of such a block is a BE u64
length and that many bytes; the entries are 4-aligned relative to the block data. In an entry,
after 8 bytes, a u8-length id, a u32 character count and twice that many bytes (the name), the
file type (4 bytes), 4 bytes and a BE u64 file length, the file starts at the first occurrence
of its leading bytes (`8BPS`, `89 50 4E 47` or `FF D8 FF`) and must fit the entry. A file is
kept if its coded form is smaller than the file and it starts after every span before it.

**Encoder identification** (per PackBits span):

1. *Forced splits*: in each sampled row (every 8th), the positions (in decoded bytes) where a
   token follows a token shorter than 128 although one encoder pass would have merged them: a
   run followed by a token starting with the same byte, or a literal followed by a literal or
   a run of 2.
2. *Tile cut*: if every split is a multiple of 128, no cut (phase and period infinite).
   Otherwise, with `s0` the first split that is not, candidate phases `c = s0 − 128 j`
   (`j = 0 .. s0 div 128`); for each, with `s1` the first split after `c` with
   `(s1 − c) mod 128 ≠ 0`, candidate periods `s1 − c − 128 j ≥ 128` (infinite if there is no
   `s1`). A split `s` is *explained* by `(phase, period)` if `s mod 128 = 0` (for `s < phase`)
   or `(s − phase) mod period mod 128 = 0`. The cut explaining the most splits wins; starting
   from no cut, a candidate replaces the best if it explains more, or as many with the same
   phase and a longer period.
3. Candidates: the tile cut, then Greedy (3, false), (3, true), (2, false), (2, true), stably
   sorted by how many of the rows `0, k, 2k, …` (`k = max(1, ceil(rows / 32))`) that decode to
   `w` bytes each reproduces, most first.
4. The first candidate codes the span; the rows it does not reproduce are counted while coding.
   If they are more than `rows div 32 + 4`, every other candidate, and the tile cut inferred
   from the forced splits of all rows (if not a candidate already), is tried on all rows; the
   one with the fewest misses wins (the earlier on ties).
5. The rows the chosen encoder does not reproduce are the exceptions.

**Model stream.** Chunks of `2^22` symbols (the last one shorter). A table is used if it has a
symbol in the chunk. Its levels: with `m` the largest count, a symbol of count `c > 0` gets
`max(1, 63 + round(4 · log2(c / m)))` (binary64, round half away from zero), and absent symbols
level 0. The table is written as in 5.1: each run of zero levels as gamma(0), gamma(run − 1);
each other level `l` as gamma(`z + 1`) with `z` the zigzag of `d = l − prev`
(`2d − 1` for `d > 0`, `−2d` otherwise).

**Skeleton.** One zstd frame without the content size, at level `L(skeleton length, file
length)`: with `need = 500 · skeleton / file` (MB/s), the first of 19 (8), 12 (30), 9 (60),
6 (120), 3 (300), 1 (500) whose speed in parentheses is at least `need`, else 1.

**Order of output.** Head, skeleton, embedded files in span order, model stream; the head is
written last and put in front.

## Appendix: level weights

`W[l] = (F[(l − 1) mod 4] << ((l − 1) div 4)) >> 8` for `l = 1 .. 63`, with
`F = [65536, 77936, 92682, 110218]` (`2^(k/4) · 2^16` rounded); `W[0]` is unused. In rows of
eight, `l = 0 .. 63`:

```text
         0        256        304        362        430        512        608        724
       861       1024       1217       1448       1722       2048       2435       2896
      3444       4096       4871       5792       6888       8192       9742      11585
     13777      16384      19484      23170      27554      32768      38968      46341
     55109      65536      77936      92682     110218     131072     155872     185364
    220436     262144     311744     370728     440872     524288     623488     741456
    881744    1048576    1246976    1482912    1763488    2097152    2493952    2965824
   3526976    4194304    4987904    5931648    7053952    8388608    9975808   11863296
```
