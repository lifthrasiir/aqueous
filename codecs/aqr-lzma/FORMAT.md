# aqr-lzma format

aqr-lzma splits an LZMA stream (LZMA1, as in `.lzma` files and 7z folders) into its
decompressed **data** and a **record** of the parse its encoder chose. From the data and the
record the stream is rebuilt bit for bit. LZMA2 streams (of `.xz` files and 7z folders) are
split likewise by their [segments](#lzma2-segments). This document describes the records, and
the [formula modules](#formula-module) that rebuild streams from data and records.

The record does not reproduce any encoder. It codes the encoder's packets given the data,
under contexts of what a match finder sees in the data, so that the packets an LZMA encoder
with a binary-tree match finder and an optimal parser chooses cost little. Any encoder's
stream has a record; others' cost more.

The document is normative. Where it describes an algorithm, it describes exactly what the
reference implementation does. Sections marked *informative* describe the reference
encoder's choices; a decoder needs none of them.

External reference: the LZMA format as Igor Pavlov's LZMA SDK specifies it
(`lzma-specification.txt`, public domain): the properties `lc`, `lp`, `pb`, the range
decoder, the probability model and the packets. This document only adds the range encoder
and how a stream ends. LZMA2's chunks are described [below](#lzma2-segments).

## Conventions

- Integers are unsigned unless said otherwise. `le32` is a little-endian 32-bit integer.
  `rd32(p)` and `rd64(p)` read 4 and 8 bytes of the data at position `p` as little-endian.
- Arithmetic marked *wrapping* is modulo 2^32 (`u32`) or 2^64 (`u64`); everywhere else it is
  exact. `>>` and `<<` are shifts; `rotl64(x, r)` rotates a `u64` left.
- `D` is the data and `N` its length. `bitlen(x)` is the number of bits of `x` (0 for 0),
  `bucket(x) = min(bitlen(x), 7)`.
- `NONE` is `2^32 − 1`; positions are below it.

## LZMA streams

### Packets

A stream is a sequence of packets over the probability model of the LZMA specification:

- a **literal** (1 byte);
- a **match** of length `n` (2 ≤ n ≤ 273) and distance `d + 1` (`d` as coded, `d < 2^32 − 1`);
- a **repeated match** `rep k` (k = 0 … 3) of length `n`, at the k-th last distance;
- a **short repeat**: one byte at the last distance.

The state (0 … 11) and the four last distances `reps[0..4]` (initially 0) change as the
specification says. A match's distance must reach no farther back than the start of the data
or the dictionary allows: `d < min(pos, R)`, where `pos` is the position of the packet and
`R = max(dict, 4096)` for the dictionary size `dict`. (Below, `R` is also what the match finder
looks back, unless the record's header says it looks less: its **reach** `F`, then.)

### Ends

A stream ends in one of two ways:

- with the **end marker**: a match of length 2 and distance code `d = 2^32 − 1`;
- without one, where its data has a known size: after the packet that reaches it, if the
  range decoder's `code` is 0 there. (Otherwise the next packet must be the end marker.)

In both cases the range decoder must end with `code = 0`. The stream is the bytes the
decoder read.

### Encoding

A stream is encoded by coding every bit the decoder reads, through the same model, with this
range encoder:

```text
low = 0 (u64), range = 2^32 − 1 (u32), cache = 0 (u8), pending = 1

bit b with probability p (11 bits):
  bound = (range >> 11) * p
  if b = 0: range = bound;  p += (2048 − p) >> 5
  else:     low += bound;  range −= bound;  p −= p >> 5
  while range < 2^24: range <<= 8; shift_low()

n direct bits of v, highest first: for each bit b:
  range >>= 1;  if b = 1: low += range
  while range < 2^24: range <<= 8; shift_low()

shift_low():
  if (low mod 2^32) < 0xFF000000 or low ≥ 2^32:
    carry = low >> 32;  out(cache + carry);  pending −= 1
    while pending > 0: out(0xFF + carry mod 256);  pending −= 1
    cache = (low >> 24) mod 256
  pending += 1
  low = (low mod 2^24) << 8

end: shift_low() five times
```

Encoding a stream's packets (and its end marker) gives back every stream a decoder accepts
under [Ends](#ends): with `code = 0` at its end, its last bytes are those of this flush.

## The match finder

The record's contexts come from a match finder over `D`. Before the packet at `pos`, it holds
the positions `0 … pos − 1`, inserted in order; it looks no farther back than its reach `F` (`R`
unless the header says less, [below](#the-record)). Everything below holds with `F` for `R`; the
data window and the distances a packet may have stay `R`'s.

### Tables

- `head2[65536]`: by `k2(p) = D[p] | D[p+1] << 8`, for `p + 2 ≤ N`.
- `head3[65536]`: by `h3(p) = (k3(p) × 0x9E3779B1, wrapping u32) >> 16`, where
  `k3(p) = D[p] | D[p+1] << 8 | D[p+2] << 16`, for `p + 3 ≤ N`.
- Three **chains**, of keys `K` = 4, 8 and 16 bytes, for `p + K ≤ N`: `head[2^20]` and a ring
  `prev[S]`, `S` the least power of 2 at least `min(F, max(N, 1))` (a candidate is at least
  `pos − F`, and its entry `c mod S` is taken again only by `c + S ≥ pos`, not inserted yet).
  Their hashes:

  ```text
  K = 4:  x = rd32(p)
  K = 8:  x = rd64(p)
  K = 16: x = rd64(p) XOR (rotl64(rd64(p + 8), 29) × 0xFF51AFD7ED558CCD, wrapping)
  h(p) = (x × 0x9E3779B97F4A7C15, wrapping u64) >> 44
  ```

Every entry starts as `NONE`. Inserting `p`: `head2[k2(p)] = p` and `head3[h3(p)] = p`, and in
each chain `prev[p mod S] = head[h(p)]`, then `head[h(p)] = p` (each where `p` is in range as
above).

### Queries

`lcp(a, b, cap)`, `a < b`: the length of the common prefix of `D[a..]` and `D[b..]`, at most
`min(cap, N − b)`. Below, `cap = min(273, N − pos)`.

- **head(k, pos)**, k = 2 or 3: none if `pos + k > N`; else `c = head2[k2(pos)]` or
  `head3[h3(pos)]`, which counts if `c ≠ NONE`, `pos − c ≤ F` and (for k = 3)
  `k3(c) = k3(pos)`.
- **The candidates** of chain K at `pos` (needs `pos + K ≤ N`): `c = head[h(pos)]`, then
  `prev[c mod S]`, and so on, as long as `c ≠ NONE` and `pos − c ≤ F`.
- **first(pos)**: if `pos + 4 ≤ N` and chain 4 has a candidate `c`, and `n = lcp(c, pos, cap)`
  is at least 4: `n`. Else 3 if head(3, pos), else 2 if head(2, pos), else 0.
- **longest(pos)**: `best` = 3 if head(3, pos), else 2 if head(2, pos), else 0. Then a pass
  over chain 4 from `best`, and (if then `best < 64` and `best < cap`) a pass over chain 16
  from 15, each keeping a list `found` (emptied first; the list of chain 16 is left empty if
  its pass does not run). A pass from `t`, if `pos + K ≤ N`, goes through the first 16
  candidates `c`: where `t < cap` and `D[c + t] = D[pos + t]`, `n = lcp(c, pos, cap)`, and if
  `n > t`, `t = n` and `(n, c)` goes on the list; once `n ≥ 64`, the pass stops. A pass that
  listed something leaves `best = max(best, t)`.
- **pairs(pos)**, after longest(pos): the entries of both lists, and `(lcp(c, pos, cap), c)`
  for each `c` of head(2, pos) and head(3, pos), ordered by position, nearest first; of these,
  each entry whose length is greater than that of every entry before it. (Entries of one
  position are equal.) The lengths increase, and so do the distances.
- **nearest(pos, n)**, after longest(pos), `2 ≤ n` and `pos + n ≤ N`: for n = 2 or 3,
  head(n, pos). Else the position of the first entry of chain 4's list with a length of at
  least `n`, else of chain 16's; else, of the first 64 candidates of chain K (16 if n ≥ 16,
  8 if n ≥ 8, else 4), the first `c` with `D[c + n − 1] = D[pos + n − 1]` and
  `lcp(c, pos, n) ≥ n`. None otherwise.

## The record

A record is a header of 7 bytes, then the body:

```text
u8    props    lc + 9 (lp + 5 pb), below 225
le32  dict     the dictionary size
u8    flags    bit 0: the stream ends with the end marker; bit 1: the body is in parts
               (below); the others 0
u8    reach    the match finder's reach F: 0 for R; else log2 F, from 12 and with 2^reach < R
```

The reach is the encoder's choice of a memory bound: the finder's rings are `12 × S` bytes, 768 MiB
for a dictionary of 64 MiB, and a finder that looks back less (`F` a power of 2 below `R`) finds
fewer far matches, which the record then codes as far distances (a distance not found is coded
the same whatever its size), but holds less. The data window stays `R`: a literal after a far
match and a far repeat read the data back to `R`. Other bytes are no record (a reach of `R` is
written as 0).

### Coding

The body is a binary arithmetic code (carry-less, 32 bits):

```text
x1 = 0, x2 = 2^32 − 1;  the decoder reads 4 bytes into x (zeros past the end)
bit with probability p1 of a 1 (16 bits):
  xmid = x1 + (((x2 − x1) × p1) >> 16)
  1: x2 = xmid;  0: x1 = xmid + 1        (the decoder reads 1 iff x ≤ xmid)
  while x1 and x2 agree in their top byte:
    the encoder writes x2 >> 24; x1 <<= 8; x2 = x2 << 8 | 0xFF; the decoder shifts in a byte
end: the encoder writes (x1 >> 24) + 1
```

A decoder that reads more than 4 bytes past the end of the body fails.

Each **adaptive bit** has `p1` (initially 32768) and a count `c` (initially 0). After coding a
bit `b`: `p1 += floor((t − p1) × RATE[c] / 65536)` with `t = 65535` for a 1 and 0 for a 0,
`RATE[i] = floor(131072 / (2i + 3))`, then `p1` is clamped to 32 … 65504 and `c`, if below 30,
grows by 1. A **raw bit** has `p1 = 32768` and is not adapted. A **tree** of `n` bits codes
`v < 2^n` highest bit first, bit `i` with the adaptive bit numbered by the bits above it
(`m = 1`, then `m = 2m + bit`); `tree9` is a tree of 9 bits.

The contexts below are arrays of adaptive bits (trees: arrays of trees), all fresh at the
start of a record.

### Packets

The body codes the packets in order until they cover `D`. The body of a segment of
[LZMA2](#lzma2-segments) has the **chunks'** headers among them (below): at the start of a chunk
the state `s` and `reps` become 0 where it resets them, and the bytes it stores are skipped (no
packet covers them); no packet goes past the end of its chunk. At each position `pos`, with the
state `s` and `reps` as the stream has them there (but for resets at chunks: there they become
0), after inserting the positions before `pos`:

```text
E = the end of the chunk, or N for LZMA
cap = min(273, E − pos)
rl[k] = lcp(pos − reps[k] − 1, pos, cap) if reps[k] < min(pos, R), else 0      (k = 0 … 3)
b0 = bucket(rl[0]),  bh = bucket(first(pos)),  brest = bucket(max(rl[1], rl[2], rl[3]))

bit is_match[s][bh][b0]: 0 a literal (done)
bit is_rep[s][bh][bucket(max(rl))]:
  0: a match (below)
  1: bit g0[s][b0][brest]:
       0: bit rep0_long[s][b0]: 0 a short repeat (fails if rl[0] = 0; done), 1 rep 0
       1: bit g1[bucket(rl[1])][bucket(max(rl[2], rl[3]))]: 0 rep 1, 1:
            bit g2[bucket(rl[2])][bucket(rl[3])]: 0 rep 2, 1 rep 3
     l = rl[k];  bit rep_full[k][bucket(l)]: 1: n = l;  0: n = tree9 rep_len[bucket(l)] + 2
     fails unless 2 ≤ n ≤ l
```

A **match**: longest(pos), then `P = pairs(pos)` of `m` entries `(len, c)`.

```text
j = tree(4) pair[min(m, 7)];  if j = 15: j += tree9 pair_more
if j ≥ m:            (longer than every pair)
  fails if j > m;  n = tree9 match_len + 2;  fails unless n > the last pair's length (if any)
else:
  i = m − 1 − j;  top = P[i].len;  lo = P[i − 1].len + 1 if i > 0, else 2
  bit pair_full[min(j, 3)][bucket(top)]: 1: n = top
    0: n = top − tree9 pair_short[bucket(top)] − 1;  fails unless lo ≤ n < top
fails unless 2 ≤ n ≤ cap
bit far: 0: d = pos − nearest(pos, n) − 1 (fails where there is none)
         1: k = tree(2) far_rep;  negative = bit far_sign[k];  b = tree(6) far_bucket
            fails if b > 32;  o = b if b ≤ 1, else 2^(b−1) + (b − 1 raw bits)
            d = reps[k] − o if negative, else reps[k] + o (exactly)
fails unless 0 ≤ d < min(pos, R) and lcp(pos − d − 1, pos, n) ≥ n
```

A literal and a short repeat cover 1 byte, the others `n`. The packets (and the bytes the breaks
skip) must cover `D` exactly (the walk ends at `N`, after every break); the end marker, if any,
follows them.

### Parts

A body in parts (flag bit 1; an LZMA stream's, never a segment's) codes its packets in
stretches of their own, so that they decode side by side:

```text
varint  k        the parts after the first, at least 1 and below N, and at most the
                 length of the body divided by 7
k + 1 times:     each part:
  varint  span     the bytes of data it covers, > 0
  varint  len      the length of its body
  u8      s        the state at its start, below 12        (all but the first)
  4 varint reps    reps[0 … 3] there, each below R         (all but the first)
  len bytes        its body
```

The spans add up to `N`; the first part starts at 0 with the state and `reps` 0, each other at
the sum of the spans before it, which is the position of a packet. A part's body is coded as
[above](#coding), with its own coder and all its contexts fresh, from its start to the end of
its span: the packets must cover that exactly, and the walk ends there. The bodies end
the record. Within a part everything is as above: in particular the
match finder holds the positions before `pos` (those of earlier parts too; with no breaks, `E`
is `N`, so a match may reach past the part's end into the data). As the match finder looks no
farther back than `R`, a decoder may start one of a part with only the positions from
`max(0, start − R)` inserted.

## Rebuilding a stream

The packets of the record (of every part, in order), then the end marker if its flag says so,
encoded as in [Encoding](#encoding) with the record's `props`.

## LZMA2 segments

An LZMA2 stream is a sequence of chunks, then a byte 0. A chunk starts with a control byte `c`:

- `c = 1` or `2`: **stored** data: `u` (big-endian u16) and `u + 1` bytes of data, as they are.
  1 resets the dictionary.
- `c ≥ 0x80`: an **LZMA chunk**: `u` (21 bits: `c & 0x1F`, then a big-endian u16), `p`
  (big-endian u16), the properties byte if `c ≥ 0xC0` (with `lc + lp ≤ 4`), then `p + 1` bytes
  coding the packets of `u + 1` bytes of data: a range coder of its own, started and flushed as
  in [Encoding](#encoding), without an end marker. Before it, by `(c >> 5) & 3`: 0 nothing is
  reset; 1 the state (the probabilities, `s` and `reps`) is reset; 2 that, with the properties
  given; 3 that, and the dictionary. Otherwise the model goes on from the chunk before (over
  stored chunks too, whose data counts as data before the next packets).

Other control bytes do not occur. A chunk resetting the dictionary (`c = 1`, or `c & 0xE0 =
0xE0`) starts a **segment**: positions (as `pos` is for the position state and the literal
contexts) count from its start, and no match reaches before it. A stream starts with one.

aqr-lzma takes a segment where it stands alone: its first LZMA chunk gives the properties (as
7-Zip's decoder wants), no match goes past its chunk, and encoding its packets gives back every
chunk.

### The record of a segment

```text
u8      flags    bit 0: the stream's end byte (0) follows the segment; the others 0
le32    dict     the dictionary size (as in an LZMA record: R = max(dict, 4096))
u8      reach    the match finder's reach (as in an LZMA record)
the body
```

The body is coded as an LZMA record's (`D` the segment's data), with a **chunk header** where
each chunk starts: at the position where the chunk before it ends (0 for the first), unless that
is `N` (so the chunks cover `D`, and an empty segment has none). A header codes, in the
[coding](#coding) above and the contexts `chunk_kind` (7 trees of 3 bits), `chunk_len` (32
adaptive bits) and `chunk_props` (a tree of 8 bits), all fresh at the start of the body:

```text
kind = tree(3) chunk_kind[prev];  c = [1, 2, 0x80, 0xA0, 0xC0, 0xE0][kind] (fails if kind ≥ 6)
x = the bytes of data of the chunk: bitlen(x) − 1 in unary, its bit k the adaptive bit chunk_len[min(k, 31)]
    (fails past 31), then the bits of x below its top one, raw, highest first
if c ≥ 0xC0: tree(8) chunk_props: the properties byte, below 225, with lc + lp ≤ 4
```

where `prev` is the kind of the chunk before it (6 for the first). The first chunk resets the
dictionary (`c` is 1 or 0xE0); no other does. A stored chunk has 2^16 bytes of data at most, an
LZMA chunk 2^21, and none more than are left of `D`. After a header the state and `reps` are
reset where `c ≥ 0xA0`, and the data of a stored chunk is skipped; the packets of an LZMA chunk
follow, ending at its end.

**Rebuilding**: the chunks in order, each its header (the control byte with the high bits of
its size, the sizes, the properties byte) and its bytes: a stored chunk's data, or the packets
of an LZMA chunk (those of the record within it) encoded through the model as above; then the
byte 0 where the flag says so. It fails where an LZMA chunk codes in more than 2^16 bytes, or a
chunk `0x80` or `0xA0` finds no properties before it in the segment.

## Implementation limits

`N < 2^32 − 1`. The reference decoder holds `D`, the stream and the match finder's tables:
`4 × 3 × S` bytes for the rings (`S` by the reach `F`), and 12.5 MiB for the heads (132.5 MiB from `N` = 2^20 on: it
keeps the newest 16 positions of each hash of the chains of 4 and 16 bytes together, the same
candidates, so that a walk takes one cache miss for them); natively, those of each part being
decoded at once, with the packets of the parts decoded and not yet encoded (a Wasm module goes
through the parts with one match finder).

The Wasm modules are [part modules](#the-module-as-a-part-module): they hold nothing of the
lengths of their inputs and output, and lay their memory out as a stack with nothing grown. For
each stream, freed after it: the **window** of its data (`R` and an eighth of it or 64 KiB,
64 KiB and 1 KiB of lookahead and 4 KiB of alignment, or all of the data where that is less),
the match finder's tables (one position of each hash, 12.5 MiB) and rings (`12 × S`, by its reach `F`), the
literal model (`2 × 0x300 × 2^(lc+lp)` bytes; for LZMA2 at its largest, 24 KiB), a piece of the
stream (16 KiB) for LZMA, the walk's contexts and candidates, and for LZMA2 a chunk's coder (64
KiB and 16). Before them, the pieces in which the index and the records are read. All of it
follows from the dictionaries and the streams' lengths (a stream shorter than its dictionary
holds less) and the records' starts, so that no size is stored for it: the encoder declares it
as the memory limit (`plan.rs`).

## Formula module

The module is `lzma-recons`, version 4 (version 1 did not have [parts](#parts); in version 2 the
parts' table preceded their bodies, a part's end was the next one's start, and a last distance
beyond `R` was taken; in version 3 the header had no reach byte: the match finder's reach was
`R`). It rebuilds one or more streams, each from its data and its record, and its output is the
streams one after another. A container cuts the output by the streams'
lengths (a Split instruction of the formula), and adds what lies around them (the 13-byte
header of a `.lzma` file; a 7z archive's headers).

**Parameters**: none (empty).

**Inputs**: exactly three, in this order.

| slot | content |
|---|---|
| 0 | the data of every stream, one after another |
| 1 | the records of every stream, one after another |
| 2 | the index: for each stream, the length of its data and the length of its record (varints, LEB128); or empty, for a single stream of all the data and the whole record |

The index must use up the data and the records exactly. The streams are rebuilt independently
of each other.

**Resources**: none.

The module fails on any parameters, any other count of inputs or resources, an index that
does not cut the inputs exactly, a record that fails above, or an output whose length is not
the application's declared output size.

### The module as a part module

Both modules are part modules (the specification's "Module execution"): the inputs are read once
each, in order, through windows of 16 pages, and the output is written once, in order, through
one. A stream is rebuilt as the module reads: its record's header, then the body (and for LZMA
the parts' headers) through the range decoder; its data into a window that holds the bytes from
`R` before the position of the walk to 66 KiB after it (a stored chunk of LZMA2 is copied from
there; nothing reaches farther), dropping the older ones; the stream's bytes put to the output
as the range encoder makes them, in pieces. What the walk looks at in the data is as if it held
all of it: the match finder's queries stay within `R` before the position and `cap` (273) after
it, and what it hashes ahead within 32 + 16. The bytes read of the record must be all of it, as
the data and output must be all of theirs.

The module `lzma2-recons`, version 3 (in version 1 the chunks' headers were a list at the start
of the record; in version 2 the header had no reach byte), is the same for the segments of LZMA2 streams: each
"stream" above is a segment, from its data and [its record](#the-record-of-a-segment), and the
output is the segments' bytes one after another (the segments of an LZMA2 stream, then its end
byte, make the stream). A `.xz` file cuts the output by its blocks' streams, and adds the rest
of the file around them as it is: its headers, the blocks' padding and checks, the index and
the footer. A block of filters before LZMA2 has its data under aqr-bcj's modules of them (the
first filter innermost), where encoding gives back what LZMA2 decodes to.

## Encoder choices (informative)

- `split` decodes the stream, codes its packets as above, and checks that encoding the packets
  gives back the stream (for LZMA2, each segment's chunks) (the decoder's end condition already pins its flush, so this does not
  fail for a stream that decodes).
- Where a match's distance is not the nearest one, `far_rep` names the last distance nearest
  to it (the first of equals), and the offset is from it.
- A stream of more than 64 MiB of data is coded in parts: one starts at the first packet at or
  past each multiple of 64 MiB (one part where two multiples fall within a packet). The
  record is the same whatever the threads that code it; decoding takes a match finder for
  each part being decoded at once, or one going on through them all.
