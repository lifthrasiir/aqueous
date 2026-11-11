# aqr-walk

The machinery that aqr-div3 and aqr-ac3 share: a bit-packed format's syntax written **once** as a
function over [`Io`], run in three ways, and the coder that codes what it walks. This crate is
`no_std` (with `alloc`) so that the recompressors' Wasm modules link it. Its coding is normative
for both formats and is specified below; their FORMAT.md files refer to it.

## The walk

A recompressor of a bit-packed format (an entropy-coded video frame, an audio frame) parses the
bits into elements, codes the elements under contexts, and on the way back writes the same bits.
Two functions that must agree on every detail are a common source of bugs, so the syntax is one
function, `fn walk<I: Io>(io: &mut I, ..)`, which asks the `Io` for each element in order:

| Method | Element |
|---|---|
| `vlc(table, ctx)` | a symbol of a variable-length code |
| `raw(n)` | `n` bits (≤ 32) coded flat, one bit costs one bit |
| `flag(ctx)` | one bit, adaptive under `ctx` |
| `field(n, ctx)` | `n` bits (≤ 16), the top eight adaptive as a binary tree under `ctx`, the rest flat |
| `expect(v, n)` | `n` bits that must be `v`, not coded |
| `derived(n)` | `n` bits that follow from the others (a checksum), not coded; written as zeros when decoding, to be patched |
| `done()`, `total_bits()` | bits consumed or produced so far; the length of the data |

and returns the element, so the walk can branch on it. The implementations:

- `Enc`: reads the element from the original bits (`BitReader`) and codes it. Fails with
  `Error::Parse` when the bits are not what the syntax allows.
- `Dec`: decodes the element and puts its bits to a `BitWriter`.
- `Check`: reads only. A frame is checked first and coded only if it parses to its exact end;
  otherwise the caller keeps its bytes (aqr-div3: a flag then the bytes flat).

`walk` may keep any state it likes between elements, but the state may depend only on elements
already walked (what `Dec` has produced), never on the bits not yet read.

## Coding

Everything below is normative: a stream coded by one implementation is decoded by another only if
each detail is the same. Integers are unsigned; `>>` and `<<` are on the width given; arithmetic
is *wrapping* where marked.

### Probabilities

A probability `Prob` is two 16-bit estimates `a` and `b` of the chance of a 1 bit, both starting
at `1 << 15`. After a bit `x`, with `t = 65535` if `x = 1`, else 0:

```
a += (t − a) >> 4        (arithmetic shift of the signed difference)
b += (t − b) >> 7
```

The probability of a 1 used by the coder is `p12 = clamp((a + b) >> 5, 1, 4095)` out of 4096.

### Range coder

An LZMA-style binary range coder: 32-bit `range` (initially `0xFFFFFFFF`), 64-bit `low`, a `cache`
byte and a count of pending bytes for carries; `TOP = 1 << 24`. To code bit `x` with the 12-bit
probability `p0` of a 0 (`4096 − p12`; 2048 for a flat bit):

```
bound = (range >> 12) × p0
if x = 0: range = bound
else:     low += bound; range −= bound
while range < TOP: range <<= 8; shift_low()
```

`shift_low` is LZMA's: when `low < 0xFF000000` or `low ≥ 2^32`, the cached byte plus the carry
(`low >> 32`) is written, followed by the pending `0xFF` bytes plus the carry; then the cache
takes bits 24–31 of `low`, `low = (low & 0x00FFFFFF) << 8`. The encoder ends with five
`shift_low` calls. The first byte written is always 0. The decoder skips it, reads four bytes
into `code`, and decodes by `code < bound` → 0 (else `code −= bound`, 1), reading a byte when
`range < TOP`; past the end of its input it reads zeros. A coded stream has no length of its
own: its container says where it ends.

### Model

The adaptive probabilities live in a table of `2^bits` `Prob`s, all initial. Contexts are hashed
into it. The binary nodes of a code tree are grouped by four levels: a **line** is 16 consecutive
probabilities (64 bytes; node numbers 1 to 15 in a group of four bits, entry 0 unused), found by

```
x = (ctx << 32) | ((group << 28) ^ prefix)                   (64-bit)
h = x × 0x9E3779B97F4A7C15          (wrapping)
h = (h ^ (h >> 31)) × 0xBF58476D1CE4E5B9   (wrapping)
line = (h >> (64 − bits)) & ~15
```

`ctx` is 32 bits chosen by the format, `group` the number of the group of four bits (0 for the
first four bits of an element) and `prefix` the bits of the element before this group (0 for the
first group). Within a line a node is `gn`, starting at 1 for each group and `gn = gn << 1 | bit`
after each bit. A single bit with a context of its own (`slot(ctx, k)`) is entry 1 of the line of
`(ctx, group 15, prefix k)`.

How the elements are coded:

- `vlc`: the symbol's code, bit by bit, left to right, the bit at position `k` (from 0) under
  the line of `(ctx, k >> 2, first k bits of the code)` at node `gn`. The code is not coded as
  a length and a value: its bits are the symbols of the tree, so an impossible code costs
  nothing and a likely one a fraction of a bit. A code that is no prefix of the table's codes
  is a parse failure when coding, and a corrupt stream when decoding. A table's escape symbol
  is `0xFFFF`.
- `flag`: one bit under the line of `(ctx, 0, 0)`, node 1.
- `field(n, ctx)`: with `k = min(n, 8)`, the top `k` bits as a binary tree: bit `i` under the line
  of `(ctx, i >> 2, the i top bits already coded)` at node `gn`; then the other `n − k` bits flat.
- `raw(n)`: `n` flat bits, most significant first.

## Bits

`BitReader` and `BitWriter` are most significant bit first. A reader yields zeros past its end
for `peek32`; `get` fails instead.

## `Reader`

The buffered, limited input of the modules (`reader::Reader`): bytes from an `aqr_api` input
through a buffer, `varint` (unsigned LEB128), `copy` to an output, and `limit`/`skip_rest` that
bound the bytes one chunk of a coded stream may read, so the range decoder's reads past its
chunk's end give zeros as they do natively. It is the `Source` of a decoder.

## Used by

- [aqr-div3](../../codecs/aqr-div3/README.md): DivX 3 video frames.
- [aqr-ac3](../../codecs/aqr-ac3/README.md): AC-3 syncframes.

Both go through `Model` sized for their own tables (2^22 and 2^21 probabilities).
