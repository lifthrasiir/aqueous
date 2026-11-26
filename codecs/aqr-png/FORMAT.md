# aqr-png format

aqr-png is a module of entry formulas (`aqr-formula`) that rebuilds a PNG file bit for bit
from a smaller representation. The chunks are kept apart from the image data. The image
data's DEFLATE streams are kept as their exact LZ77 tokens and block structure, and the
tokens are entropy coded against a model of the image. This document describes the module
and that representation.

The dynamic Huffman headers are coded with aqr-flate's header coder. They are specified
in aqr-flate's `FORMAT.md` (section *Header stream*), which this document refers to
rather than repeats. Everything else is specified here.

## Conventions

- Integers in byte fields are little endian, unless marked BE (big endian, as PNG stores
  them).
- **varint**: unsigned LEB128 (7 bits per byte, least significant group first, high bit set
  on every byte but the last). At most 10 bytes; bits beyond 64 are dropped.
- **Bit streams** are packed least significant bit first. An `n`-bit field holding `v` is
  written least significant bit first. A bit stream ends with zero bits up to a byte
  boundary. Reading past the end of a bit stream gives zero bits, and a part that needs more
  bits than it has is invalid.
- **gamma(v)** (`0 ≤ v < 2^32`) is Elias gamma of `u = v + 1` in a bit stream. With
  `k = floor(log2 u)`, it is `k` zero bits, a one bit, then the low `k` bits of `u` as a
  `k`-bit field. For example, gamma(5) is the bits 0, 0, 1, 0, 1 in that order, the byte
  `0x14` on its own. Note that aqr-flate's gamma (in the header stream) is a different
  code: ones then a zero.
- **zigzag** of a byte `e`, read as a signed 8-bit value, is `(e << 1) ^ (e >> 7)` taken
  mod 256: 0, −1, 1, −2, …, 127, −128 map to 0, 1, 2, 3, …, 254, 255. **unzigzag** is its
  inverse.
- `x div y` and `x mod y` are integer division and remainder; `ceil(x / y)` rounds up.
- Arithmetic on bytes (sample values, filtered bytes, residuals) is mod 256.

## Skeleton

A PNG file is its 8-byte signature `89 50 4e 47 0d 0a 1a 0a`, then chunks: a u32 BE data
length, a 4-byte chunk type, the data, and a u32 BE CRC-32 (of the type and the data). The
compressor reads chunks up to and including the first `IEND`, or up to the first chunk that
does not fit in the file. Whatever follows is the *trailer*.

The skeleton is every chunk without its image data, followed by the trailer:

| field | |
|---|---|
| varint | number of chunks |
| per chunk | |
| 4 bytes | chunk type |
| varint | data length (below 2^32) |
| 1 byte | flags: the class in bits 0–6; bit 7 set if the CRC is an exception |
| 4 bytes | the CRC (u32 BE), only if bit 7 is set |
| bytes | class 0: the whole data; class 2: its first 4 bytes; class 1: nothing |
| varint | length of the trailer |
| bytes | the trailer |

The skeleton must end exactly there.

Classes:

| class | chunks | data |
|---|---|---|
| 0 (plain) | every other chunk | kept in the skeleton |
| 1 (IDAT) | `IDAT` | all image payload |
| 2 (fdAT) | `fdAT` with at least 4 bytes of data | a sequence number (in the skeleton), then image payload |

A class 2 chunk with less than 4 bytes of data, or any other class, is invalid. A CRC is an
exception if it is not the CRC-32 of the chunk's type and data. When rebuilding, a chunk
without an exception gets the CRC that it should have.

### Images

An *image* is a maximal run of consecutive chunks of the same class 1 or 2. A chunk of
another class ends the run, so an `IDAT` run directly followed by an `fdAT` run is two
images. An image's *payload* is the concatenation of its chunks' data, past the 4-byte
sequence number of each `fdAT`. Its length is known from the skeleton. In the payload are
one or more zlib streams (several in Apple's split IDATs, for example).

Each image has a header, or none:

- The *IHDR* is the data of the first chunk if that chunk is class 0, has type `IHDR` and
  has exactly 13 bytes of data: width (u32 BE), height (u32 BE), bit depth, colour type,
  compression, filter and interlace (one byte each). Otherwise there is no IHDR.
- An `IDAT` image has the IHDR.
- An `fdAT` image has the frame of the last class 0 `fcTL` chunk before it. If that chunk
  has exactly 26 bytes of data and there is an IHDR, the frame is the IHDR with the width
  replaced by bytes 4–7 of the `fcTL` data and the height by bytes 8–11 (u32 BE). Otherwise,
  or if no `fcTL` came before, the image has no header.

### Rebuilding the file

Write the PNG signature, then each chunk: its length (u32 BE), its type, its data, and its
CRC (u32 BE). A class 0 chunk's data comes from the skeleton. The data of a class 1 or 2
chunk is the next bytes of its image's payload (as many as its length, less 4 for class 2),
after the sequence number for class 2. Then write the trailer.

## Image records

| byte | record |
|---|---|
| 0 | the payload, as it is (the record is one byte longer than the payload) |
| 1 | a coded payload (only for an image with a header) |
| 2 | a payload coded against its [reference](#references) (only for an image with one) |

Any other first byte is invalid. A coded record rebuilds a payload of exactly the length
the skeleton gives.

The compressor codes an image whose header gives a [geometry](#geometry) and whose payload
it can parse. Where the image has a reference, it codes it against it too, and keeps the
smaller of the two records. It keeps the payload as it is if coding fails, or if the coded
record would be longer than the payload.

A coded record is:

| field | |
|---|---|
| 1 byte | 1 or 2 |
| varint | length of the layout |
| bytes | the [layout](#layout) (a bit stream) |
| varint | length of the header stream |
| bytes | the [header stream](#header-stream) (a bit stream) |
| varint | length of the tables |
| varint | length of the symbol stream |
| varint | length of the raw bits |
| bytes | the [tables](#tables) (a bit stream) |
| bytes | the [symbol stream](#symbol-stream) (rANS) |
| bytes | the raw bits (a bit stream) |

The record must end exactly after the raw bits.

## References

The module may be given a *reference file*, a PNG file such as a variant of the same
picture (a layer shown or hidden). Its images are found as in [Images](#images), from its
own chunks (as the compressor reads them). Image `i` of the file has a *reference* if the
reference file has an image `i`, both have a header, and the headers have the same width,
height, bit depth, colour type and interlace byte. A record of kind 2 for an image without
a reference is invalid.

The reference's *tokens* are those of its image's payload: the payload's zlib streams,
found as the compressor finds them (see [Layout](#layout): the first at the payload's start,
then a new one wherever the 4 bytes after a stream's end are followed by a valid zlib header
without preset dictionary, at least 3 more bytes remain, and a DEFLATE stream parses from
there), each parsed as DEFLATE. If the first does not parse, the record is invalid. Each
token *starts* at a position of the data (all streams' decompressed data concatenated, as
for the image itself): a literal takes one position, a match its length, and stored bytes
take their positions without being tokens. Whatever follows the streams is not read.

## Geometry

The geometry is how the image's decompressed data lays out as rows. It comes from the
header:

- *channels*: 1 for colour types 0 and 3, 3 for 2, 2 for 4, 4 for 6. Any other colour type
  has no geometry.
- *bits* per pixel: channels × bit depth. *bpp*: `ceil(bits / 8)`. If bits is 0, or the
  width or height is 0, there is no geometry. The bit depth is not otherwise checked.
- *Passes*: if the interlace byte is 1, the seven Adam7 passes, with (x0, y0, dx, dy) of
  (0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2),
  (0, 1, 1, 2). Otherwise one pass, (0, 0, 1, 1). A pass is `pw` = `ceil((width − x0) / dx)`
  pixels wide (0 if `width ≤ x0`) and `ph` rows high likewise. Passes with `pw` or `ph` of
  0 are left out.
- A pass's *row size* is `rs = ceil(pw × bits / 8) + 1`: the filter byte, then the row's
  bytes. Its rows are `ph` rows of `rs` bytes, and the passes follow each other from
  position 0. The *image length* is the sum of `rs × ph`. If it is above 2^40, there is no
  geometry.

The data is the image's decompressed data, all of its zlib streams concatenated. Data
position `p`, below the image length, lies in a row of a pass. If `p` is that row's first
position, it is a *filter byte*. Otherwise it is *pixel byte* `x = p − row start − 1` of the
row (`0 ≤ x < rs − 1`), of *channel* `x mod bpp`. A position at or past the image length is
*past the image*. The data may be shorter or longer than the image.

**Raw values.** The raw (unfiltered) value of a pixel byte is what PNG's filtering gives,
with the row's filter byte as its filter type. Filter types 1 (Sub), 2 (Up), 3 (Average)
and 4 (Paeth) predict from the raw values `a` (pixel byte `x − bpp` of the row), `b`
(pixel byte `x` of the row above, in the same pass) and `c` (pixel byte `x − bpp` of the
row above). Each is 0 where it is outside the pass, that is, for `x < bpp` or in a pass's
first row. Type 0 and types above 4 predict 0. Paeth is PNG's: with `pa = |b − c|`,
`pb = |a − c|`, `pc = |a + b − 2c|`, it is `a` if `pa ≤ pb` and `pa ≤ pc`, else `b` if
`pb ≤ pc`, else `c`. The raw value is the filtered byte plus the filter's prediction. These
neighbours always come before the byte in the data.

## Layout

The layout is a bit stream:

| field | |
|---|---|
| 1 bit | the model kind: 0 *image*, 1 *bytes* |
| gamma | `n`, the data length (all streams) |
| gamma | the number of zlib streams − 1 |
| per stream | |
| 8 bits | CMF, the first byte of the zlib header |
| 8 bits | FLG, its second byte |
| gamma | the number of DEFLATE blocks |
| 1 bit | 1 if only the stream's last block is final, else 0 |
| per block | |
| 1 bit | the block's BFINAL, only if the previous bit is 0 |
| 2 bits | the block type: 0 stored, 1 fixed, 2 dynamic (3 is invalid) |
| stored: 8 bits | the value of the padding bits before LEN |
| stored: 16 bits | LEN |
| fixed or dynamic: 1 bit | 1 if the block has as many tokens as the previous fixed or dynamic block of the stream, else 0; always present (for the stream's first such block, 1 means 0 tokens) |
| fixed or dynamic: gamma | its token count, only if the previous bit is 0 |
| 8 bits | the value of the padding bits after the final block |
| gamma | the number of trailing bytes |
| 8 bits each | the trailing bytes |
| after the streams: gamma | the number of matches sent as 284 + 31 |
| gamma each | their token indices, each as the difference to the previous one (to 0 for the first) |

Everything from the CMF byte to the last trailing byte is repeated for each stream; the
gamma codes after the streams come once.

The compressor chooses the *image* kind when the bit depth is 8 and the colour type is 0,
2, 4 or 6, and *bytes* for everything else. An *image* kind needs `bpp` of 1 to 4.

A stream's trailing bytes are whatever follows its DEFLATE data in the payload before the
next stream: its Adler-32, and for the last stream anything after it too. The compressor
gives every stream but the last exactly 4 trailing bytes; the format allows any number. (The compressor
starts a new stream where the 4 bytes after a stream's end are followed by a valid zlib
header without preset dictionary, at least 3 more bytes remain, and a DEFLATE stream parses
from there.)

Padding values are the bits read (least significant first) between the end of the block
header (or of the final block) and the next byte boundary. A padding value must fit in that
many bits, or the stream will not be rebuilt as it was.

*Tokens* (literals and matches) are numbered over the whole image, all streams and blocks
in order, from 0. Stored bytes are not tokens and are not counted. Matches of length 258 are normally sent as length symbol 285. The listed
indices are matches sent as symbol 284 with extra bits 31 instead. Each must be a match of
length 258 and appear only once, so a difference can be 0 only for the first index. The
compressor lists them in increasing order.

The *spans* of the image are its blocks in order over all streams: a stored block is a span
of LEN data bytes, and a fixed or dynamic block a span of its token count tokens. The tokens
and stored bytes of the spans make up the `n` bytes of data, in order.

## Entropy coding

The data is coded as a sequence of *symbols*, each from one of 117 *tables* (121 in a record
of kind 2), with rANS, and of *raw bits*, written to the raw bit stream in order.

### Tables

| index | tables | alphabet |
|---|---|---|
| 0–95 | literals: `48 × after + 12 × group + bucket` | 256 |
| 96 | filter bytes, and data past the image | 256 |
| 97 | escape | 2 |
| 98–110 | length codes: `98 + bucket` (bucket 0–12) | 29 |
| 111–116 | distance codes: `111 + length bucket` (0–5) | 30 |
| 117–120 | kind 2 only, the same token as the reference's: `117 + 2 × same + match` | 2 |

The tables part is a bit stream: first one bit per table (0 to 116, or to 120 in a record of
kind 2), 1 if the table is used. Then, for every used table in order, its *levels*, one per symbol of its alphabet.
Each level is 0 (the symbol is absent) or 1 to 63. They are coded with `prev = 0` at the
start of each table, as a series of gamma codes `g`:

- `g = 0`: gamma(`r`) follows. The next `r + 1` symbols have level 0. The run must not pass
  the alphabet's end.
- `g > 0`: with `z = g − 1`, the level is `prev + (z + 1) / 2` for odd `z` and
  `prev − z / 2` for even `z`. It must be in 1 to 63. It becomes `prev`.

Level 0 runs leave `prev` as it is. (The compressor writes every run of zero levels as one
code.)

A table's frequencies (summing to 4096) come from its levels. Each level `l ≥ 1` has the
weight

    W(l) = (F[(l − 1) mod 4] << ((l − 1) div 4)) >> 8,   F = [65536, 77936, 92682, 110218]

With `S` the sum of the weights of the symbols present, each present symbol has frequency
`max(1, floor(W(l) × 4096 / S))` (64-bit arithmetic), and the others 0. If the total `T` is
not 4096, the frequency `f[m]` of the most frequent symbol (the lowest index of equals) is
fixed, again and again until `T` is 4096; `m` is chosen afresh each time. If `T < 4096`, `4096 − T` is added to it. If
`T > 4096`, it loses `d = min(T − 4096, f[m] − 1)`, and if `d` is 0 the table is invalid. If
no symbol is present, symbol 0 has frequency 4096. Symbol `s` takes the slots `cum[s]` to
`cum[s] + f[s] − 1`, where `cum[s]` is the sum of the frequencies before it.

The compressor counts every table's symbols, and gives a symbol of count `c` (largest count
`M`) the level `max(1, 63 + round(4 × log2(c / M)))`. A table without symbols is not used,
and coding a symbol from a table that is not used is invalid.

### Symbol stream

The symbols are coded with four interleaved 32-bit rANS states, 12-bit probabilities and
16-bit renormalisation, with `L = 2^15`. Words are u16, little endian.

**Decoding.** The stream starts with the four states `x0`, `x1`, `x2`, `x3`, each as two
words: the high 16 bits, then the low 16 bits. Symbol `i` (from 0) is decoded with state
`x[i mod 4]`, from its table `t`:

1. `slot = x mod 4096`, giving the symbol `s` with `cum[s] ≤ slot < cum[s] + f[s]`.
2. `x = f[s] × (x >> 12) + slot − cum[s]`.
3. If `x < 2^15`, `x = (x << 16) | the next word`.

Reading past the end gives zero words. After the last symbol, the stream must have been read
exactly to its end, and all four states must be `2^15`.

**Encoding** runs backwards from the last symbol, with every state starting at `2^15`. To
encode `s` in state `x`: if `x ≥ f[s] << 19`, emit the word `x mod 2^16` and set
`x = x >> 16`; then `x = ((x div f[s]) << 12) + (x mod f[s]) + cum[s]`. The stream is the
final states (`x0` first) followed by the emitted words in the reverse of their emission
order. States stay below 2^31.

### Raw bits

The raw bits are a bit stream of the DEFLATE extra bits of the matches, in coding order.
Reading past its end is invalid.

## The model

The data is coded in order, position `p` from 0 to `n − 1`, span by span. Each position is
coded with a table and, for a pixel byte of the *image* kind, against a prediction. `data[i]`
is the filtered (decompressed) byte at position `i`, 0 for `i < 0`. Raw values are those of
[Geometry](#geometry), 0 outside the pass.

**After.** `after` is 1 if the last token was a match, 0 if it was a literal or there has
been none. It carries over between blocks and streams. Stored bytes leave it as it is.

**Activity.** `bucket(v)` is the bit length of `v`, capped at 11: 0 for 0, 1 for 1, 2 for
2–3, 3 for 4–7, …, 11 for 1024 and up. `mag(d)` is the magnitude of the byte `d` read as
signed 8 bits (0 to 128). At pixel byte `x` of a row of size `rs` at position `p`:

- `fl = mag(data[p − bpp])`, `fu = mag(data[p − rs])` (whatever positions those are);
- *image* kind: `a`, `b`, `c` are the raw values left, above and above left (as for the PNG
  filters), and `act = |a − c| + |b − c|`.

**The coding of a position** gives its table `T`, its prediction `pred`, the filter's
prediction `fp` and the length bucket `lb`:

| position | `T` | `pred` | `fp` | `lb` |
|---|---|---|---|---|
| filter byte, or past the image | 96 | 0 | 0 | 12 |
| pixel byte, *bytes* kind | `12 × ((x mod bpp) & 3) + bucket(fl + fu)` | 0 | 0 | `bucket(fl + fu)` |
| pixel byte, *image* kind | `12 × min(ch, 3) + bucket(act + fl + fu)` | below | the row filter's prediction of the byte | `bucket(act)` |

For the *image* kind, `ch = x mod bpp` and `pred = med(a, b, c)`, where `med` is the median
of `a`, `b` and `a + b − c` (LOCO-I's predictor). If `bpp ≥ 3` and `ch` is 1 or 2, the
previous channel's residual is added:
`pred = clamp(med(a, b, c) + v1 − med(a1, b1, c1), 0, 255)`, where `v1` is the raw value of
pixel byte `x − 1` and `a1`, `b1`, `c1` are its neighbours as above.

This is the pixel model of aqr-raw (its `FORMAT.md`, §7.2, which codes raw rasters with it,
taking the magnitudes of its own residuals where `fl` and `fu` are here).

A position's *literal symbol* is `zigzag(v − pred)`, where `v = data[p] + fp` (the raw value
for an *image* pixel byte, else the filtered byte itself). When decoding, `v = pred +
unzigzag(symbol)` and `data[p] = v − fp`.

**A stored byte** is its literal symbol in table `T`, without the `after` offset, and
without an escape symbol even if the literal symbol is 255.

**Against a reference** (a record of kind 2), a token at a position where a reference token
starts is first a symbol from table `117 + 2 × same + match`, where `match` is 1 if the
reference token is a match, and `same` is the last such symbol of the image (0 for the
first). Symbol 1 means the token is the reference token: a literal of the same byte, or a
match of the same length and distance (whether it is sent as 284 + 31 is the layout's). It
updates the data and `after` as that token would, and nothing else of it is coded. Symbol 0
means it is not, and the token is coded as below. Elsewhere, and in a record of kind 1, a
token is coded as below only.

**A token** is coded at its position from table `T + 48 × after` if `T < 96`, else `T`:

- A literal: its literal symbol. If that symbol is 255, a symbol 0 of the escape table
  follows.
- A match: the symbol 255, then a symbol 1 of the escape table. Then:
  1. the length code `lc` (0–28, DEFLATE's length symbol − 257) from table `98 + lb`, then
     its extra bits as a raw field: `len = LEN_BASE[lc] + extra`;
  2. the distance code `dc` (0–29) from table `111 + LB(len)`, then its extra bits as a raw
     field: `dist = DIST_BASE[dc] + extra`.

  `LEN_BASE`, `DIST_BASE` and their extra bit counts are DEFLATE's (RFC 1951 §3.2.5). The
  compressor sends length 258 as code 28, and marks a 258 sent as 284 + 31 in the layout.
  `LB(len)` is 0 for 3, 1 for 4, 2 for 5, 3 for 6–8, 4 for 9–32 and 5 for 33–258. A match
  must have `dist ≤ p` and `len ≤ n − p`. Its bytes are copied from `dist` back, one by
  one, so they may overlap. The copied bytes have raw values like any other bytes (the
  unfiltering of the data), which later predictions read.

A token at a position past the end of the data, or a stored span or match going past it, is
invalid. When the spans are done, exactly `n` bytes must have been coded.

## Header stream

The header stream holds the Huffman header of every dynamic block of the image, in order
over all streams, coded with aqr-flate's header coder (aqr-flate's `FORMAT.md`,
*Header stream*, with its bit conventions). The coder's recipe starts fresh for each image
and carries over from stream to stream. The stream is padded with zero bits to a byte
boundary.

Each header is read against its block's symbol frequencies, which come from the block's
tokens once they are decoded: 288 literal/length counts (each literal byte, the
end-of-block symbol 256 counted once, each match's length symbol, with symbol 284 for the
matches the layout lists and 285 for other matches of length 258) and 32 distance counts.

## Rebuilding the payload

1. Read the layout. Decode the tokens and stored bytes of the spans: this gives the data,
   the tokens, and each block's symbol frequencies.
2. Mark the listed matches as sent as 284 + 31.
3. Read the dynamic blocks' headers from the header stream.
4. For each stream in order, write its CMF and FLG bytes and then its DEFLATE data. Each
   block is BFINAL (1 bit), BTYPE (2 bits), and:
   - stored: the padding value in the bits up to the byte boundary, LEN (16 bits), its
     one's complement NLEN (16 bits), then the stream's next LEN data bytes;
   - fixed: the tokens with DEFLATE's fixed codes, then the end of block (256);
   - dynamic: the header (HLIT − 257 in 5 bits, HDIST − 1 in 5 bits, HCLEN − 4 in 4 bits,
     the code-length code lengths in 3 bits each in DEFLATE's permuted order, then the
     run-length coded lengths), the tokens with its codes, then the end of block.

   Then come the final padding value up to the byte boundary, and the trailing bytes. Every
   token's symbol needs a code. Each stream covers the data of its own tokens and stored
   blocks, and the streams follow each other in the data.

## Formula module

The module is `png-recons`, version 5, in `formula::encode` and `formula::decode`; and
`png-against`, version 1, in `formula::encode_against` and `formula::decode_against`, for a file
coded against a reference file. Its output is the PNG file, whose length the application gives.
The file is rebuilt as in [Rebuilding the file](#rebuilding-the-file), from the skeleton and the
image records.

**Parameters**: none (empty). A module application with parameters is invalid.

**Inputs**: five for `png-recons` (version 5), four for `png-against`, in this order. Any other count is
invalid.

| slot | class | content |
|---|---|---|
| 0 | `png/skeleton` | the [skeleton](#skeleton) |
| 1 | `png/index` | for each image in order, the length of its record: a varint |
| 2 | `png/records` | the [image records](#image-records), concatenated in order, without length prefixes |
| 3 | `png/reference`, or any node | the [reference file](#references), for `png-against` |

`formula::encode_against` puts the reference file in a stream of its own, for the caller to
replace with whatever node gives it (the output of the reference's own application, say).
It gives an application of `png-against` only if a record is of kind 2, and of `png-recons`
otherwise.

`png-recons` takes the records of its images as three inputs, in place of the one that
`png-against` takes (version 5 of `png-recons`, a part module whose inputs are each read once, in
order): a coded record is the *head* of its record, then the model payload's rANS stream, then its
raw bits, and the three are kept apart.

| slot | class | content |
|---|---|---|
| 0 | `png/skeleton` | the [skeleton](#skeleton) |
| 1 | `png/index` | for each image in order, the length of its head: a varint |
| 2 | `png/heads` | the heads, concatenated in order: a record of kind 0 whole (the byte 0 and the payload), of kind 1 up to the end of the model payload's tables (the byte 1, the layout, the headers, the three lengths and the tables) |
| 3 | `png/rans` | the rANS streams of the model payloads of the coded images, concatenated in order |
| 4 | `png/raw` | their raw bits, concatenated in order |

Each coded image takes as many bytes of slots 3 and 4 as its lengths (in the head) say, and the
slots must be used up exactly. The module is a part module (windows of four pages): it
puts the file out in order, an image at a time (where it has several images of a payload of 8 MiB at most each, a batch of up to 8 of them and 8 MiB of records at a time, decoded as tasks), and holds a block of tokens, the image's model
tables, layout and rows, whatever the lengths of the file. A record of kind 2 is not taken.

The images are those that the skeleton gives. The index has one varint per image and ends
exactly there; the records fill slot 2 exactly. Each record is the byte 0 and the payload,
or the byte 1 and a coded record, and a coded record rebuilds a payload of exactly the
length the skeleton gives. An image that is kept as it is stays kept (record 0), so the
payloads are the file's wherever coding does not make an image smaller.

The rebuilt file must have the length that the application gives. There is no checksum to
test: the formula's own evaluation is trusted to hand the module the inputs it was made
with.

`formula::encode` gives no formula (`None`) for a file that is not a PNG file, or that
cannot be parsed, or in which no image has a coded record (the formula would be the file and
the overhead of the slots).

## Sets of files

The module `png-set`, version 2, in `formula::encode_set` and `formula::decode_set`, rebuilds
several PNG files at once, each coded against an earlier one (variants of a picture, say). Its
output is the files one after another, whose length the application gives; a Split instruction
separates them by the sizes the formula declares, which are those of the parameters.

**Parameters**:

| field | |
|---|---|
| varint | `n`, the number of files |
| `n` varints | for each file `i`, 0 if it has no parent, else its parent's index plus 1, which must be at most `i` |
| `n` varints | for each file `i`, its size; the sizes must add up to the length of the output |

The parameters must end exactly there.

**Inputs**: exactly three, each a packed byte sequence (aqr-formula's `packed`) of `n` items,
the items of file `i` being:

| slot | content |
|---|---|
| 0 | the [skeleton](#skeleton) of file `i` |
| 1 | the index of file `i`, as slot 1 of `png-recons` |
| 2 | the records of file `i`, as slot 2 of `png-recons` |

File `i` is rebuilt as `png-recons` rebuilds a file from its slots 0 to 2, or as
`png-against` with its parent's file (file `p`, rebuilt before it) as the
[reference file](#references) if it has a parent `p`. It must have the size the parameters
give.

`formula::encode_set` gives an application of `png-set` split into the files, with the files'
slots as streams of the classes of `png-recons`, and a root of the files one after another, for
the caller to put each where its file is. It codes every record against the parent's image where that makes
it smaller (as `formula::encode_against`), and gives no formula if a file does not have one of
its own.

## Design notes *(informative)*

### Model and representation

Literals are the residual of the raw (unfiltered) byte against aqr-raw's pixel model
([`aqr_raw::image::pixel`](../aqr-raw/src/image/pixel.rs)); the row's PNG filter is reapplied to
give the byte the stream holds, so the filter choice does not matter. Matches start as an
escape symbol in the literal alphabet, so a literal costs a single symbol. Entropy coding is
static per-image tables (two passes when compressing), four interleaved 32-bit rANS states
(`src/rans.rs`, on `aqr_common::rans32`); the encoder divides by multiplying with packed
reciprocals, the decoder looks symbols up in a full table per context and renormalises without
a branch. Huffman headers go through aqr-flate's header coder, against the frequencies of the
block's tokens (zlib's own trees cost a bit). An image's payload may hold several zlib streams;
Apple's iDOT files use one stream with a full flush, which needs nothing special.

### Why the LZ77 parse is kept

The obvious design inflates the image data, codes the pixels with a strong image model, and
re-creates the DEFLATE stream on the way back by reproducing the encoder's choices
(aqr-flate's job). Measured on these sets, it cannot be fast:

- Most PNG files here are exact zlib output (libpng's level 6, or level 9). aqr-flate's
  zlib emulation reproduces them for about 10 bytes, but at zlib's own speed: 1-20 MB/s of
  PNG file. Its fast, rule-based representation is 5-25 MB/s of PNG here, and its
  corrections cost 3-30% of the PNG (photo streams have long literal codes and short,
  unpredictable matches, and oxipng/zopfli-style streams follow no simple rule).
- PNG data expands 2-30× when inflated, so even 100 MB/s of decompressed data is only
  3-50 MB/s of PNG.

So aqr-png keeps the stream's own tokens (literals and matches) and codes *them* against
the image: decompression decodes tokens and Huffman-codes them again, with no match search.
The price is ratio on photos, where an LZ77 parse spends many bits on short matches that an
image codec would not need (`src/bin/probe.rs`: 1.1 literals and 0.1 matches per PNG byte
on the photo sets).

### Against a reference

A PNG file can be coded against a similar one (`formula::encode_against`, the module's
fourth input): sets of variants of a picture, with layers shown or hidden ("差分"), are
mostly the same pixels, and the same writer mostly makes the same parse of them. Each token
where the reference's image has one starting at the same place of the data is first a
decision whether it is that token (by the last decision and whether the reference's is a
match); the tokens that are are coded by nothing else. The reference's tokens come from
parsing its payload again, so the decoder needs its file, not its record.

On seven ZIP archives of such sets (Clip Studio Paint, 684 MB of PNG files in groups of 13
to 88 of the same size), each file coded against a parent along a maximum spanning tree of
how alike their data are (`aqr-png-refprobe --code`): 85% of the tokens are the parent's,
and the records come to 109.5 MB against 491.5 MB alone (4.5×). Both encoders code against
a reference (by rows, and token by token for streams expanding more than 4×, as these do),
the same symbols; each file is coded alone too to keep the smaller record: coding takes 36 s
and decoding 18 s of one thread over the 684 MB.

A set of such files is one application of `png-set` (`formula::encode_set`), of an output
per file, each against an earlier one, its parent. The encoder takes the files' formulas
alone where it is given them (those without a parent as they are), and parses each parent's
images once for all its children. The decoder rebuilds the files of each depth of the trees
at once, their parents being given to them as their reference files.

Tried and dropped: predicting the literals that are not the reference's by the reference's
pixel where their three neighbours are the reference's (tables of their own): 2.6% of the
tokens, records 108.5 MB only, decoding twice as long.

### Speed

Measured with `aqr-png bench`, `samply`, and retired instruction counts
(`/usr/bin/time -l`). The decoder runs at about 6 instructions per cycle, so what helped
was doing less:

- **Decoding**: the rANS states live in a local copy of the decoder (in registers, not
  behind a pointer); literals are decoded in runs within a row, in a loop specialised by
  the row's filter type, without bounds checks on the offsets the buffer layout keeps in
  range (the data has a row of padding in front, the raw image a zero row and zero left
  margin per pass). Symbol frequencies for the Huffman headers are counted while decoding.
- **Encoding**: the data is parsed with aqr-flate; as its rows are complete, a window of
  them at a time, they are unfiltered and every pixel byte's context and residual computed
  (NEON, 16 bytes at a time), then the window's tokens walked. Streams that expand more
  than 4× (mostly long matches) are modelled as the parser hands over the tokens instead,
  skipping the bytes inside matches.
- **aqr-flate** got two-level Huffman decoding tables (photo streams' 15-bit literal
  codes made its single-level tables 128 KB), pattern copies for short match distances, a
  branch-free token writer and a header reader that takes counted frequencies. Its match
  copy (16-byte chunks, no library call) is aqr-png's too: a `copy_within` per match cost
  the parse 15%.
- Tried and dropped: two rANS states (latency-bound), padding the rANS stream to drop a
  bounds check (the clamp lengthened the dependency chain), passing literals to the model
  in batches (it serialised the parser's and the model's dependency chains, which ran
  side by side before), tracking the row position incrementally, smaller (11-bit) tables
  (not cache-bound; 0.2-0.6% larger output).

Where the time goes (kadid10k): decompression is ~75% token decoding (about 210
instructions per token), ~15% Huffman writing, ~6% Huffman headers, ~3% tables;
compression is ~39% parsing, ~16% unfiltering and the row model, ~19% the token walk,
~20% rANS tables and encoding, ~6% block layout and Huffman headers. Photos average
1.1 literals per PNG byte, so 100 MB/s means about 9 ns per literal end to end. gb82-sc
expands 30× when inflated; copying and unfiltering 93 MB of image data for 3 MB of PNG is
already over budget.

### Memory

Neither side holds the inflated data, the raw image or the stream's tokens whole:

- The data and the raw rows are windows: what the parser and the model may still read (the
  farthest match, 32 KiB, and the row above) is kept, and moved down every MiB or so. The
  image encoder computes the symbols of a window of rows (256 KiB per thread) as soon as its
  rows are parsed and walks its tokens; of the parse it keeps only the matches and stored
  blocks not walked yet. The decoder, and the encoder of streams expanding more than 4×,
  move their buffers on the same way (with whole buffers up to 4 MiB of data).
- The decoder hands each block's tokens to the DEFLATE writer as the block ends: one
  block's tokens at a time.
- What still follows the image: the encoder's symbols, 2 bytes each (about one per byte of
  data on photos), since the static tables are counted over all of them before the rANS
  stream is coded backwards. They are freed before the record is copied out.

`png-recons` is a part module (version 5) laying its memory out as a stack (`aqr_api::stack`), so its
limit is its plan (`plan::part`, `src/part.rs`): the skeleton and the index read whole, the file's
chunks taken in pieces of 16 KiB, and the most an image takes (its record's head, layout and model
tables reserved exactly, the decoder's windows, a block's tokens, room for a block written); each
image's payload is written block by block and cut into its chunks as it goes. A file of several
images, each of a payload of 8 MiB or less, is decoded in batches of up to 8 images as tasks
(8 slots); any other, an image at a time. Natively it still decodes whole, on threads
(`plan::recons` is the whole module's plan). `png-against` and
`png-set` are planned the same way (`plan::against`, `plan::set`): a reference file's images are
parsed with code tables of room for the largest code (`aqr_flate::inflate::tokens_of`), their
tokens counted first and their references reserved exactly; `png-set` reserves its output first
and copies each file to its place, and in WebAssembly makes a parent's references of its file
there, where each child is rebuilt, rather than keeping them (which would leave holes on the
stack: about 7% more time on sets than keeping them, which native decoding still does).

Peak heap of `formula::encode` and of the evaluation (`tests/memory.rs` checks it, on a
9.4 MB image), fitted over the 364 files of the sets above that are coded: 4.2 MB + 3.7×
the file for the first, 2.9 MB + 1.1× for the second (the constants are mostly the
windows).

### Threads

The `threads` of `formula::encode` and `formula::decode`: the output is the same whatever the
number of threads, and one thread runs the code above unchanged. One image is coded by one
rANS stream whose contexts come from the decoded pixels, so decoding an image stays serial;
threads overlap what is independent around it:

- An APNG (or any file with several image runs) has its images coded, and rebuilt, in
  parallel: each image makes a record of its own, concatenated in order.
  At most `N` images are in flight.
- A large image (1 MB of data or more) has the symbols of each window computed by stripes
  of rows on several threads, after the window's serial unfilter pass, and the symbol
  histograms counted by chunks and summed.
- Decompressing writes the DEFLATE streams on a thread of their own, block by block as the
  model decodes them (a few blocks in flight): `aqr-png bench -t 4` decompresses the sets
  above at 104 MB/s, against 88 MB/s on one thread.

Files under 64 KB are handled on the calling thread. A panic in a worker is a panic of the
call, so `formula::encode` gives no formula. Not done: parsing the zlib streams of a
multi-stream image in parallel (a stream's end is known once it is parsed).
