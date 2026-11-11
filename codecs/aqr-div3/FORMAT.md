# aqr-div3 format

aqr-div3 turns an AVI file with a DivX 3 video stream into four streams: the video frames **coded**
by a context model (§4–§5), the **skeleton** (every byte of the file that is no frame and no audio
chunk), the **layout** (where the frames and audio chunks are) and the **audio** (the audio
chunks' bytes, as they are or by [aqr-ac3](../aqr-ac3/FORMAT.md)). The module `div3-recons`
(§7) puts the file together again bit for bit.

The coding of elements (the range coder, probabilities and hashing) is that of
[aqr-walk](../../crates/aqr-walk/README.md), *Coding*, and is not repeated here. This document is
normative for the rest, except for the tables of codes, which are `src/tables.rs` (§3.1): a list of
over a thousand codes cannot be in prose. Sections marked *informative* describe the encoder's
choices; a decoder needs none of them.

External reference: none. DivX 3 / MS-MPEG4 v3 has no published specification. The syntax here is
what aqr-div3 reads and writes, found by probing an ffmpeg decoder and encoder as black boxes.

Contents:

1. [Conventions](#1-conventions)
2. [The AVI file](#2-the-avi-file)
3. [Frames](#3-frames)
4. [Coding a frame](#4-coding-a-frame)
5. [The coded frames](#5-the-coded-frames)
6. [Layout, skeleton and audio](#6-layout-skeleton-and-audio)
7. [Formula module](#7-formula-module)
8. [The encoder](#8-the-encoder-informative) *(informative)*

## 1. Conventions

- Integers are unsigned. A **varint** is unsigned LEB128, least significant group first.
- Bits of a frame are read most significant bit first.
- `FRAME_MAX` is 4 MiB: no frame is longer.

## 2. The AVI file

The file is `RIFF` `AVI ` chunks (little-endian 32-bit sizes, chunks padded to even length). The
encoder looks at

- the header list `hdrl`: its `strl` lists in order, each with `strh` and `strf`. The **video
  stream** is the first one whose `strh` type is `vids` and whose handler (fourcc, ASCII
  case-insensitive) is `div3`, `mp43`, `div4`, `div5` or `div6`; its width and height are from
  `strf` (the bitmap header: width at offset 4, height at offset 8, as signed 32-bit; width in
  1..=4096, height non-zero, at most 4096 in magnitude). The **audio stream** is the first
  with type `auds`; it is **AC-3** if its `strf`'s format tag is `0x2000`.
- the data list `movi` (found by its fourcc), walked chunk by chunk from there; `LIST` chunks are
  entered. A chunk `NNdc` whose `NN` is the video stream's index (two decimal digits) is a
  **frame**; `NNwb` of the audio stream is an **audio chunk**; the walk ends at the first chunk
  that is none of `JUNK`, `NNdc`, `NNwb`, `NNdb`, or whose data runs past the file.

The frames and the audio chunks, each as the byte range of its data (not its 8-byte header), in
file order, are the **items**. The skeleton is the file with the items cut out.

## 3. Frames

A frame is an I or a P frame of MS-MPEG4 v3. The frame is described by a walk that is the same in
both directions (aqr-walk's `Io`); §3.2–§3.4 give it in the notation of `Io` elements:
`raw(n)`, `flag`, `vlc(T)`, `field`, as in aqr-walk. A syntax error anywhere means the frame is
**not parsed**.

### 3.1 Tables

All the tables are in `src/tables.rs`:

- `DC_LUMA0`, `DC_LUMA1`, `DC_CHROMA0`, `DC_CHROMA1`: DC levels 0..118 (the symbol is the level)
  and an escape.
- `CBP_I`: the 64 codes of the I frame's macroblock pattern.
- `AC_INTRA0..2_VLC` / `_INFO`: the AC tables of intra luma blocks for `rl` 0, 1, 2; and
  `AC_B0..2_VLC` / `_INFO`: those of inter blocks and of chroma blocks (for `rlc` or `rl` 0, 1,
  2). `_INFO[symbol]` is `(run, level, last)`; the escape symbol is `0xFFFF`.
- `MB_P`: the macroblock types of P frames: 63 inter codes, one inter with no coded block, and
  intra ones. A symbol is `intra << 6 | cbp`, where `cbp` has bit `5 − i` set for coded block
  `i` (blocks 0–3 luma, 4 and 5 chroma).
- `MV0_VLC`, `MV1_VLC` (motion vector tables 0 and 1: 1097 and 1099 codes and an escape) with
  `MV0_VEC`, `MV1_VEC` giving the vector `(x, y)` of each symbol.

A VLC is a list of `(code left-aligned to 32 bits, length, symbol)`. Codes form a prefix tree; a
code with no leaf is a syntax error.

### 3.2 Frame

```
kind  = raw(2)               0: I, 1: P, others not parsed
I frame or P frame (below)
tail: while bits are left: flag(TAIL | min(j, 63))   for the j-th left bit
```

The tail is the extension header and padding. At most 40 bits (I) or 16 bits (P) may be left,
else the frame is not parsed.

Macroblocks are 16×16 pixels: `mbw = ⌈width/16⌉` by `mbh = ⌈height/16⌉`, in raster order. A
macroblock has six blocks: 0–3 luma (2×2), 4 and 5 chroma.

`d012` is `raw(1)` then, if it was 1, `1 + raw(1)`: a number 0, 1 or 2.

### 3.3 I frame

```
q = raw(5);  slice = raw(5);  rlc = d012;  rl = d012;  dc = raw(1)
for each macroblock:
    sym = vlc(CBP_I, ctx CBPI)
    block flags: for block i in 0..6, bit_i = sym >> (5 − i) & 1;
        for i < 4 the flag is bit_i xor predicted, with the flags of the neighbouring luma
        blocks A (left), B (up-left), C (up) taken as 0 outside the picture:
            predicted = (B == C) ? A : C,   and the flag stored for the block is the xored one
    flag(AP)                                          ac_pred
    for each block i in 0..6:
        DC(i)
        if flag[i]: AC(i < 4 ? INTRA[rl] : B[rlc], kind = i < 4 ? 2 : 3, maxi = 63)
```

The flag of a block ≥ 4 is `bit_i` itself.

**DC(i)**: with the table `DC_LUMA{dc}` for `i < 4` or `DC_CHROMA{dc}` otherwise,
`s = vlc(table, DC | (i < 4))`; `level = s`, or `raw(8)` if `s` is the escape; if `level ≠ 0`,
`raw(1)` (the sign).

### 3.4 P frame

```
q = raw(5);  use_skip = raw(1);  rl = d012;  dc = raw(1);  mvt = raw(1)
for each macroblock at (mx, my):
    lk, uk = kind of the left and the up macroblock (0 skipped, 1 inter, 2 intra; 0 outside)
    nctx = lk | uk << 2
    if use_skip and flag(SKIP | nctx) = 1: macroblock skipped (kind 0), next
    sym = vlc(MB_P, MBT | nctx)
    if sym & 64: intra macroblock (kind 2):
        flag(AP)
        for i in 0..6: DC(i); if cbp bit i: AC(i < 4 ? INTRA[rl] : B[rl], kind = i < 4 ? 2 : 3, maxi = 63)
    else: inter macroblock (kind 1):
        lz, uz = the left and the up macroblock's vector state (0: not inter, 1: zero vector,
                 2: other; 0 outside), s = vlc(MV[mvt], MV | lz | uz << 2)
        if s is the escape: raw(12), the vector state of this macroblock is 2;
        else its state is 1 if MVn_VEC[s] = (0, 0), else 2
        for i in 0..6: if cbp bit i: AC(B[rl], kind = i >= 4 ? 1 : 0, maxi = 64)
```

The inter blocks use the chroma AC tables `B` for luma too, and their scan index starts at 0
(the DC position is a coefficient). The tables of quantizers 3–5 are the same; quantizer 2 was
not told apart from them.

### 3.5 AC(T, kind, maxi)

Symbols of a block until `last`:

```
i = 0; prev = 0
loop:
    ctx = AC | kind | min(i, 15) << 2 | min(prev, 2) << 6
    s = vlc(T, ctx)
    if s is the escape:
        if flag(ESC | kind) = 1:           esc1
            s2 = vlc(T, ctx | 1 << 8)  (not the escape)
            (run, level, last) = T.info[s2];  level += maxl[T][last][run]
        else if flag(ESC | 4 | kind) = 1:  esc2
            s2 = vlc(T, ctx | 2 << 8)  (not the escape)
            (run, level, last) = T.info[s2];  run += maxr[T][last][level]    (a missing maxr: not parsed)
        else:                              esc3
            last = raw(1); run = raw(6); v = raw(8); level = v ≥ 128 ? 256 − v : v
            (no sign bit follows)
        if not esc3: raw(1)               sign
    else:
        (run, level, last) = T.info[s];  raw(1)     sign
    i += run + 1;  if i > maxi: not parsed
    prev = level;  if last: end of the block
```

`maxl[T][last][run]` is the highest level of `T.info` with that `last` and `run`;
`maxr[T][last][level]` the highest run with that `last` and `level` (level < 128), or none. In esc3 the 8-bit level `v`
is read as signed (128..255 mean negative) but its magnitude only is used; the sign is in `v`, so
no sign bit follows. After esc3 the block ends if `last` is 1, with `i` and `prev` updated as for
the others.

### 3.6 Contexts

The element ids (the top byte of `ctx` in the walk; `kind` and the other fields are or-ed into
the low bits): `SKIP = 1<<24`, `MBT = 2<<24`, `MV = 3<<24`, `AC = 4<<24`, `DC = 5<<24`,
`AP = 6<<24`, `CBPI = 7<<24`, `TAIL = 9<<24`, `ESC = 10<<24`, `OK = 11<<24`. A bare id is the whole
context where no field is given (`flag(AP)`).

## 4. Coding a frame

Each frame is coded as follows into the chunk's range coder:

1. A bit under the context `OK` (`slot(OK, 0)`): 1 if the frame is parsed (§3), else 0.
2. If 1: the walk of §3 with the elements coded as aqr-walk's *Coding* says. The frame's length
   is known from the layout, and so are the bits `total` that the walk may use: the tail count is
   `total − done`.
3. If 0: the frame's bytes, each bit flat, most significant first.

A frame is checked first (`Check`): the walk must end within its limits. When decoding, the walk
produces the bits; they must come to exactly 8 × the frame's length, else the stream is corrupt.

The model has `2^22` probabilities (16 MiB). The walk keeps nothing between frames: the model alone
carries what is learned.

## 5. The coded frames

The stream `div3/frames`:

```
varint  n                     the number of chunks (at most 2^24)
n ×  { varint frames, varint bytes }
the n chunks, one after the other, of `bytes` bytes each
```

A chunk codes `frames` consecutive frames (in file order) with a new model (all probabilities at
their initial value) and a new range coder, by §4. The sum of `frames` is the number of frames
in the layout. The encoder starts a new chunk at the first frame after the bytes of its frames
have reached 16 MiB (§8).

## 6. Layout, skeleton and audio

- `div3/layout`: for each item in file order two varints: `gap`, the bytes of the skeleton before
  the item, and `tag = length << 1 | kind` (`kind` 0 a frame, 1 an audio chunk). The bytes of the
  file after the last item are the rest of the skeleton.
- `div3/skeleton`: the bytes of the file that are in no item.
- audio: the audio chunks' bytes one after the other; as the class `div3/audio`, or, for AC-3
  audio that comes to less than 97% by aqr-ac3, the output of the module `ac3-recons` on the class
  `ac3/frames` with the length of the audio.

The file is rebuilt by taking, for each layout entry, `gap` bytes from the skeleton, then the item:
a frame decoded (§4) in order, or `length` bytes of the audio; then the rest of the skeleton.
The total must be the file's length.

## 7. Formula module

`div3-recons` version 2: params the width and the height as two little-endian 16-bit numbers;
inputs, in order, the coded frames (§5), the skeleton, the layout and the audio (§6); the
output length the file's. It is a **part module**: the inputs are read and the output written in
windows of 4 pages, and the working memory is constant: the model (16 MiB), the frame buffer
(`FRAME_MAX` + 8), and four buffers of 16 KiB; plus the stack. `aqr_div3::module::plan` is the
plan the formula declares (`div3_plan_holds` tests it to the page).

In Wasm the chunks are decoded one after another, the model reset in place at each start. Natively
(`formula::decode`) the chunks' frames are decoded at once on threads and the file assembled after;
the result is the same.

A stream is corrupt when: a count or length is cut, a frame is longer than `FRAME_MAX`, the chunks
do not account for the frames of the layout, the output would be longer than the length, a frame
does not decode to its length, or bytes are left in an input.

## 8. The encoder *(informative)*

`formula::encode(file, threads)` returns a formula only if the file has a video stream of DivX 3,
at least 4 frames, none longer than `FRAME_MAX`, and the coded frames come to at most 97% of the
frames' bytes. The frames are cut into chunks of about 16 MiB (a chunk is closed before the first frame
once the frames in it have reached 16 MiB), and the chunks coded on `threads` threads. Cutting
the stream in chunks costs 0.12% of the video; the coded model is not shared.

The probing, the tables and the measurements are in `docs/div3-design.md`.
