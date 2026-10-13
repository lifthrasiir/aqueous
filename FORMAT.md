# JRC container format

This document specifies the container written by `jrc` (jpeg-recons) completely enough to
reimplement both directions. A container holds one JPEG file, the *original*, which
decompression must restore bit-exactly.

Sections 1–10 are normative for decoders. Section 11 is about the compressor: everything
there is the encoder's free choice within the constraints given, except where it says
otherwise.

## 1. Conventions

- Integers are unsigned unless stated. `u16` and `u64` fields are little-endian.
- **varint**: LEB128. 7 bits per byte, least significant group first, bit 7 set on every byte
  but the last. Values are `u64`. Writers emit the shortest form. Readers accept up to 10 bytes;
  more is an error. Bits above bit 63 in the 10th byte are ignored.
- `bitlen(x)` is the number of significant bits of `x ≥ 0` (`bitlen(0) = 0`,
  `bitlen(5) = 3`).
- `a / b` on integers is floor division, and `ceil(a / b)` rounds up.
- *Zigzag order*: coefficient `k` (0–63) of a block in zigzag order is the coefficient at
  natural (row-major) index `ZIGZAG[k]`; see Appendix A. `ZI` is the inverse map
  (`ZI[ZIGZAG[k]] = k`). Coefficient `k = 0` is DC.
- **xxh3** is XXH3-64 (`XXH3_64bits`) with seed 0 and the default secret.

## 2. Container

```text
offset 0   6 bytes   magic: 69 04 7E 8E 9D E0        (base64 "aQR+jp3g")
offset 6   u16       version
offset 8   streams:  stream 1, stream 2, ..., stream N
stream  =  varint length, then `length` bytes of data
```

The version selects the internal layout:

| version | layout |
|---|---|
| 0 | coded, sequential JPEG (SOF0 / SOF1), section 3 |
| 1 | coded, progressive JPEG (SOF2), section 3 |
| 2 | stored |
| other | invalid |

The number of streams and the meaning of stream *n* follow from the version and the contents of
streams 1..*n*−1. A container ends exactly after its last stream. Further bytes, a missing
stream, or a stream extending past the end make the container invalid.

**Stored** (version 2): one stream, the original file verbatim.

## 3. Coded containers (versions 0 and 1)

### 3.1 Stream sequence

| # | stream | present |
|---|---|---|
| 1 | head (3.2) | always |
| … | nested container (3.3) | one per embedded entry of kind 0, in entry order |
| … | cut blob (3.4) | always |
| … | scan info (3.5) | always |
| … | raw scan data (3.6) | one per raw scan |
| … | truncated tail (3.6) | one per truncated scan (version 0 only) |
| … | tables (section 8) | always |
| … | rANS words, then raw bits (section 9) | one pair per stripe |

The raw scan and tail streams are interleaved in scan order. A scan contributes at most one of
them. The stripe count comes from the scan info stream.

A stream whose fields are listed below (head, scan info) must be used up exactly by those
fields. Any excess byte is an error.

### 3.2 Head

```text
u64     xxh3 of the original
varint  n_embedded
per embedded entry i = 0 .. n_embedded-1:
  varint  gap      bytes of the cut blob between the previous entry (or the blob start) and this one
  varint  length   length of the embedded JPEG
  u8      kind     0 = coded in a nested container, 1 = copy of an earlier entry
  [kind 1: varint ref   index of an earlier entry, ref < i]
```

Each embedded entry is a byte range of the blob (3.4) that holds a JPEG embedded in the
original, such as an Exif thumbnail or an MPF secondary image. Kinds other than 0 and 1 are
invalid.

*Depth*: the outermost container has depth 0, and a nested container has its parent's depth
plus 1. Kind 0 is invalid in a container of depth 3 or more.

### 3.3 Nested containers

Each kind 0 entry, in entry order, is followed by one stream holding a complete container
(magic, version, streams; any version) at depth + 1. Decompressing it must give exactly
`length` bytes. A kind 1 entry has no stream: its bytes are those of entry `ref`, and the
lengths must agree.

### 3.4 Blob

The *blob* is the original with the entropy-coded data of every scan removed. Section 5 defines
which bytes those are. Everything else (markers, segments, data between and after them, the
trailer) stays. The *cut blob* stream holds the blob with the embedded entries removed. The
blob is rebuilt as:

```text
q = 0
for each entry:  blob += cut[q .. q+gap]  (error if past the end);  blob += entry bytes;  q += gap
blob += cut[q ..]
```

### 3.5 Scan info

```text
varint  n_scans
per scan: u8 flags, then
  [not raw: varint trailing_rst,
            [truncated: varint mcus, varint bits]]
varint  n_stripes
```

Flag bits:

| bit | version 0 | version 1 |
|---|---|---|
| 0 (1) | pad_zero: padding uses 0-bits instead of 1-bits | pad_zero |
| 1 (2) | raw: the scan's data is stored verbatim | raw |
| 2 (4) | truncated | must be 0 |
| 3 (8) | boundary: truncated at a restart boundary | must be 0 |
| 4–7 | must be 0 | must be 0 |

Constraints: a raw scan has flags exactly 2. `boundary` requires `truncated`.
`trailing_rst < 2^32`. `n_scans` must equal the number of scans found by parsing the blob.
`1 ≤ n_stripes ≤ 256` and `n_stripes ≤ max(1, mcu_rows)` (section 6).

Version 1 has no truncated scans: the compressor keeps a progressive scan that ends early
as raw, or stores the file (section 11).

### 3.6 Raw scan data and truncated tails

For a raw scan the stream is the scan's entropy-coded data, exactly as it appeared in the
original (stuffed, RST markers included). For a truncated scan the stream is the tail used in
10.2, given in destuffed form.

### 3.7 Decompression

1. Read the head and the nested containers, then rebuild the blob (3.4).
2. Parse the blob as a JPEG (section 5). The frame must be progressive exactly when the version
   is 1. Every scan's entropy-coded data in the blob must be empty (`start = end`).
3. Read the scan info, the raw scan and tail streams, the tables and the stripes. Check that
   the container ends there.
4. Decode the coefficient arrays (sections 6–9).
5. Re-emit every non-raw scan (section 10), with version 0 or version 1 semantics.
6. Output the blob with each scan's data (re-emitted, or the raw stream) inserted at that
   scan's `start` offset in the blob, in scan order.
7. If xxh3 of the output differs from the head's value, decompression fails. The output must
   not be returned.

## 4. Bit orders

The format uses three bit orders.

- **JPEG bits** (section 10): MSB-first. Each byte fills from bit 7 down.
- **Model bits** (tables and raw bits, sections 8–9): LSB-first. Bit `i` of a byte string is
  `(byte[i/8] >> (i%8)) & 1`. An `n`-bit field read at position `p` has the value
  `Σ bit(p+j) << j` for `j` in 0..n. Writers pad the final byte with zeros and write no
  further bytes. Readers ignore bits after the last field.
- **rANS words** (section 9): 32-bit words, each stored as 4 little-endian bytes.

## 5. JPEG parsing

This parser runs on the blob during decompression. The compressor runs it on the original,
where it also delimits the entropy-coded data. Any error makes the container invalid.

State: an optional frame; DC and AC Huffman table specs for slots 0–3 (initially undefined);
quantisation tables for slots 0–3 (initially all zero); a restart interval `ri = 0`; a list of
scans.

```text
error unless len(d) >= 4 and d[0..2] = FF D8
i = 2
loop:
  skip bytes != FF;  skip bytes == FF;  if i >= len(d): stop (no EOI is fine)
  m = d[i];  i += 1
  m = D9:                 stop
  m = 01 or D0..D8:       continue (standalone marker)
  L = be16(d[i..i+2])     (error if truncated); error if L < 2
  seg = d[i+2 .. i+L]     (error if truncated)
  handle m (below);  i += L      -- except SOS, see there
after the loop: error if there is no frame or no scan
```

Reading a field past the end of `seg` is an error. Markers:

- **C0, C1, C2 (SOF)**: a second SOF is an error. `progressive = (m = C2)`. `seg[0]` (the
  precision) must be 8. Read `height = be16(seg[1..])`, `width = be16(seg[3..])` and
  `n = seg[5]`. It is an error unless `1 ≤ n ≤ 4`, `width > 0` and `height > 0`. For each
  component `c` read 3 bytes at `seg[6+3c..]`: id, `H = byte >> 4`, `V = byte & 15` and
  `tq = byte & 3`. It is an error unless `1 ≤ H, V ≤ 4` and the id is not already used. Then,
  with `Hmax` and `Vmax` the largest H and V:
  - `mx = ceil(width / (8·Hmax))` and `my = ceil(height / (8·Vmax))`.
  - Per component, `wib = ceil(width·H / (8·Hmax))` and `hib = ceil(height·V / (8·Vmax))`.
  - The block grid is `bw × bh = wib × hib` when `n = 1`, otherwise `(mx·H) × (my·V)`.
  - It is an error if `Σ bw·bh > 2^24`.
- **C3, C5–C7, C9–CB, CD–CF**: error (unsupported frame types).
- **C4 (DHT)**: while bytes remain in `seg`, read:
  - a byte with `tc = byte >> 4` and `th = byte & 15`. It is an error if `tc > 1` or `th > 3`.
  - 16 counts (codes of length 1..16).
  - `Σ counts` symbol values.
  
  These replace the DC (`tc = 0`) or AC (`tc = 1`) spec in slot `th`.
- **DB (DQT)**: while bytes remain, read a byte with `pq = byte >> 4` and `tq = byte & 15`. It
  is an error if `pq > 1` or `tq > 3`. Then read 64 entries, u8 if `pq = 0` and be16 otherwise.
  They replace table `tq` and are kept in stream (zigzag) order.
- **DD (DRI)**: `ri = be16(seg[0..])`.
- **DA (SOS)**:
  1. It is an error without a frame.
  2. At the first SOS, snapshot the four quantisation tables. Section 7 uses this snapshot.
  3. `ns = seg[0]` must satisfy `1 ≤ ns ≤ 4`.
  4. For each scan component `s`, read the id at `seg[1+2s]` and the table selector `t` at
     `seg[2+2s]`. The id must name a frame component, and no component may appear twice in the
     scan. Snapshot the DC spec of slot `(t>>4)&3` and the AC spec of slot `t&3`. Either may be
     undefined.
  5. `Ss = seg[1+2ns]`, `Se = seg[2+2ns]`. The byte at `seg[3+2ns]` gives `Ah = byte>>4` and
     `Al = byte&15`.
  6. MCU grid: `mcux × mcuy = wib × hib` of the scan's component if `ns = 1`, otherwise
     `mx × my`.
  7. The entropy-coded data spans `start = i + L` to `end = scan_end(d, start)`. Record the scan
     with its components, tables, parameters and the current `ri`. Set `i = end` and do not add
     `L`.
- **Any other marker** (APPn, COM, …): skipped.

```text
scan_end(d, start):
  i = start
  loop:
    ff = position of the next FF at or after i;  if none: return len(d)
    n = ff + 1;  while n < len(d) and d[n] = FF: n += 1
    if n >= len(d): return ff                  -- trailing FFs are a cut-off marker
    if d[n] in {00, D0..D7}: i = n + 1  else: return ff
```

*Destuffing* turns a scan's data into the *destuffed data* and a list of segment offsets:

- `FF 00` becomes `FF`.
- `FF Dn` (with any extra `FF` fill before it) ends a restart segment. It is dropped, and the
  current destuffed length is recorded as the next segment's start.
- The first segment starts at 0.

The destuffed data concatenates all segments with no markers.

## 6. Coefficient arrays and stripes

The decoded image is one array per frame component of `bw × bh` blocks, in raster order. A
block holds 64 signed coefficients in zigzag order. Valid containers have `|AC| ≤ 32767` and
`|DC| ≤ 16383`. A decoder need not check these limits: the reference keeps coefficients as
i16 (a DC of `pred + value` is wrapped to 16 bits), and a container that breaks them fails in
re-emission (section 10) or at the checksum. The component class is `cc = 0` for the first frame component and `cc = 1` for
all others.

The image is coded as `ns = n_stripes` horizontal stripes of whole MCU rows:

```text
mcu_rows = bh of component 0              if the frame has 1 component
         = bh / V of component 0          otherwise
f(c)     = 1 if the frame has 1 component, else V of component c
rows(c, s) = [ (s·mcu_rows/ns)·f(c),  ((s+1)·mcu_rows/ns)·f(c) )     -- block rows of component c in stripe s
```

Stripe `s` codes, for each component `c` in frame order, the blocks of `rows(c, s)` in raster
order. Each stripe has its own rANS and raw bit streams (section 9). Each component within
each stripe starts with fresh neighbour state (section 7), so no context crosses a stripe or a
component. All stripes share one set of tables (section 8).

## 7. Block model

### 7.1 Symbol groups

The model's symbols fall into *groups*, each with its own distribution:

| group | parameters | count | alphabet |
|---|---|---|---|
| NZ (nonzero count) | `cc` ∈ 0..1, `p` ∈ 0..63 | 128 | 64 |
| AC (category) | `cc`, `k` ∈ 1..63, `rc` ∈ 0..10 | 1386 | 16 |
| DC (category) | `cc`, `nb` ∈ 0..8, `sub` ∈ 0..7 | 144 | 16 |

*Group order* is all NZ groups by `cc·64 + p`, then all AC groups by
`(cc·63 + k−1)·11 + rc`, then all DC groups by `(cc·9 + nb)·8 + sub`. That makes 1658 groups.

### 7.2 Value splitting

A value `x` is coded as its category `cat = bitlen(|x|)` (a symbol) followed by `cat` raw bits
(section 9) holding
`payload = ((|x| & (2^(cat−1) − 1)) << 1) | (x < 0 ? 1 : 0)`. In stream order that is the sign
bit first, then the bits of `|x|` below its leading 1, least significant first. `cat = 0` writes
no bits.

Decoding: `m = (payload >> 1) | (2^cat >> 1)` and `x = (payload & 1) ? −m : m`.

### 7.3 Coding one block

Neighbour state per component per stripe:

- `nz[]` and `dcc[]` per block.
- `bottom[]` per block column.
- `right`, a scalar.

`has_a` means the block is not in the first row of the component's rows in this stripe, and
`has_l` means it is not in column 0. A neighbour value that is not available counts as absent.

1. **Nonzero count**. Let `nz` be the number of nonzero coefficients among `k = 1..63`. Let
   `S` be the sum of `nz` of the block above (if `has_a`) and of the block to the left (if
   `has_l`). The context is:
   - `p = ceil(S/2)` with both neighbours.
   - `p = S` with one.
   - `p = 0` with none.
   
   Clamp `p` to at most 63, then code `nz` in group NZ(`cc`, `p`).
2. **AC coefficients**, if `nz > 0`:
   - Start with `rem = nz`. For each `k = 1, 2, …` while `rem > 0`, split `x = coef[k]`.
   - Code its `cat` in group AC(`cc`, `k`, `rc(rem)`) and append its payload to the raw bits.
   - Decrement `rem` if `x ≠ 0`.
   
   So positions are coded up to and including the last nonzero coefficient, and a decoder
   leaves the rest zero. `rc(rem)` is the class of `rem`:

   | rem | 1 | 2 | 3 | 4–5 | 6–7 | 8–11 | 12–15 | 16–23 | 24–31 | 32–47 | 48–63 |
   |---|---|---|---|---|---|---|---|---|---|---|---|
   | rc | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 |

   (`b = bitlen(rem)`, `rc = 2b + ((rem >> max(b−2, 0)) & 1) − 3`.)
3. **DC**. Compute `(pred, sub)` (7.4) from this block's AC coefficients. Split
   `r = coef[0] − pred`.
   - `C` is the sum of `dcc` of the block above (if `has_a`) and to the left (if `has_l`).
   - `nb` is the number of edges in `{0, 1, 2, 4, 6, 8, 10, 12}` that are `≤ min(C, 63)`, so
     `nb` ranges over 1..8.
   - Code `cat` in group DC(`cc`, `nb`, `sub`) and append the payload to the raw bits.
4. **Update**:
   - `nz[block] = nz` and `dcc[block] = cat` of the DC residual.
   - `(bottom[bx], right) = bounds(block)` (7.4).

Symbols go to the stripe's rANS stream and payloads to its raw bit stream, both in exactly this
order.

### 7.4 DC prediction

The prediction comes from boundary continuity with the neighbours, in the coefficient domain.
`q` is the quantisation table (zigzag order) that the first-SOS snapshot holds in slot `tq` of
the block's component. Define the f64 constants (exact bit patterns):

| v | CV[v] | | v | CV[v] |
|---|---|---|---|---|
| 0 | `0x1.6a09e667f3bcdp-1` (1/√2) | | 4 | `0x1.6a09e667f3bcdp-1` |
| 1 | `0x1.f6297cff75cb0p-1` | | 5 | `0x1.1c73b39ae68c9p-1` |
| 2 | `0x1.d906bcf328d46p-1` | | 6 | `0x1.87de2a6aea964p-2` |
| 3 | `0x1.a9b66290ea1a3p-1` | | 7 | `0x1.8f8b83c69a60dp-3` |

Per component, compute integer weights with IEEE f64 arithmetic. `round` is
round-half-away-from-zero, and the products are evaluated left to right:

```text
qc(v) = q[ZI[8v]]      (column 0, row v)        qr(u) = q[ZI[u]]      (row 0, column u)
W(v, x) = (i32) round(CV[v] * x * 1024.0)
wa[v] = W(v, qc(v)),  wl[v] = W(v, qr(v))                   for v = 1..7
wb[v] = (−1)^v · W(v, qc(v)),  wr[v] = (−1)^v · W(v, qr(v))  for v = 0..7
inv   = (f32) (1.0 / (CV[0] * max(q[0], 1) * 1024.0))       (f64, rounded once to f32)
```

For a block `b`, products, sums and the differences below use wrapping i32 arithmetic (`c(r, s)` is the coefficient at row `r`,
column `s`, that is `b[ZI[8r + s]]`):

```text
bounds(b)       = ( Σ_{v=0..7} wb[v]·c(v,0),  Σ_{u=0..7} wr[u]·c(0,u) )          -- (bottom, right)
sa = Σ_{v=1..7} wa[v]·c(v,0),   sl = Σ_{u=1..7} wl[u]·c(0,u)
dca = (f32)(bottom[bx] − sa) * inv        dcl = (f32)(right − sl) * inv          -- i32 difference, then f32 ops
both neighbours:  pf = (dca + dcl) * 0.5,  unc = |dca − dcl|
only above:       pf = dca, unc = 0        only left: pf = dcl, unc = 0        none: pf = 0, unc = 0
pred = (i32) clamp(round(pf), −16383, 16383)                                    -- f32 round half away
sub  = number of edges in {1, 2, 4, 8, 16, 32, 64} that are ≤ min(trunc(unc), 127)
```

`(f32)` of an i32 rounds to nearest-even. `round` of `pf` is half away from zero, and
converting a float to an integer (`(i32)`, `trunc`) saturates. All f32 operations are single IEEE operations, so
the results are exact and platform independent. `bottom[bx]` belongs to the block above and
`right` to the block to the left.

## 8. Tables

The tables stream gives a normalised distribution for every group. Each distribution has
frequencies summing to 4096 (`PROB_BITS = 12`). The stream is one model bit stream (section 4).

Groups are pooled into *parents*, and AC parents into *grandparents*:

| level | parameters | count | alphabet | parent of a group |
|---|---|---|---|---|
| NZ parent | `cc`, `j` ∈ 0..7 | 16 | 64 | NZ(`cc`, `p`) → (`cc`, `bitlen(p)`) |
| AC parent | `cc`, `k` ∈ 1..63 | 126 | 16 | AC(`cc`, `k`, `rc`) → (`cc`, `k`) |
| DC parent | `cc`, `nb` ∈ 0..8 | 18 | 16 | DC(`cc`, `nb`, `sub`) → (`cc`, `nb`) |
| grandparent | `cc`, `band` ∈ 0..9 | 20 | 16 | AC parent (`cc`, `k`) → (`cc`, `band(k)`) |

Parent order is NZ parents by `cc·8 + j`, then AC parents by `cc·63 + k−1`, then DC parents by
`cc·9 + nb`. Grandparent order is `cc·10 + band`. `band(k)` is:

| k | 1–2 | 3–5 | 6–9 | 10–14 | 15–20 | 21–27 | 28–35 | 36–44 | 45–54 | 55–63 |
|---|---|---|---|---|---|---|---|---|---|---|
| band | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 |

Layout, all in one bit stream:

1. **used**: 1 bit per group, in group order.
2. **inherit**: 1 bit per used group, in group order. 1 means the group takes its parent's
   distribution. A parent is *needed* if some used group inherits it.
3. **inherit-grand**: 1 bit per needed AC parent, in parent order. 1 means it takes its
   grandparent's distribution. A grandparent is *needed* if some needed AC parent inherits
   it. NZ and DC parents have no grandparent and no bit here: when needed, they always have
   their own table (step 5).
4. A **table** per needed grandparent, in grandparent order.
5. A **table** per needed parent that does not inherit its grandparent, in parent order.
6. A **table** per used group that does not inherit, in group order.

The result:

| group | distribution |
|---|---|
| unused | `f[0] = 4096`, all other `f = 0` |
| used, inheriting | the parent's distribution |
| used, not inheriting | the group's own table |

A parent that inherits uses its grandparent's distribution.

**Table**:

```text
4 bits   sh (precision shift), must be <= 8;  total = 4096 >> sh
for each symbol but the last:  v = gamma() − 1;  running sum of v must stay <= total;  f = v << sh
last symbol:                   f = (total − sum) << sh
```

`gamma()` reads an Elias gamma code. Read bits up to and including the first 1 bit; `n` is the
number of 0 bits before it (more than 13 is an error). That 1 bit is the value's leading bit;
read `n` more bits, most significant first, below it. The value is in `1 .. 2^14`. Any read past the end of the stream is an error. Writers emit nothing after the last table
except the zero padding of the final byte.

## 9. Stripes: rANS and raw bits

Each stripe has two streams:

- **rANS words**: a multiple of 4 bytes, at least 32, read as 32-bit LE words `w[0..]`.
- **Raw bits**: a model bit stream (section 4). The payloads of section 7 are read from it in
  order.

**Decoding** uses four 64-bit states. Initially `x[i] = w[2i] << 32 | w[2i+1]` for `i` in 0..3,
and `pos = 8`. Symbol `j` of the stripe (counting from 0, across all groups) uses state
`x[j mod 4]`. To decode one symbol of a group with frequencies `f` and cumulative frequencies
`c[s] = Σ_{t<s} f[t]`:

```text
slot = x & 4095
s    = the symbol with c[s] <= slot < c[s] + f[s]
x    = f[s] · (x >> 12) + slot − c[s]
if x < 2^31:  x = (x << 32) | w[pos];  pos += 1
```

Reading a word past the end of the stream, or raw bits past its end, makes the container
invalid. Words left over at the end are allowed.

**Encoding** (reference, gives a valid stream):

1. Symbols are encoded from last to first, symbol `j` with state `j mod 4`. All states start at
   `2^31`.
2. Before encoding a symbol with frequency `f` and cumulative `c`: if `x ≥ 2^51 · f`, output the
   word `x & 0xFFFFFFFF` and set `x >>= 32`.
3. Then `x = (x / f)·4096 + (x mod f) + c`.
4. The stream is `x[0] >> 32, x[0] & 0xFFFFFFFF, …, x[3] >> 32, x[3] & 0xFFFFFFFF`, followed
   by the output words in reverse order of output.

The encoder must give every coded symbol a nonzero frequency.

## 10. Scan re-emission

### 10.1 Common parts

- **Huffman codes**: canonical, as in JPEG Annex C. Lengths 1..16 come in order with
  `counts[l]` codes each. Codes increase by one within a length and are doubled between
  lengths, and symbols are assigned in `vals` order. If a symbol occurs twice, its later
  assignment wins. Emitting a symbol that has no code, or using an undefined table, is an
  error.
- **`size_bits(v)`**: `s = bitlen(|v|)`. The bits are `v` if `v ≥ 0`, otherwise
  `(v − 1) & (2^s − 1)`. *Emit a value* means emit the code of the symbol, then the `s` bits.
- **Layout**: the scan components follow SOS order. If the scan has one component, each MCU is
  one block of that component, at `(mx, my)` on the `mcux × mcuy` grid. Otherwise each MCU
  holds, for each scan component, its `H × V` blocks at `(mx·H + h, my·V + v)`, with `v` as the
  outer and `h` as the inner loop. A block outside the component's `bw × bh` is an error. MCU
  `m` sits at `(m mod mcux, m / mcux)`, and `nmcu = mcux · mcuy`.
- **Bits** are written MSB-first into an unstuffed buffer. To *pad*, fill up to the next byte
  boundary with 1-bits, or 0-bits if `pad_zero`. Nothing is written if already aligned.
- **Restart**: with `ri > 0`, before every MCU `m > 0` with `m mod ri = 0`:
  1. Progressive only: flush the EOB run.
  2. Pad.
  3. Record an RST position at the current byte offset.
  4. Reset the DC predictions to 0.
- **Finish**: append `trailing_rst` further RST positions at the end offset. Then output the
  buffer cut at the RST positions. Each piece is stuffed (`FF` becomes `FF 00`), and the
  `r`-th RST position (0-based) gets the marker `FF (D0 + r mod 8)`.

### 10.2 Version 0 (sequential)

For each MCU `m` in `0 .. emit`, where `emit = nmcu` (or `min(mcus, nmcu)` if truncated), apply
the restart rule. Then code each block:

```text
diff = b[0] − pred[component of the scan];  pred = b[0];  emit value diff with the DC table
last = 0
for each k in 1..63 with b[k] != 0, ascending:
  run = k − last − 1
  while run >= 16:  emit symbol F0 (ZRL);  run −= 16
  (s, bits) = size_bits(b[k]);  emit symbol (run << 4 | s), then the s bits;  last = k
if last < 63:  emit symbol 00 (EOB)
```

After the last MCU:

- **Not truncated**: pad.
- **Truncated**:
  1. If `boundary`, pad and record an RST position.
  2. The written bit count must now equal `bits`, otherwise it is an error.
  3. Cut the buffer to `floor(bits/8)` bytes and append the tail stream. The tail starts with
     the byte holding the partial bits. It is part of the buffer from now on: finish stuffs it
     like the rest, and `trailing_rst` positions go after it.

Then finish (10.1).

### 10.3 Version 1 (progressive)

The scan kind comes from its parameters. Any other combination is an error. Let
`ok = Al ≤ 13 and (Ah = 0 or Al + 1 = Ah)`:

| kind | condition | table |
|---|---|---|
| DC first | `Ss = Se = 0`, `Ah = 0`, ok | DC |
| DC refine | `Ss = Se = 0`, `Ah > 0`, ok | none |
| AC first | `1 ≤ Ss ≤ Se ≤ 63`, one component, `Ah = 0`, ok | AC |
| AC refine | `1 ≤ Ss ≤ Se ≤ 63`, one component, `Ah > 0`, ok | AC |

The encoder is libjpeg's `jcphuff.c`. State: `EOBRUN = 0` and a list `BE` of buffered
correction bits. To *flush the EOB run*, when `EOBRUN > 0`:

1. Let `n = bitlen(EOBRUN) − 1`. It is an error if `n > 14`.
2. Emit symbol `n << 4`, then the low `n` bits of `EOBRUN`.
3. Emit all bits of `BE` in order.
4. Set `EOBRUN = 0` and clear `BE`.

Per block (`>>` is an arithmetic shift):

- **DC first**: `d = b[0] >> Al`. Emit the value `d − pred` (as in 10.2) and set `pred = d`.
- **DC refine**: emit 1 bit, `(b[0] >> Al) & 1`.
- **AC first**:
  ```text
  last = Ss − 1
  for k in Ss..Se:  a = |b[k]| >> Al;  if a = 0: continue
    r = k − last − 1;  flush EOB run
    while r > 15:  emit symbol F0;  r −= 16
    s = bitlen(a);  emit symbol (r << 4 | s);  emit s low bits of (b[k] < 0 ? ~a : a);  last = k
  if last < Se:  EOBRUN += 1;  if EOBRUN = 0x7FFF: flush EOB run
  ```
- **AC refine**:
  ```text
  a[k] = |b[k]| >> Al for k in Ss..Se;  eob = largest k with a[k] = 1 (0 if none)
  r = 0;  BR = empty bit list
  for k in Ss..Se:
    if a[k] = 0:  r += 1;  continue
    while r > 15 and k <= eob:  flush EOB run;  emit symbol F0;  r −= 16;  emit BR;  BR = empty
    if a[k] > 1:  append (a[k] & 1) to BR;  continue
    flush EOB run;  emit symbol (r << 4 | 1);  emit 1 bit (b[k] >= 0 ? 1 : 0);  emit BR
    BR = empty;  r = 0
  if r > 0 or BR not empty:
    EOBRUN += 1;  append BR to BE
    if EOBRUN = 0x7FFF or len(BE) > 937:  flush EOB run
  ```

After the last MCU, flush the EOB run, pad and finish (10.1). EOB runs use the first scan
component's table.

## 11. Compressor

Everything here is the encoder's choice. A decoder relies only on sections 1–10 and on the
checksum.

- **Coefficients**: blocks of modelled scans hold their decoded quantised coefficients, in
  zigzag order:
  - **Sequential**: the values coded in the scan. Blocks of MCUs a truncated scan does not
    contain are zero.
  - **Progressive**: the final values after jdphuff decoding of the scans in order:
    - DC first: `v << Al`.
    - DC refine: OR in `1 << Al`.
    - AC first: `v << Al`.
    - AC refine: a new coefficient is `±(1 << Al)`. A correction bit adds `1 << Al` to the
      magnitude of a coefficient whose `Al` bit is clear.

  Blocks no modelled scan needs may hold anything within the limits of section 6.
- **Scan choices**: a scan may be modelled (not raw) only if 10.2 or 10.3 reproduces its bytes
  exactly. The reference decoder checks this inline for every degree of freedom (EOB and ZRL
  placement, padding, RST numbering and fill, trailing RSTs, EOB-run cuts). For progressive
  files, from the first scan that breaks the scan script on, every scan is raw:
  - Each coefficient bit must be coded once, first by a scan with `Ah = 0`, then by refinements
    whose `Ah` equals the previous `Al`.
  - This also applies from the first scan that fails to decode.
  
  A file in which no scan can be modelled is stored.
- **Truncation** (sequential): when the data ends inside MCU `M`:
  - `mcus = M`.
  - `bits` is the bit position in the destuffed data right after MCU `M−1`. When `M` starts a
    restart interval whose segment exists, it is that segment's start instead, with `boundary`
    set.
  - The tail is the destuffed data from byte `floor(bits/8)` to the end.
- **Embedded JPEGs**: the compressor searches the blob for `FF D8 FF`. It follows each
  candidate's markers and scans to its EOI (`jpeg_extent`), keeping non-overlapping hits of at
  least 1024 bytes. A hit is coded at depth + 1. It is kept as kind 0 only if its container is
  more than 8 bytes shorter than the hit. A repeat of an earlier entry's bytes becomes kind 1.
- **Stripes**: `ns = clamp(Σ bw·bh / 32768, 1, clamp(mcu_rows, 1, 256))`. This depends only on
  the image, so the output does not depend on the thread count.
- **Tables**:
  - For each candidate shift (0..8), the frequencies are normalised to `4096 >> sh`:
    1. Each count is scaled down, with a floor of 1 for occurring symbols.
    2. The remainder goes to the most frequent symbol, or is taken 1 at a time from symbols
       above 1.
  - The shift that minimises table bits plus estimated data bits wins.
  - A group inherits its parent when coding its counts with the parent's distribution costs
    less than its own table and data. Parents choose against grandparents the same way.
- **Fallback**: the result is stored (version 2) if coding fails or panics, or if the coded
  container is not smaller than the stored one.

## Appendix A. Zigzag order

`ZIGZAG[k]`, the natural index of zigzag position `k`:

```text
 0  1  8 16  9  2  3 10 17 24 32 25 18 11  4  5
12 19 26 33 40 48 41 34 27 20 13  6  7 14 21 28
35 42 49 56 57 50 43 36 29 22 15 23 30 37 44 51
58 59 52 45 38 31 39 46 53 60 61 54 47 55 62 63
```
