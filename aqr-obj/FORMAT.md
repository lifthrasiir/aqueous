# The objr format, version 0

This document specifies the compressed format of aqr-obj (`objr`) in enough detail
to write an independent decoder, or an encoder whose output this decoder accepts. The
decoder's behaviour is normative; what the encoder chooses (block size, number modes,
predictors, stream methods) is not, and is marked as such.

Conventions: integers are little-endian unless stated otherwise. `a / b` on integers
truncates toward zero. Signed arithmetic marked "wrapping" is two's complement modulo
2^64. Indices into arrays start at 0. "Fails" means the input is corrupt and decoding
stops with an error.

## 1. Primitives

**varint**: unsigned LEB128, 7 bits per byte, least significant group first, the high bit
set on every byte but the last. Writers emit the shortest form, at most 10 bytes for a
u64. A reader reads at most 10 bytes: the tenth ends the varint whatever its high bit,
and bits beyond 64 are dropped.

**svarint**: a signed i64 as the varint of its zigzag form, `zz(v) = (v << 1) ^ (v >> 63)`
(arithmetic shift), `unzz(u) = (u >> 1) ^ -(u & 1)`.

**Streams** inside a block are read with a lenient cursor: reading past the end yields
zero bytes (a varint read there yields 0) and advances the position anyway. Every stream
must be consumed exactly (§5.8); a position past the end fails. Outside streams (the file
header, block headers, stream descriptors), a read past the end of the file fails.

## 2. File

```text
magic       6 bytes   69 04 7f d1 b8 ed      ("aQR/0bjt" in base64)
version     u16       0
length      varint    length of the original file in bytes
hash        u64       XXH3-64 (seed 0) of the original file, little-endian
blocks      until their lengths sum to `length`
```

A file whose magic differs is not an objr file; any version other than 0 is
unsupported, and decoders must refuse it. After the last block the file must end: no
trailing bytes. The decoded output must hash to `hash`. An empty original is the header
alone.

## 3. Blocks

```text
method      u8        0 whole, 1 coded (others fail)
n           varint    length of the block's output; 1 <= n <= 64 MiB and n <= bytes still to come
method 0:   varint c, then c bytes: zstd data decompressing to exactly n bytes
method 1:   35 stream descriptors, in stream order (§4):
              raw     varint   stream length after decoding
              if raw > 0:
                how   u8       0 stored, 1 zstd, 2 rANS (§6)
                c     varint   length of data
                data  c bytes
```

Constraints on a coded block (others fail): `raw <= 8 n + 64`; stored data has
`c == raw`; zstd data decompresses to exactly `raw` bytes; rANS is allowed only for the
streams marked rANS in §4 and must decode to exactly `raw` bytes. The streams are then
decoded (§5) with the state carried from the previous coded blocks, and must produce
exactly `n` bytes.

A whole block is output as is and **does not touch the decoder state**: the next coded
block continues from the state after the last coded one. (The encoder stores a block
whole when its coding fails verification, and rolls its own state back.)

zstd data is as produced by the zstd library's one-shot compression (RFC 8878 frames).

Encoder (non-normative): blocks of 4 MiB, cut after the last `\n` within the limit when
there is one. A stream longer than 64 bytes is zstd-compressed (level 3) and kept if
smaller than raw; a rANS stream is tried too and kept if smaller than both.

## 4. Streams

| # | name | contents | rANS |
|---|---|---|---|
| 0 | ops | varint per line: 0 raw line, 1 raw line without `\n`, t+2 template t | |
| 1 | tpl | definitions of new templates: varint length, key bytes | |
| 2 | text | raw lines, concatenated | |
| 3 | hdr | one byte: bit 0 unit normals; other bits 0 (ignored by readers) | |
| 4 | scale | svarint: scale changes of non-fixed numbers | yes |
| 5 | nsel | byte per unit-predicted normal: bits 0-1 component, bit 2 negative | |
| 6-8 | fv, ft, fn | corner index symbols per attribute v, vt, vn (bytes) | |
| 9-11 | fv-esc, ft-esc, fn-esc | svarint escapes per attribute | yes |
| 12-14 | v-dup, vt-dup, vn-dup | varint per line: distance back to an equal line, or 0 | yes |
| 15-30 | v0 v1 v2 v3+, vt0…, vn0…, vp0… | svarint residuals by kind (v, vt, vn, vp) and position (0, 1, 2, 3 or more): stream `15 + 4 kind + min(j, 3)` | yes |
| 31 | vn-unit | svarint residuals of unit-predicted normal components | yes |
| 32-34 | v-pred, vt-pred, vn-pred | predictor per line (bytes, 0..=31) | yes |

## 5. Decoding a coded block

### 5.1 State

Carried from coded block to coded block, starting as:

- `prev[kind][j]` for kind 0..=3 (v, vt, vn, vp), position j 0..=7: a value and a scale,
  all (0, 0);
- `count[k]` for k 0..=2 (v, vt, vn): lines of that kind decoded through templates, 0;
- the template table, empty; at most 65,536 templates and 4 MiB of key bytes in all;
- per kind v, vt, vn, a ring of recent lines (§5.5), empty;
- per attribute v, vt, vn, an index cache (§5.6): 32 entries 0, `max` 0, `off` 0;
- the corner table (§5.6), empty.

The decoder never resets any of it.

### 5.2 Lines

Read the hdr byte (`unit` = bit 0). Then, while the ops stream is not exhausted, read an
op:

- **0**: copy the text stream up to and including its next `\n` (none left: fail).
- **1**: copy the rest of the text stream.
- **op ≥ 2**: template `t = op - 2`. If `t` equals the number of templates, a new one is
  defined first: read a varint length and that many bytes from tpl; the key must be
  valid (§5.3) and fit the table's limits, else fail; it becomes template `t`. A larger
  `t` fails. Output the line of template `t` (§5.4).

Raw lines change no state.

### 5.3 Template keys

```text
kind  u8   0 v, 1 vt, 2 vn, 3 vp, 4 f, 5 l
n     u8   numbers (1..=8) for kinds 0-3, corners (1..=64) for kinds 4-5
n items:
  mode  u8
  flags u8
  sl    u8   1..=255
  sep   sl bytes    blanks before the item
tl    u8   then tl bytes: the tail (blanks, then "\r\n", "\n" or nothing)
```

The key must end exactly after the tail. For numbers, `(mode, flags)` is a number
descriptor (§7.1) and must be valid. For corners, `flags` is 0 and `mode` is a pattern
(each corner item has its own, so the corners of one line may differ) below 32 with bit 3 set only if bit 0 is, and bit 4 only if bit 1 is:

| bit | meaning |
|---|---|
| 0 | vt index present |
| 1 | vn index present |
| 2 | v index written relative (negative) |
| 3 | vt index relative |
| 4 | vn index relative |

The decoder copies `sep` and the tail as they are. (The encoder only makes keys of
blanks, spaces and tabs, and a valid tail; the keyword is a token of its own, followed by
at least one blank.)

### 5.4 Template lines

Output the keyword (`v`, `vt`, `vn`, `vp`, `f`, `l`), then the items (§5.5 or §5.6),
then the tail.

### 5.5 Number lines (kinds 0-3)

Let `K` be the kind, `n` the count, `P = prev[K]`, `c = count[K]` for K < 3.

1. **Equal line.** If K < 3 and n ≤ 3, read `d` from dup[K]. If d > 0, the line equals
   the ring entry for line `c - d` (d > c, or no such entry: fail): set `P[j]` to the
   entry's value and scale for j < n (the entry's own scale, whatever the descriptors),
   and skip to step 6.
2. **Unit normal.** If `unit`, K = 2 and n = 3, read a byte from nsel: `sel` =
   min(bits 0-1, 2), `neg` = bit 2. Otherwise there is no `sel`.
3. **Predictor.** If K < 3 and 2 ≤ n ≤ 3, read a byte from pred[K]: `vp` = min(byte, 31).
   Otherwise `vp` = 0.
4. **Values**, in coding order: positions 0..n-1 except `sel`, then `sel`. For position
   j with descriptor (mode, flags) and previous `(pv, ps) = P[j]`:
   - scale `s`: if mode is 0 (fixed), `flags & 31`; else `clamp(ps + svarint(scale),
     -60, 60)` (the addition in i64, wrapping);
   - prediction `q`:
     - j = sel: `unit_pred` below, from positions `a = (j+1) % 3`, `b = (j+2) % 3` of
       `P` (already decoded on this line);
     - else vp > 0: the window predictor `vp` (below);
     - else `rescale(pv, ps, s)`;
   - residual `r`: svarint from vn-unit if j = sel, else from stream `15 + 4K +
     min(j, 3)`;
   - `P[j] = (q + r wrapping, s)`.
5. (Nothing else is read.)
6. If K < 3 and n ≤ 3, put the entry `(P[0..n])` into the ring as line `c`.
7. Output, for j in 0..n: `sep` of item j, then the number `P[j]` printed in item j's
   descriptor (§7).
8. If K < 3, `count[K] += 1` (whatever n, equal line or not).

**rescale(v, from, to)**: if `to >= from`, `v × 10^min(to-from, 18)` (wrapping); else
`v / 10^min(from-to, 18)` (truncating).

**unit_pred**: with `x(v, s)` = `v as f64 / 10^min(s,22)` for s ≥ 0, `v as f64 ×
10^min(-s,22)` for s < 0 (`v` rounded to the nearest double, then one IEEE double
operation, round to nearest even; the powers of ten are exact doubles):

```text
A = x(P[a]), B = x(P[b])
z = sqrt(max((1 - A*A) - B*B, 0))           IEEE double, in this order, no fused multiply-add
p = z × 10^min(s,22) if s >= 0, else z / 10^min(-s,22)
p = round half away from zero, converted to i64 (saturating)
q = -p if neg, else p
```

**The ring** of a kind holds the last 65,536 lines by number: slot `line mod 65536`
holds the line's values and scales (scales as i8), tagged with the line number. A
lookup of line L succeeds only if the slot's tag is L. An entry of fewer than three
numbers has value `i64::MIN` and scale -128 at the absent positions.

**Window predictors** (vp > 0) of position j at scale s, from the ring entries of lines
`c - k` (written `w[k][j]`, each rescaled from its own scale to `s`; a missing entry
fails):

- 1 ≤ vp ≤ 15: `w[vp+1][j]`;
- 16 ≤ vp ≤ 31: with `k = vp - 15`: `w[1][j] + w[k][j] - w[k+1][j]` (wrapping).

Encoder (non-normative): equal lines are found with a hash of the values. The predictor
is the one leaving the fewest residual bits among 0, the window and the parallelograms,
staying with the previous line's choice unless another saves more than 3 bits. `unit`
is set when 80% of the sampled normals have length 1 within 10^-3; the unit-predicted
component is the one of largest magnitude (the first on ties), `neg` its sign. A zero
in a non-fixed mode is coded at the previous scale of its position, so that its scale
change is 0.

### 5.6 Corner lines (kinds 4-5)

Per attribute k (0 v, 1 vt, 2 vn), an index cache: `cache[0..32]`, `max`, `off`. Corner
symbols (bytes from stream 6+k):

| symbol | index |
|---|---|
| 0..=31 | `cache[symbol]` |
| 32 | `max + 1` (wrapping) |
| 33, 34 | candidate 0, 1 (an absent candidate gives 0) |
| 35..=66 (higher clamped to 66) | `cache[symbol - 35] + svarint(esc[k])` (wrapping) |

After decoding index `a` for a corner whose v index is `v` (for k = 0, `v = a`), update
the cache: let `p` be `a`'s position for symbols 0..=31, its first position in the cache
for 33 and 34 (31 if absent), and 31 for 32 and escapes; move `cache[0..p]` to
`cache[1..p+1]`, set `cache[0] = a`, `max = max(max, a)`, `off = a - v` (wrapping). With
`p = 31` the last entry drops out. A corner's candidates (below) are computed before its
cache update.

**The corner table**: 65,536 slots, slot `v mod 2^16` (of v as a 64-bit two's complement
number) holding a corner `(v, before, after, t, n)`. A lookup of v finds the slot's
corner if its v equals v. Before any face has been recorded every lookup fails; after,
slots never written hold `(0, 0, 0, 0, 0)`. Whether a face has been recorded is part of
the state, carried across blocks like the table.

**Candidates** of attribute k for a corner with v index `v`, after a corner of the same
line with v index `pv`:

- k = 0: the corner of `pv` in the table: `[before, after]`; for the first corner of a
  line or when the lookup fails, both absent;
- k = 1, 2: `[v + off_k (wrapping), t (k = 1) or n (k = 2) of the corner of v]`, the
  second absent when the lookup fails.

For each of the n corners, with pattern `pat` from the key:

1. output `sep`;
2. decode the v index (k = 0) and output it;
3. if `pat & 1`: output `/`, decode the vt index (k = 1), output it;
4. if `pat & 2`: output `/` if `pat & 1`, else `//`; decode the vn index (k = 2),
   output it.

An index `a` of attribute k is output as its decimal digits (`-` first if negative),
or, if its relative bit is set, as `-` followed by `count[k] + 1 - a` (i64, wrapping)
printed the same way, so a non-positive value gives `-0` or `--` and digits (an encoder
never codes one). Absent vt, vn indices are 0.

Then record the line's corners in the table, in order: for corner i of n, at the slot of
its v, `(v_i, v_(i-1 mod n), v_(i+1 mod n), t_i, n_i)`. Corner lines do not change
`count`.

Encoder (non-normative): relative indices are resolved as `count[k] + 1 - x`. The symbol
is the first that applies of: candidate 0, candidate 1, a cache position, `max + 1`, an
escape from the cache entry minimizing the bit length of the zigzagged difference (plus
2 bits for positions other than 0).

### 5.7 Lines kept as text

Not normative: the encoder keeps a line as text when it is not a `v`, `vt`, `vn`, `vp`,
`f` or `l` line, when a number token is printed in no mode that reproduces it, when an
index is not `[-]` digits without leading zeros (1 to 18 digits), when a line has more
than 8 numbers or 64 corners, a separator or tail over 255 bytes, or when the template
table is full.

### 5.8 End of block

After the ops stream is exhausted, every other stream (text, tpl, hdr, scale, nsel,
symbols, escapes, dup, residuals, predictors) must be consumed exactly, else fail.

## 6. rANS streams

A rANS-coded stream is a sequence of varints (decoded to their canonical LEB128 bytes).
Each value `u` is one symbol and some raw bits:

- `b` = bit length of `u` (0 for 0); for b < 2 the symbol is `4b`, no raw bits;
- else, with `nb = b - 1`, `top = min(nb, 2)`, `nraw = nb - top`: symbol
  `4b | ((u >> nraw) & (2^top - 1))`, and the low `nraw` bits of `u` raw.

So 260 symbols (b up to 64). Back: symbol s, `b = s >> 2`; for b < 2 the value is b;
else `2^nb | (s & (2^top - 1)) << nraw | raw`.

```text
used        varint     1..=260 symbols in the table, in increasing order
used times:
  gap       varint     symbol - previous symbol - 1 (the first: the symbol itself)
  freq      varint     frequency - 1
len         varint
rans        len bytes
raw         the rest of the data
```

Frequencies must sum to 4096 (`M`, 12 bits), each symbol below 260. Symbols are laid out
in increasing order: `cum[s]` = sum of the frequencies of the symbols below s.

Decoding: two states, `x0` = the first four rans bytes big-endian, `x1` = the next four
(reading past `len` gives zeros). The table's varints are read like a stream's (past the
end of the data they read 0); only `len` bytes must fit in the data. For value i = 0, 1, 2… (until the output reaches the
stream's raw length), with `x = x_(i mod 2)`:

```text
slot = x & 4095
s    = the symbol with cum[s] <= slot < cum[s] + freq[s]
x    = freq[s] * (x >> 12) + slot - cum[s]
twice (two separate tests): if x < 2^23 then x = (x << 8) | next rans byte
```

then take the symbol's raw bits from `raw`, read as one bit stream least significant bit
first (bit k of the stream is bit `k mod 8` of byte `k / 8`; past the end, zeros), and
append the LEB128 of the value. The output must reach exactly the raw length. Unread rans
bytes and raw bits are ignored.

Encoder: states start at 2^23; symbols are coded last to first, symbol i on state
`i mod 2`: `while x >= 2^19 * freq: emit x & 255, x >>= 8`, then `x = (x / freq << 12) +
x mod freq + cum`. The rans bytes are x0 then x1, each big-endian, then the emitted bytes
in reverse order of emission. Frequencies (non-normative): the histogram scaled to 4096,
each used symbol at least 1, the remainder to the most frequent symbol. A stream holding a
non-canonical varint is not rANS coded.

## 7. Numbers

A number is a descriptor `(mode, flags)` and a decimal `D = v × 10^-s`, `v` an i64. The
printed token depends only on the descriptor and `D`. Every printed byte is ASCII.

### 7.1 Descriptors

`mode = render | source << 4`. Sources: 0 decimal (the value is `D`), 1 `f32`, 2 `f64`.
Valid descriptors (526):

| render | | sources | flags |
|---|---|---|---|
| 0 | fixed `%.Nf` | 0 | bits 0-4 N (0..=18), bit 5 point, bit 6 no integer part, bit 7 negative zero; without bit 5, N = 0 and bit 6 clear; bit 6 needs N > 0 |
| 1 | C `%.Pg` | 0-2 | bits 0-4 P (1..=17), bit 5 three-digit exponents, bit 6 negative zero, bit 7 clear |
| 2 | C `%.Pe` | 0-2 | as `%g`, P in 1..=19 |
| 3 | Python `repr` | 0-2 | 0 or 64 (negative zero) |
| 4 | JavaScript `toString` | 0-2 | 0 or 64 |
| 5 | positional (Rust `Display`) | 0-2 | 0 or 64 |

### 7.2 Fixed

`s` is ignored: the number is `v` with N digits after the point. Output `-` if v < 0, or
v = 0 with bit 7; then the decimal digits of |v|, left-padded with zeros to N+1 digits
(N with bit 6); with bit 5 a `.` before the last N digits (`5.` for N = 0).

### 7.3 Values and digits

The value printed:

- decimal source: `D`; a zero is negative if flag bit 6 (negative zero) is set;
- `f64`: `D` rounded to the nearest double, ties to even (overflow: infinity);
- `f32`: that double rounded to the nearest `f32`, ties to even (two roundings);
- a float source of a zero `D` is -0.0 with the negative zero flag, else +0.0. Its sign
  is the float's sign bit. A non-finite value prints `inf` (an encoder never codes one).

Its digits `d1…dk` and the exponent `E` of `d1` (value = d1.d2…dk × 10^E):

- decimal source: the digits of `D` without trailing zeros; for `%e` with fewer than P
  digits, padded with zeros to P;
- `%g`, `%e` of a float: exactly P significant digits of the float's exact value,
  rounded half to even (as C's `printf`);
- `repr`, `toString`, positional of a float: the shortest digits that convert back to
  the same double (for `f32`, its value widened to a double), the nearest to the value
  among those, ties to an even last digit (Python's `repr`, ECMAScript's
  `Number::toString`).

Zero is the digit `0` with E = 0. Below, `ds` is the digits without trailing zeros (at
least one digit).

### 7.4 Layouts

A `-` comes first for a negative value or negative zero, except in `toString`, where a
zero never has one. An exponent is `e`, then `+` or `-`, then the decimal of |E|
left-padded with zeros to at least 2 digits (3 with flag bit 5; 1 in `toString`).
*Scientific* is `ds[0]`, then `.` and the rest of `ds` if there is more than one digit,
then the exponent. *Positional* of `ds` and E: for E < 0, `0.`, -E-1 zeros, `ds`; for
E < len(ds) - 1, `ds` with a `.` after its first E+1 digits; else `ds` and E+1-len(ds)
zeros.

- `%.Pg`: zero is `0`; scientific when E < -4 or E ≥ P, else positional. A decimal with
  more than P digits prints all of them.
- `%.Pe`: `d1`, then for P > 1 a `.` and `d2…dP` (the digits truncated to, or padded with
  zeros to, P; an encoder never codes a decimal with more than P digits), then the
  exponent.
- `repr`: positional when zero or -4 ≤ E < 16, `.0` appended when that has no point;
  else scientific.
- `toString`: positional when zero or -6 ≤ E < 21; else scientific.
- positional: always positional.

Encoder (non-normative): a token is coded in the first of these that prints it back
exactly: the block's mode for its kind of line, fixed, decimal `%g` (P of the mode,
clamped to 6..=17), and for tokens with an exponent decimal `%e`, `f64` and `f32` `%e`,
decimal `repr` and `toString`. The scale is that of `D`'s last digit, except for decimal
`%g` and `%e`, where it is P-1-E; scales outside -60..=60 are not coded. The block's
mode per kind of line is the one costing the fewest digits and scale changes on a sample
of the block (16 parts of 2 KiB, the first 512 tokens per kind).

## 8. Example

The 36-byte file

```text
v 1.5 2 -3
v 1.5 2 -3
f 1 2 -1
# hi
```

compresses (`objr c -t 1`) to:

```text
69 04 7f d1 b8 ed  00 00                 magic, version 0
24                                       length 36
94 98 51 1a 16 71 88 1e                  XXH3-64
01 24                                    coded block of 36 bytes
04 00 04  02 02 03 00                    ops: template 0, template 0, template 1, raw line
22 00 22  10 00 03 04 00 01 20 04 00 01 20 04 00 01 20 01 0a
              10 04 03 00 00 01 20 00 00 01 20 04 00 01 20 01 0a
                                         tpl: "v" with 3 numbers in toString (mode 4), " " before
                                         each, tail "\n"; "f" with 3 corners, the third relative
05 00 05  23 20 68 69 0a                 text: "# hi\n"
01 00 01  00                             hdr: no unit normals
03 00 03  02 00 00                       scale: +1 (1.5 is 15 at scale 1), 0, 0
00                                       nsel
03 00 03  20 20 00                       fv: max+1, max+1, cache[0] (the 2 that -1 stands for)
00 00  00 00 00                          ft, fn, escapes
02 00 02  00 01                          v-dup: none, then the line 1 back
00 00                                    vt-dup, vn-dup
01 00 01  1e                             v0: 15
01 00 01  04                             v1: 2
01 00 01  05                             v2: -3
00 (x 14)                                v3+, vt0-vt3+, vn0-vn3+, vp0-vp3+, vn-unit
01 00 01  00                             v-pred: predictor 0 (previous numbers)
00 00                                    vt-pred, vn-pred
```

## 9. Formula module

The same coding as an entry formula (aqr-formula), for the archive to store: module
`obj-recons`, version 1. The blocks and their streams are those of §3-§6; what the file
header and the container carry is not repeated: no magic, version, length or hash (the
output length is the formula's), and no zstd (streams and whole blocks are stored raw; the
archive compresses the inputs). The module's output is the original file.

**Params**: none (empty).

**Inputs**: 37, always, any of them may be empty:

| # | class | contents |
|---|---|---|
| 0 | `obj/index` | the blocks, below |
| 1 | `obj/whole` | the bytes of the blocks coded whole, concatenated |
| 2 + i | `obj/<name>` | stream i of §4 (`ops`, `tpl`, ...), concatenated over the coded blocks: each stored stream as is, each rANS stream as its data (§6) |

**Index**: per block, in file order, until their lengths sum to the output length:

```text
v           varint    n << 1 | coded; n is the block's length (1 <= n <= 64 MiB, n <= bytes still to come)
coded = 0:  nothing; the next n bytes of input 1 are the block
coded = 1:  35 times, in stream order:
              v       varint    0 for an empty stream, else raw << 1 | rans
              if rans:
                c     varint    length of the rANS data
```

A coded block's stream i takes `raw` bytes (stored; `raw <= 8 n + 64`) or `c` bytes (rANS,
allowed for the streams marked rANS in §4, decoding to `raw` bytes) from where input 2 + i
was left by the previous blocks. A stream with `raw == 0` and `rans == 1`, and any
inconsistency, fails. The coded blocks are then decoded as in §5, with state carried over
from one coded block to the next, and a whole block does not touch it, as in §3. A block
must produce exactly n bytes. After the last block the index and all inputs must be
consumed exactly.

Compared with the container: the `method` byte and `n` become `v`; the stream descriptors
lose `how` (zstd does not exist; stored and rANS are the low bit of `v`) and the stored
length (`c == raw`); the encoder (non-normative) takes rANS for a stream longer than 64
bytes when it is smaller than raw, and no formula is made for an empty file or when every
block was stored whole.
