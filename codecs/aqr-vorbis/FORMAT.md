# aqr-vorbis format

This document specifies the module `vorbis-recons` of aqr-formula entry formulas, which rebuilds
a Vorbis stream's audio packets from the stream's setup header and a **record** of the values the
packets code. The record codes those values with a binary arithmetic coder under a model of them
(§6), in place of the packets' Huffman codes; the packets are written back from the values bit
for bit.

The document is normative and complete: the module can be implemented from it alone. Sections
marked *informative* describe this implementation's encoder; a decoder needs none of them.

External reference: the Vorbis I specification (Xiph.Org), for the setup header (§4.2.4 and the
codebook, floor 1 and residue headers it holds, §3.2.1, §7.2.2, §8.6.1), the identification
header's channels and block sizes (§4.2.2), and the decode of an audio packet (§4.3.1, with
floor 1 §7.2.3 and residues §8.6.2). Floors of type 0 are not handled.

Contents:

1. [Conventions](#1-conventions)
2. [Overview](#2-overview)
3. [The module](#3-the-module)
4. [The record](#4-the-record)
5. [A packet's values](#5-a-packets-values)
6. [The model](#6-the-model)
7. [Encoding](#7-encoding-informative) *(informative)*

## 1. Conventions

- Integers are exact unless marked otherwise. `a / b` of integers is their quotient rounded
  toward zero; `a >> s` of a signed integer rounds toward minus infinity (an arithmetic shift).
  `bitlen(x)` is the number of bits of `x` (0 for 0); `ilog` is Vorbis I's (§9.2.1), the same.
- Bits of a packet are as Vorbis packs them: least significant bit of a byte first, an n-bit
  field's bit 0 first.
- **varint**: unsigned LEB128, least significant group first, at most 10 bytes.
- `clamp(x, a, b)` is `min(max(x, a), b)`.

## 2. Overview

A stream is coded in **packets** (Vorbis I §4.3), framed here each after its length: 16 bits,
little-endian, then the packet's bytes. The module outputs the framed packets of one stream,
all of them audio packets of one setup header.

A decoder of a packet (§4.3.1) reads, in an order the packet's own values decide, fields of a
fixed number of bits and entries of codebooks, coded by the books' Huffman codes. The values of a
packet are those, in that order (§5). Where the packet ends inside a field (end-of-packet), the
values stop before it. A packet is its values written in turn (a field's bits, an entry's
codeword), then its **tail**: the bits after the last value up to the end of its last byte.

The record holds, per packet, the values, coded under the model, and the tail. The model is
fed the packets in order and predicts each decision from the stream so far.

## 3. The module

- **Module**: name `vorbis-recons`, version 1.
- **Params**: `varint channels` (1 to 255), then a byte: the short block size's exponent in its
  low 4 bits, the long one's in its high 4, each from 6 to 13 (64 to 8192 samples), the short
  not above the long. Nothing after.
- **Resources**: none.
- **Inputs**: exactly 2: `setup` (class `vorbis/setup`), the setup header (a whole packet of
  type 5, Vorbis I §4.2.4), parsed for the params' channels; `record` (class `vorbis/record`), §4.
- **Output**: the framed packets, `len` bytes (the declared length).

The setup header must parse under Vorbis I, with these restrictions: its floors are of type 1;
every codebook's codewords are assigned as §3.2.1 assigns them (in entry order, each used
entry the lowest codeword of its length that neither is prefixed by one taken nor prefixes one;
a book of a single used entry takes the codeword of zeros of its length); no codeword length
above 32; no assignment is overspecified. A tree with unused paths is allowed.

## 4. The record

The record is one binary arithmetic code (carry-less, 32 bits), as aqr-lzo's FORMAT.md §5.1
defines it: with probability `p1` (of 16 bits) that a decision is 1,

```
x1 = 0, x2 = 2^32 − 1;  the decoder reads 4 bytes into x (zeros past the end)
code a decision:
  xmid = x1 + (((x2 − x1) × p1) >> 16)
  1: x2 = xmid;  0: x1 = xmid + 1        (the decoder reads 1 iff x ≤ xmid)
  while x1 and x2 agree in their top byte:
    the encoder writes x2 >> 24; x1 <<= 8; x2 = x2 << 8 | 0xFF; the decoder shifts in a byte
end: the encoder writes (x1 >> 24) + 1
```

All decisions below are coded with probabilities of the model (§6), which starts afresh for
each record. The **frame** decisions use counters of their own (§6.1): `more` (starting at
probability 65000, count 4), `short` (64, 4), `tail[0]` and `tail[1]` (64, 4), and `count[0..64]`
(the default). A frame decision of counter `k` is coded with probability `p16(k)` and `k` is then
updated with it.

A **number** `v` (below 2^32) is coded as: for `n` from 0, while `n < 32`, the decision
`n < bitlen(v)` with counter `count[n]` (the run ends at the first 0); then, where `bitlen(v) =
L > 0`, its bits below the leading one, bit `L − 2` first down to bit 0, bit `i` with counter
`count[32 + min(i, 31)]`.

The record is, per packet in order:

1. the decision 1 (`more`);
2. whether the packet is cut short (`short`);
3. where it is, the number of its values (a number);
4. its values, under the model (§6), the decode driven as §5 says;
5. where the packet is whole, the number of its bytes past the fewest that hold its values;
   where it is cut short, the number of bits of its tail;
6. the tail's bits, each with counter `tail[0]` (a whole packet) or `tail[1]` (cut short).

then the decision 0 (`more`). A whole packet's tail is its padding (0 to 7 bits, up to the end
of the byte its last value ends in) and 8 bits per byte past it.

The decoder writes each packet's values and tail as §2 says and outputs its length (16 bits,
little-endian) and its bytes. It fails where:

- a value is not one of a packet (§5: an entry without a codeword; a field's value of more bits
  than the field);
- the decode of a whole packet ends short of the values, or a short one's decode reads all its
  values and ends without a missing one (its count not used up exactly);
- a packet's bits (its values and tail) do not fill whole bytes, or are none, or are more than
  65535 bytes, or the packets' bytes with their lengths exceed `len`, or end short of it;
- the decoder reads more than 4 bytes past the end of the record.

## 5. A packet's values

The decode of an audio packet of Vorbis I §4.3.1 reads, in order (each a **field** of `n` bits,
or an **entry** of a book, read by its Huffman code), with `C` the channels:

1. the packet type, 1 bit (field `Type`; 0, else the packet is not taken);
2. the mode number, `ilog(modes − 1)` bits (`Mode`), which must be a mode of the setup;
3. for a mode of long blocks, the previous and next window flags, 2 bits (`Window`);
4. for each channel `ch` in order, its floor (of the mode's mapping's submap of `ch`): 1 bit
   (`Nonzero`); where 1, Y[0] and Y[1], each `ilog(range − 1)` bits with `range` 256, 128, 86 or
   64 for multipliers 1 to 4 (`Y01`, `i` 0 and 1); then for each partition `part`, where its
   class has subclasses, an entry of its master book (`Master`), and each of the class's `dims`
   values `Y[i]` that has a book, an entry of it (`Y`), as §7.2.3 reads them;
5. each submap's residue, as §8.6.2 reads it, over the vectors of the channels of the submap in
   order (`v` their index there), those of floors unused and not coupled to a used one not
   decoded; for type 2 one vector of all of them interleaved, not read at all where none is
   decoded. In pass 0, at the start of each run of partitions a classword covers, an entry of
   the classbook per vector decoded (`Class`); then for each partition `part` and vector, where
   its class has a book in pass `pass`, `reads` entries of it (`Vq`, the `k`-th of them `k`),
   `reads` the partition size divided by the book's dimensions (type 0: rounded down; types 1
   and 2: up).

Where the packet ends inside a field or a codeword (end-of-packet), the decode stops there.

A `Vq` entry's values are at **frequencies** of its vector: with `base = begin + part ×
partition` (the residue's begin and partition size), `dims` the book's dimensions and `step =
partition / dims`, value `j` of the entry is at `base + k + j × step` for type 0 and at `base +
k × dims + j` for types 1 and 2 (a type-2 vector interleaving its channels).

## 6. The model

### 6.1 Primitives

**squash** maps a logit `d` (8 fractional bits) to a probability of 12 bits: 4095 where `d >
2047`, 1 where `d < −2047`, else with `T = [1, 2, 3, 6, 10, 16, 27, 45, 73, 120, 194, 310, 488,
747, 1101, 1546, 2047, 2549, 2994, 3348, 3607, 3785, 3901, 3975, 4024, 4050, 4068, 4079, 4085,
4089, 4092, 4093, 4094]`, `w = d & 127` (two's complement), `i = (d >> 7) + 16`:
`(T[i] × (128 − w) + T[i + 1] × w + 64) >> 7`.

**stretch** is a table of 4096 entries: with `pi = 0`, for `x` from −2047 to 2047, `v =
squash(x)`, every entry from `pi` to `v` (where `pi ≤ v`) is `x`, and `pi = v + 1`; the entries
from `pi` to 4095 are 2047. `stretch(p)` is the entry `clamp(p, 0, 4095)`.

**p16(p)** of a probability `p` of 12 bits is `clamp(clamp(p, 1, 4095) × 16, 32, 65504)`, the
probability given the coder.

A **counter** is a probability `p` of 16 bits and a count `n`; by default `p = 32768`, `n = 0`;
"starting at `q`, count `m`" is `p = clamp(q, 1, 65535)`, `n = m`. Its probability of 12 bits is
`p >> 4`. Updated with bit `b`: `p = p + (t − p) / (n + 2)` with `t` 65535 for 1 and 0 for 0
(the quotient rounded toward zero), then `n = n + 1` where `n < 60`. A decision coded with a
counter alone has probability `p16(p >> 4)`.

A **mixer** of 8 inputs has 16 sets of 8 weights (16 fractional bits), each starting at
`[16384, 0, 0, 0, 16384, 16384, 16384, 0]`. With inputs `x[0..8]` and set `s`: `dot = Σ w[s][i] ×
x[i]` (in 64 bits), `p = squash(dot >> 16)`. Updated with bit `b`: `err = b × 4096 − p`; each
`w[s][i] += (x[i] × err × 10) >> 16` (in 32 bits).

An **APM** has contexts of 33 entries of 16 bits, entry `i` starting at `squash((i − 16) × 128) ×
16`. For a logit `st` and context `c`: `u = clamp(st, −2047, 2047) + 2048`, `lo = u >> 7`, `w = u
& 127`; its probability is `clamp((t[c][lo] × (128 − w) + t[c][lo + 1] × w) >> 11, 1, 4095)`, and
the entry it will update is `t[c][lo + (w >> 6)]`. Updated with bit `b` (the rate 6): `g = b ×
(65536 + 64 − 2)`, `t = t + ((g − t) >> 6)`.

### 6.2 Fields

Each kind of field has a tree of 512 counters (default): `Type`, `Mode`, `Window`, `Nonzero`,
`Y01` with `i = 0`, and `Y01` with `i = 1`. A field of `n` bits is coded its most significant bit
first: with `node = 1`, each bit with counter `min(node, 511)`, then `node = node × 2 + bit` where
`node < 256`.

The model keeps the **mode** of the packet: a `Mode` field's value, once coded.

### 6.3 Entries by their tree

An entry other than a `Vq` entry of a book of digits (§6.4) is coded down its book's decoding
tree: from the root, at each node, a bit chooses a child (0 the codeword's next bit 0). A node of
one child takes it without a decision; else the decision is coded with the node's counter. The
walk ends at an entry.

Each book's node counters start at the probability its code gives the 1 side, count 12: with
`weight(len) = 2^(40 − len)` of a codeword of `len` bits, `W(side)` the weights of the entries
below that side (0 for no child), `p = (W(1) × 65536) / max(W(0) + W(1), 1)`.

### 6.4 VQ entries by their digits

A book has **digits** where its lookup is of type 1, `q = lookup1_values(entries, dims)` is from
2 to 64, `q^dims` is at most its entries, and it has no used entry at or past `q^dims`. Entry `e`
is then its digits `e_0 … e_(dims−1)`, `e = Σ e_j × q^j`; digit `j` is the value at the
frequency of value `j` (§5). Of each digit, for each value `x` of the digits before it as a
number (`Σ_(i<j) e_i × q^i`, its **prefix**), the book's **row** is the weight of each digit
value: `row[x] = Σ weight(len(e))` over the used entries `e` of that prefix and digit value.

The **centre** of a digit is `h = q / 2`, its **magnitude** `|x − h|`.

A digit is coded as decisions (each of a **node**):

1. node 0: whether it is `h`, of weights `row[h]` (yes) and the rest (no); where yes, done;
2. node 1: whether it is below `h`, of weights `Σ_(x<h) row[x]` and `Σ_(x>h) row[x]`;
3. for `m = 1, 2, …`: node `2 + min(m − 1, 5)`: whether its magnitude is above `m`, of weights
   (of that side of the centre) the rows of magnitudes above `m`, and the row of magnitude `m`;
   where no, it is `h ± m`.

A decision whose weights are not both above 0 is not coded: it is the side of weight above 0.

**Magnitudes.** The model keeps, per vector `v` and pass, the magnitudes of the digits coded in
the packet, by frequency, for the packet's size (`blocksize / 2` of its mode, times the vector's
channels for type 2), as UNKNOWN (255) where none is; and the previous packet's of each block
size. They start empty; a `Mode` field empties this packet's; at the end of a packet, each kept
for this packet (not empty) becomes the previous one of its block size, and this packet's are
emptied. A digit coded stores `min(magnitude, 254)` at its frequency (if within the size).

**Contexts** of a digit `j` of book `B` (its frequency `f`, vector `v`, pass `p`), where
`bucket(m)` is `m` for 0 to 2, 3 for 3 to 254, 4 for UNKNOWN (outside the size: UNKNOWN):

- `f1`, `f2`: the buckets at `f − 1` and `f − 2` of this packet's (`v`, `p`), 5 where `f` is below
  1 (2);
- `pp`: the bucket at `f` of the previous packet's (`v`, `p`) of this block size;
- `pv`: the bucket at `f` of this packet's (`v`, `p − 1`), 5 in pass 0;
- `nb`: the bucket at `f` of this packet's (`v − 1`, `p`), 5 for vector 0;
- `band`: `min((f × 8) / max(end, 1), 7)`, `end` the residue's end.

They make three 64-bit keys: `c0 = B << 32 | j << 24 | f1 << 8 | f2`, `c1 = 1 << 60 | B << 32 |
pp << 8 | pv`, `c2 = 2 << 60 | B << 32 | nb << 8 | band`.

**A decision** of node `node` and weights `w1` (yes) and `w0` (no), in pass `p`:

1. `cut = max(bitlen(w0 + w1) − 50, 0)`, `w0 >>= cut`, `w1 >>= cut`; the **prior** is `(w1 ×
   4096) / max(w0 + w1, 1)`.
2. Each key `c_i` gives a slot `s_i = ((c_i ⊕ node << 16) × 0x9E3779B97F4A7C15 (wrapping in 64
   bits)) >> 48`, of 16 bits, in table `i` of 65536 counters (default), and APM `i` (of 4096
   contexts) context `s_i & 4095`.
3. With `sp = stretch(prior)`, the mixer's inputs are `sp`, the stretch of the three counters'
   probabilities (12 bits), the stretch of the three APMs' probabilities for `sp`, and 256; its
   set is `node × 2 + (1 if p > 0 else 0)`.
4. The decision is coded with `p16` of the mixer's probability; then the mixer, the three APMs
   and the three counters are updated with it.

## 7. Encoding *(informative)*

`aqr_vorbis::formula::packets` takes the framed packets at the start of data, up to its end or a
length of 0 (FSB5's end of a sample's packets), and the record of them; it does not take a
stream where a packet does not parse under the setup (aqr-fsb then keeps it as it is).
`aqr_vorbis::codec::encode` parses each packet to its values and tail (end-of-packet giving a
short one) and codes them as §4 says.

Measured on The Crew's FSB5 banks (aqr-fsb), 1,491 MB of Vorbis packets with their lengths: on
1,548 packets of 30 small banks the record is 89.9% of them; on a 12.5 MB corpus of small banks
91.5%. Of the values (12 large banks, 119 MB of packets), against their Huffman codes: VQ
entries 95.7%, floor Y 89.7%, classwords 84.7%, master entries 68.0%, Y[0..1] 63.4%. The model
codes about 2 MB/s on one thread.
