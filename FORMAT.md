# pdf-recons format

This document specifies the file written by `pdfr` (pdf-recons) completely enough to
reimplement both directions. A file holds one input file, the *original* (normally a PDF file,
but any bytes are allowed), which decompression must restore bit for bit.

The original is cut into a *skeleton* and *items*. Items are byte ranges of stream data, coded
on their own, and the skeleton is everything between them. An item is kept as it is, refers to
an earlier item, or is rebuilt by one of these coders:

- **tok**: a FlateDecode stream, as its DEFLATE tokens, entropy coded by this format
  ([Token coding](#token-coding));
- **full**: a FlateDecode stream, as its inflated data plus a flate-recons record;
- **png**: a FlateDecode image with PNG predictors, as a png-recons file;
- **jpeg**: a DCTDecode image, as a jpeg-recons container.

The nested formats are specified by their own documents, which this one refers to rather than
repeats:

- flate-recons' `FORMAT.md`: the *record* (section *The record*), and the *header stream*
  that token coding uses for dynamic Huffman headers (section *Header stream*);
- png-recons' `FORMAT.md`: the png-recons file;
- jpeg-recons' `FORMAT.md`: the JRC container.

Sections 1–6 are normative for decoders. Section 7 is about the compressor: everything there
is the encoder's free choice within the constraints given.

## 1. Conventions

- Integers are unsigned unless stated. `u16` and `u64` fields are little endian; `u32 BE` is
  big endian.
- **varint**: unsigned LEB128. 7 bits per byte, least significant group first, bit 7 set on
  every byte but the last. Values are `u64`. Readers accept up to 10 bytes, and more is an
  error. Bits above bit 63 are dropped. Writers emit the shortest form; readers accept longer
  ones.
- **xxh3** is XXH3-64 (`XXH3_64bits`) with seed 0 and the default secret.
- **zstd** data is one or more zstd frames (RFC 8878).
- `bitlen(v)` is the number of significant bits of `v ≥ 0` (`bitlen(0) = 0`,
  `bitlen(5) = 3`). `x div y` and `x mod y` are floor division and its remainder, and
  `ceil(x / y)` rounds up.
- **Bit streams** are packed least significant bit first. An `n`-bit field holding `v` is
  written least significant bit first. A bit stream is padded with zero bits to a byte
  boundary. Reading past the end of a bit stream gives zero bits, and a part that consumes
  bits past the end (past its last byte, padding included) is invalid. Bits left unread at
  the end of a bit stream are allowed.
- **gamma(v)** (`0 ≤ v < 2^32`) is Elias gamma of `u = v + 1` in a bit stream. With
  `k = bitlen(u) − 1`, it is `k` zero bits, a one bit, then the `k` bits of `u` below its
  leading one, **most significant first**. For example, gamma(5) is the bits 0, 0, 1, 1, 0 in
  that order, the byte `0x0c` on its own. png-recons' gamma (fields least significant first)
  and flate-recons' (ones, then a zero) are different codes.
- `LEN_BASE[c]`, `LEN_EXTRA[c]` (length codes `c` = 0–28, DEFLATE symbols 257–285) and
  `DIST_BASE[c]`, `DIST_EXTRA[c]` (distance codes 0–29) are DEFLATE's (RFC 1951 §3.2.5).
- **Adler-32** is zlib's checksum (RFC 1950), written as `u32 BE`.

## 2. File

```text
offset 0   6 bytes   magic: 69 04 69 75 FD B4        (base64 "aQRpdf20")
offset 6   u16       version
offset 8   streams:  stream 0, stream 1, ..., stream N−1
stream  =  varint length, then `length` bytes of data
```

The version selects the layout of the streams:

| version | layout |
|---|---|
| 0 | coded, section 3 (N = 18) |
| 1 | stored (N = 1) |
| other | invalid |

The number of streams and the meaning of stream *k* follow from the version and the contents of
streams 0..*k*−1. A file ends exactly after its last stream. Further bytes, a missing stream,
or a stream extending past the end make the file invalid.

**Stored** (version 1): stream 0 is the original, as it is.

## 3. Coded files (version 0)

### 3.1 Streams

| # | stream | contents |
|---|---|---|
| 0 | head | 3.2 |
| 1 | table | the items (3.3) |
| 2 | skeleton | the bytes between items |
| 3 | raw | the bytes of `RAW` items, in order |
| 4 | jpeg | the jpeg-recons containers of `JPEG` items, in order |
| 5 | png | the png-recons files of `PNG` items, in order |
| 6–11 | pools | the inflated data of `FULL` items of class 0–5 (3.4), in order |
| 12 | meta | the flate-recons records of `FULL` items, in order |
| 13 | token layout | 6.2 |
| 14 | token tables | 6.4 |
| 15 | token symbols | 6.5 |
| 16 | token raw bits | 6.6 |
| 17 | token headers | 6.8 |

Streams 1–17 are *coded* as the head says: their *data* is the stream's bytes, or what zstd
decoding of them gives.

### 3.2 Head

```text
varint  length of the original
u64     xxh3 of the original
per stream k = 1 .. 17:
  u8      method: 0 = stored, 1 = zstd
  [method 1: varint  decoded length]
```

A stored stream's data is its bytes. A zstd stream's bytes must decode to exactly the decoded
length. Other methods are invalid. The head must be used up exactly by these fields.

Decoders may reject sizes beyond these limits, which no valid file exceeds: the original's
length at most 2^40; the data of the pools (6–11) at most 1032 times the original's length,
and of every other stream at most the original's length plus 2^20.

### 3.3 Table

The table is a sequence of items, until its data ends. Each item is:

```text
varint  gap      bytes of skeleton before the item
u8      mode
...     the mode's fields
```

| mode | name | fields |
|---|---|---|
| 0 | `RAW` | varint `n` |
| 1 | `DUP` | varint `back` |
| 2 | `FULL` | u8 `flags`, [2 bytes `zlib`], u8 `class`, varint `plain`, [varint `bpp`, varint `stride`], varint `mlen` |
| 3 | `JPEG` | varint `n` |
| 4 | `TOK` | u8 `flags`, [2 bytes `zlib`], u8 `class`, varint `plain`, [varint `bpp`, varint `stride`] |
| 5 | `PNG` | u8 `colour type`, u8 `bit depth`, varint `width`, varint `height`, varint `n` |

Other modes are invalid. Field constraints:

- `gap` and every `n` at most the original's length; `back` from 1 to the item's index (the
  number of items before it); `plain` at most 1032 times the original's length; `mlen` at most
  2^40.
- `flags`: bit 0, the 2 `zlib` bytes follow; bit 1, the item ends with an Adler-32; bit 2, the
  `bpp` and `stride` fields follow (pixels, `TOK` only). Other bits must be 0, and bit 2 must
  be 0 for `FULL`.
- `class` from 0 to 5: content, font, image, object stream, cross-reference stream, other data.
- Pixels: `1 ≤ bpp ≤ 64`, `bpp ≤ stride ≤ 2^34`, and `stride` a multiple of `bpp`.
- `PNG`'s colour type, bit depth, width and height describe the image (section 7); a decoder
  does not need them, but the varints must still be well formed.

### 3.4 Item bytes

Each item gives its *bytes*, taking its data from the streams in table order: every item of a
mode takes the next bytes of its streams, from where the item before it left off.

- `RAW`: the next `n` bytes of the raw stream.
- `DUP`: the bytes of the item `back` items before it.
- `JPEG`: the next `n` bytes of the jpeg stream are a JRC container. Its decompression is the
  item's bytes.
- `PNG`: the next `n` bytes of the png stream are a png-recons file. Its decompression is a
  PNG file `P`. With `m` = `u32 BE` at `P[33..37]` (the length of the second chunk), the item's
  bytes are `P[41 .. 41 + m]`, which must lie within `P`. (This is the data of the IDAT chunk
  of the PNG file in section 7.3.)
- `FULL` and `TOK`: the `zlib` bytes (if flagged), the DEFLATE stream, and the Adler-32 of the
  inflated data (if flagged). The inflated data is `plain` bytes long.
  - `FULL`: the inflated data is the next `plain` bytes of the pool of the item's `class`, and
    the record the next `mlen` bytes of the meta stream. flate-recons rebuilds the DEFLATE
    stream from the two (flate-recons' `FORMAT.md`, *Rebuilding the stream*).
  - `TOK`: the next stream of the token decoder (section 6) gives the DEFLATE stream and its
    inflated data.

### 3.5 Decompression

1. Read the magic, the version and the streams, and decode streams 1–17.
2. For each item of the table: append the next `gap` bytes of the skeleton, then the item's
   bytes. The output must never exceed the original's length.
3. Append the rest of the skeleton.
4. Check that the raw, jpeg, png, meta and pool streams were used up exactly, that the token
   symbol stream ended as section 6.5 requires (if any item is `TOK`), that the output is the
   original's length, and that its xxh3 is the head's. Otherwise the file is invalid, and the
   output must not be returned.

If the token streams (13–17) are all empty, there is no token decoder, and a `TOK` item is
invalid. Otherwise the token decoder is set up from them (6.4) before the first item, and the
symbol stream must end as 6.5 requires even if no item is `TOK`. The encoder leaves them all
empty exactly when no item is `TOK`.

Every `gap`, `n`, `plain` and `mlen` must also fit in what is left of its stream. The layout,
tables, raw bits and headers streams need not be used up: what follows the last item's bits is
not read.

## 4. Token coding: overview

All `TOK` items of a file are coded together, in table order, as one sequence of *symbols* with
static tables and one rANS coder. A `TOK` item's DEFLATE stream is kept as its exact LZ77
tokens and block structure, so rebuilding it needs only Huffman coding:

- the *layout* stream: the blocks of each item and its matches of length 258 sent as 284 + 31;
- the *tables* stream: the frequencies of the tables;
- the *symbols* stream: literals, length codes and distance codes, rANS coded;
- the *raw bits* stream: the extra bits of lengths and distances;
- the *headers* stream: the dynamic Huffman headers, with flate-recons' header coder.

Each part carries over from item to item: the bit streams continue where the previous item
left them, the rANS states run on, and the header coder's recipe carries over too.

## 5. Contexts

### 5.1 Slots

A `TOK` item has a *slot*: 6 if it has pixels (flags bit 2), else its class (0–5). Each slot has
its own tables: literal tables (literals and length codes, alphabet 285: bytes 0–255, then
length codes 0–28 as 256–284) and 8 distance tables (alphabet 30).

A position `p` of the item's inflated data (from 0) has a *fine context* `t`, which gives the
literal table, and a prediction `pred`. The *resolution* of the slot (6.4) maps fine contexts
to the tables that are coded:

| slot | fine contexts | resolution 0 | 1 | 2 | 3 |
|---|---|---|---|---|---|
| 0–5 | 256 | `t` (256 tables) | `MAP[t]` (32) | `MAP[t] div 4` (8) | 0 (1) |
| 6 | 96 | `t` (96 tables) | `t mod 12` (12) | `t div 12` (8) | 0 (1) |

`MAP` is `TEXT` for classes 0, 3, 4 and 5, and `BIN[b] = b div 8` for classes 1 and 2.
`TEXT[b]` is:

| value | bytes | value | bytes |
|---|---|---|---|
| 0 | `0` `1` | 16 | `A`–`F` |
| 1 | `2`–`4` | 17 | `G`–`M` |
| 2 | `5`–`7` | 18 | `N`–`S` |
| 3 | `8` `9` | 19 | `T`–`Z` |
| 4 | space | 20 | `/` |
| 5 | LF (0x0a) | 21 | `(` `)` |
| 6 | CR (0x0d) | 22 | `<` `>` |
| 7 | TAB (0x09), FF (0x0c), NUL (0x00) | 23 | `[` `]` `{` `}` |
| 8 | `.` | 24 | `\` |
| 9 | `-` | 25 | `%` `#` |
| 10 | `+` | 26 | `*` `'` `"` `_` |
| 11 | `,` | 27 | every other byte below 0x80 |
| 12 | `a`–`f` | 28 | 0x80–0x9f |
| 13 | `g`–`m` | 29 | 0xa0–0xbf |
| 14 | `n`–`s` | 30 | 0xc0–0xdf |
| 15 | `t`–`z` | 31 | 0xe0–0xff |

### 5.2 Slots 0–5: the byte before

`t = d[p − 1]`, the byte before in the item's inflated data `d`, and 0 at `p = 0`. `pred = 0`.

### 5.3 Slot 6: pixels

The inflated data is rows of `stride` bytes, of pixels of `bpp` bytes, with no filter bytes.
Position `q` has column `col(q) = q mod stride` and channel `ch(q) = q mod bpp`. Its neighbours
are left `a = d[q − bpp]` if `col(q) ≥ bpp`, up `b = d[q − stride]` if `q ≥ stride`, and up-left
`c = d[q − stride − bpp]` if both, each 0 otherwise. Then:

- `med(q) = clamp(a + b − c, min(a, b), max(a, b))` (LOCO-I's median predictor), and
- `act(q) = |a − c| + |b − c|`.

At position `p`, with `ch = ch(p)`:

- `pred = med(p)` if `ch = 0`, else `med(p) + d[p − 1] − med(p − 1)`, taken mod 256;
- `t = (4 × after + min(ch, 3)) × 12 + min(bitlen(act(p)), 11)`.

`after` is 1 if the last token of the item was a match, else 0: it is 0 at the start of the
item, a match sets it, and a literal or a stored byte clears it.

## 6. Token coding

### 6.1 One item

A `TOK` item of `plain` bytes is decoded as:

1. its layout (6.2);
2. its blocks' tokens and stored bytes (6.3), giving the inflated data and the tokens;
3. its dynamic blocks' headers (6.8);
4. its DEFLATE stream, written (6.9).

### 6.2 Layout

Per item, in the layout bit stream:

| field | |
|---|---|
| gamma | number of blocks |
| 1 bit | 1 if exactly the last block is final, else 0 |
| per block: | |
| 1 bit | the block's BFINAL, only if the bit above is 0 |
| 2 bits | the block type: 0 stored, 1 fixed, 2 dynamic (3 is invalid) |
| stored: 8 bits | the value of the padding bits before LEN |
| stored: 16 bits | LEN |
| fixed or dynamic: 1 bit | 1 if the block has as many tokens as the item's previous fixed or dynamic block (0 tokens for the first), else 0 |
| fixed or dynamic: gamma | the token count, only if the bit above is 0; the counts of all blocks add up to at most `plain` |
| 8 bits | the value of the padding bits after the last block |
| gamma | the number of matches sent as 284 + 31, at most the item's token count |
| gamma each | their token indices: the first as it is, each other as the difference to the one before, which must be at least 1 |

The item's tokens are numbered from 0 over its fixed and dynamic blocks in order. Stored bytes
are not tokens. Every listed token must be a match of length 258.

Padding values are the bits (least significant first) from the end of the block header (or of
the last block) to the next byte boundary. A value must fit in that many bits, or the stream is
not rebuilt as it was.

### 6.3 Tokens

The item's blocks are decoded in order, from position `p = 0`, into the inflated data `d`.
Symbols come from the symbol stream (6.5), from the item's slot's tables (5.1); `L(t)` is the
literal table of fine context `t` and `D(k)` distance table `k`.

- **A stored block**: LEN bytes. Each is a symbol `s` from `L(t)`, which must be below 256, and
  `d[p] = (s + pred) mod 256`.
- **A fixed or dynamic block**: its count of tokens. Each starts with a symbol `s` from `L(t)`:
  - `s < 256`, a literal: `d[p] = (s + pred) mod 256`.
  - `s ≥ 256`, a match: with `lc = s − 256`, `len = LEN_BASE[lc] + r` where `r` is the next
    `LEN_EXTRA[lc]`-bit field of the raw bits. Then `dc` is a symbol from `D(min(lc, 7))`, and
    `dist = DIST_BASE[dc] + r'` with `r'` the next `DIST_EXTRA[dc]`-bit field of the raw bits.
    `len` must be at most 257 for `lc < 28`: length 258 is code 28 alone. `dist ≤ p` is
    required. The match copies `len` bytes from `dist` back, one by one (they may overlap).

`t` and `pred` are those of the position `p` where the token or byte starts (section 5), and
`p` advances by 1 for a literal or stored byte and by `len` for a match. No byte may be written
at or past `plain`. When the blocks are done, `p` must equal `plain` and the raw bits must not
have been read past their end.

### 6.4 Tables

The tables bit stream is read once, before the first `TOK` item. For each slot 0–6 in order:

| field | |
|---|---|
| 1 bit | 1 if the slot is used |
| used: 2 bits | its resolution `res` |
| used: per literal table `i` (5.1, as many as `res` gives) | 1 bit, 1 if the table is used; if so, its levels (285 of them) |
| used: per distance table `k` = 0–7 | 1 bit, 1 if the table is used; if so, its levels (30 of them) |

The bits of a used slot must not pass the end of the stream; the flags of unused slots past
the end read as 0. Encoders never code a symbol from a table or slot that is not used. The
reference decoder treats such a table as one whose only symbol is 0, with frequency 4096: it
gives 0 and leaves the state as it is (6.5).

**Levels.** Each symbol of a table's alphabet has a level: 0 (absent) or 1 to 63. They are
read with `prev = 0` at the start of the table, as a series of gamma codes `g`:

- `g = 0`: gamma(`r`) follows. The next `r + 1` symbols have level 0. The run must not pass the
  alphabet's end. `prev` is left as it is.
- `g > 0`: with `z = g − 1`, the level is `prev + (z + 1) div 2` for odd `z` and `prev − z div 2`
  for even `z`. It must be in 1 to 63. It becomes `prev`.

**Frequencies** (summing to 4096) come from the levels. Level `l ≥ 1` has the weight

    W(l) = (F[(l − 1) mod 4] << ((l − 1) div 4)) >> 8,   F = [65536, 77936, 92682, 110218]

With `S` the sum of the weights of the symbols present, each present symbol has frequency
`max(1, floor(W(l) × 4096 / S))` (64-bit arithmetic), and the others 0. Then, while the total
`T` is not 4096, the most frequent symbol `m` (the lowest index among equals, chosen afresh each
time) is adjusted: if `T < 4096`, it gains `4096 − T`; if `T > 4096`, it loses
`min(T − 4096, f[m] − 1)`, and if that is 0 the table is invalid. If no symbol is present,
symbol 0 has frequency 4096. Symbol `s` takes the slots `cum[s]` to `cum[s] + f[s] − 1`, where
`cum[s]` is the sum of the frequencies before it.

### 6.5 Symbol stream

The symbols of all `TOK` items are coded in one stream, in decoding order, with four
interleaved 32-bit rANS states, 12-bit probabilities and 16-bit renormalisation, with
`L = 2^15`. Words are `u16`, little endian; a stream of odd length is invalid.

**Decoding.** The stream starts with the four states `x0`, `x1`, `x2`, `x3`, each as two words:
its high 16 bits, then its low 16 bits. Symbol `i` (from 0, over the whole file) is decoded
with state `x[i mod 4]`, from its table:

1. `slot = x mod 4096`, giving the symbol `s` with `cum[s] ≤ slot < cum[s] + f[s]`;
2. `x = f[s] × (x >> 12) + slot − cum[s]`;
3. if `x < 2^15`, `x = (x << 16) | the next word`.

Reading past the end gives zero words. After the last `TOK` item, the stream must have been
read exactly to its end, and all four states must be `2^15`.

**Encoding** runs backwards from the last symbol, every state starting at `2^15`. To encode `s`
in state `x`: if `x ≥ f[s] << 19`, emit the word `x mod 2^16` and set `x = x >> 16`; then
`x = ((x div f[s]) << 12) + (x mod f[s]) + cum[s]`. The stream is the final states (`x0`
first) followed by the emitted words in the reverse of their emission order.

### 6.6 Raw bits

A bit stream of the matches' extra bits, in decoding order: for each match, its length's then
its distance's.

### 6.7 Symbol frequencies of a block

Each fixed or dynamic block has symbol frequencies, from its tokens: 288 literal/length counts
(each literal's byte, the end-of-block symbol 256 counted once, and each match's DEFLATE length
symbol: 284 for the matches the layout lists, else `257 + lc` with length 258 as 285) and 32
distance counts (each match's distance code).

### 6.8 Headers

The headers bit stream holds the Huffman header of every dynamic block, over all `TOK` items
in order, coded with flate-recons' header coder (flate-recons' `FORMAT.md`, *Header stream*,
with that document's bit conventions). Each header is read against its block's frequencies
(6.7), after the item's tokens. The coder's recipe starts at its initial value once per file
and carries over from block to block and from item to item.

### 6.9 Writing the DEFLATE stream

Mark the listed matches as sent as symbol 284 with extra bits 31, and write each block of the
item: BFINAL (1 bit), the type (2 bits), and

- stored: the padding value up to the byte boundary, LEN (16 bits), its one's complement
  (16 bits), then the next LEN bytes of the inflated data;
- fixed: the block's tokens with DEFLATE's fixed codes, then the end of block (256);
- dynamic: the header (HLIT − 257 in 5 bits, HDIST − 1 in 5 bits, HCLEN − 4 in 4 bits, the
  code-length code lengths in 3 bits each in DEFLATE's permuted order, then the run-length
  coded lengths with their extra bits), the tokens with its codes, then the end of block.

A literal is its byte's code. A match is its length symbol's code and extra bits, then its
distance code's code and extra bits. Every symbol written needs a code (a nonzero length), or
the file is invalid. Codes are canonical (RFC 1951 §3.2.2) and written most significant bit
first, as DEFLATE does. After the last block comes the final padding value up to the byte
boundary.

## 7. Compressor

Everything here is the encoder's choice; a decoder relies only on sections 1–6 and the
checksum. The source is the reference for the heuristics named here.

### 7.1 Items

- **Streams** (`scan.rs`): any `N G obj << … >> stream` whose dictionary parses. The data
  starts after the end of line following `stream` and runs for `/Length` bytes if that is a
  direct integer followed (after up to 4 white-space bytes) by `endstream`, else to the end of
  line before the next `endstream` or the end of the file. Streams are taken in file order and
  never overlap.
- A stream becomes an item unless its data is shorter than 64 bytes, it has no filter, or its
  first filter is ASCIIHex or ASCII85. Its first filter decides how it may be coded: FlateDecode
  (`/FlateDecode`, `/Fl`) as `TOK`, `FULL` or `PNG`, DCTDecode alone as `JPEG`, anything else
  `RAW`. A stream equal to an earlier item's data is a `DUP` of it. If the file has an
  `/Encrypt` entry, every item that is not a `DUP` is `RAW`.
- The class (`kind.rs`), by the first rule that applies: `/Type /XRef` → cross-reference
  stream; `/Type /ObjStm` → object stream; `/Subtype /Image` → image; `/Type /Metadata` or
  `/EmbeddedFile` → other data; `/Subtype` Type1C, CIDFontType0C or OpenType, or a `/Length1`
  → font; an `/N` without `/BBox` (ICC profiles) → other data; anything else → content.
- **The plan** (`container.rs`, `choose`): each item gets the mode that minimises estimated
  size plus λ times estimated time, with the smallest λ that keeps the file's estimated time
  within `file size / (1.1 × target speed)` (100 MB/s by default). The estimates come from the
  dictionaries and fitted cost tables, never from timing, so the output is the same on any
  machine and with any number of threads.
- An item's bytes may cover less than the stream's data: what a FlateDecode item does not
  cover (anything after its DEFLATE data and Adler-32) stays in the skeleton, before the next
  item.

### 7.2 FlateDecode streams (`TOK`, `FULL`)

- The data starts with a zlib header if its first two bytes have compression method 8, a
  window size of at most 32 KB, no preset dictionary, and a valid check (`(CMF × 256 + FLG)
  mod 31 = 0`). The DEFLATE stream after it is parsed with flate-recons; a stream that does not
  parse is `RAW`. The Adler-32 flag is set if at least 4 bytes follow the DEFLATE data and the
  first 4 are the Adler-32 of the inflated data.
- `FULL`: the record is `flate_recons::deconstruct_with` of the DEFLATE stream, one thread.
- `TOK` uses pixels for images whose geometry (`image.rs`) has no predictor, 8 bits per
  component and at most 4 components: `bpp` is the number of components and `stride` is width
  × `bpp`.
- The token coder counts the symbols of every table at the finest resolution. Per slot, the
  resolution is the one with the smallest estimate of `Σ c × log2(C / c) + 6` bits over each
  table's symbols (`C` its total) plus 16 bits per table, the finer one on ties. A table with no
  symbols is not used. A symbol of count `c` in a table whose largest count is `M` gets the
  level `max(1, 63 + round(4 × log2(c / M)))` (rounding half away from zero). Every run of
  zero levels is one code.
- A stream that the token coder or flate-recons fails on is `RAW`.

### 7.3 Images (`PNG`, `JPEG`)

- `PNG` is for FlateDecode images whose geometry has a PNG predictor (10–15), with 1 component
  of 1, 2, 4, 8 or 16 bits, or 2, 3 or 4 components of 8 or 16 bits (PNG colour types 0, 4, 2
  and 6). The stream's data is wrapped as a PNG file: the signature, an IHDR chunk (width,
  height, bit depth, colour type, then 0, 0, 0), one IDAT chunk holding the data, and an empty
  IEND chunk, each with its CRC-32. The table records the colour type, bit depth, width and
  height of that IHDR. The item is kept only if the png-recons file is smaller than the data.
- `JPEG` is jpeg-recons' container of the data, kept only if smaller than the data.

### 7.4 Streams and fallback

- The table, the skeleton and the pools are zstd at the chosen level (9 by default) if they
  are at least 32 bytes long and zstd makes them smaller; every other stream is stored.
- The token streams are all empty if no item is `TOK`.
- The result is stored (version 1) if coding fails or panics, or if the coded file is not
  smaller than the stored one. With verification on, a coded file that does not decompress to
  the original is coded again without the `TOK`, `FULL` and `PNG` modes.
