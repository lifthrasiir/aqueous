# aqr-zstd format

A Zstandard file ([RFC 8878]) stays as it is, and what its frames hold is taken from them by the
archive's own instructions (spec, "Patch instruction", "Split instruction"): the first part of
this document describes the formulas made of it, so that what they hold is clear; a decoder needs
nothing beyond the spec. The module `zstd-frames` ([below](#the-module-zstd-frames)) makes frames
in other data (a database's blobs) again of their contents.

[RFC 8878]: https://www.rfc-editor.org/rfc/rfc8878

## Frames

A file of frames is Zstandard frames and skippable frames one after another from its start,
as long as their structure holds: each frame's header, its blocks' headers (the last one marked
the final one) and its checksum if its header tells one. What follows them is something else.

A frame's **blocks** are the bytes from its first block's header to the end of its final block:
the frame without its header and its checksum.

A frame is taken from only if:

- it has no dictionary (its `Dictionary_ID` is 0 or absent);
- it decompresses, of the content size its header tells if it tells one, and of its checksum if
  it has one;
- its content is at most 2^31 bytes (the window of a Patch instruction's frame, a single segment
  of its content, in libzstd), and at most the bound the compressor keeps applications under.

## Why a Patch instruction gives a frame's content

A Patch instruction decompresses Zstandard blocks with its source as a raw content dictionary:
the entropy tables start empty and the repeat offsets at (1, 4, 8), as they do at the start of a
frame without a dictionary. With an empty source, its blocks are decompressed as the frame's
are. The frame's `Window_Size` does not apply to a patch (any byte of the output so far may be
matched), which allows all that the frame's window allows. Each block of a frame holds at most
`Block_Maximum_Size` = min(`Window_Size`, 128 KiB) bytes, as a patch's blocks must. A Patch
instruction does not check the frame's checksum: the archive's output checksums cover the files
taken.

## The formula

The files taken from a file of frames are given ranges `wanted` of its frames' contents put one
after another (apart, in order, none empty). Each frame that a range is in (frames none is in
are left out) is:

```text
patch = Slice(the frame's blocks in the file's stream)
Patch(Concat(), patch)            of the frame's content size
Split(sizes)                      into its pieces
```

Its **pieces** are cut at the frame's ends and at the ranges' ends: each is within one range
(the range's) or between them (nobody's).

- A range's file is the concatenation of its pieces' outputs, in order (over several frames if
  the range is).
- The pieces between ranges are dropped: those before a range (since the range before it) by
  `Patch(Concat(those pieces), [01 00 00])`, of an empty output, put before the range's pieces in
  its file; those after the last range likewise, after its pieces. `01 00 00` is a raw block of
  no bytes, the final one.

Every output of a Split is so taken once, as the spec wants of every byte sequence. The files
taking pieces of the same frames are the outputs of one formula block (a Split's outputs are
taken in the formula block that makes them), in the order of their pieces.

## Encoder choices (informative)

- aqr-compress takes the files of a batch (whole files, before they are recompressed) whose
  size and XXH3-128 are those of a piece of the content: all of it, or a member of a tar, ar,
  cpio or asar archive or an ISO 9660 or UDF image that the content is. Of those overlapping,
  the longest; each file once. A file of frames gives to others and takes from none.
- A file whose content is not taken is archived as before: the file of frames, its content not
  looked at further.

## The module `zstd-frames`

`zstd-frames` version 1 is a part module (spec, "Module execution"): a file of bytes kept and of
Zstandard frames that libzstd wrote, each frame made again of its content by the compressor of
libzstd that it emulates (`src/compress`), and put where its pieces are. A frame's pieces may be
anywhere in the file and in any order (a database's blobs, on pages of overflow chains), so a
frame is made where its first piece in the file is and held until all of it is put.

### Params

| field | type | |
|---|---|---|
| level | u8 | the compression level of libzstd the frames are of: 3 |
| content | varint | the most bytes of a frame's content |
| pool | varint | bytes of the pool of the frames open (below) |
| open | varint | the most frames open at once |

Everything is LEB128 varints. The params must be exactly these; `content` is at most 262143
(2^18 - 1), the most emulated: libzstd may cut the blocks of more otherwise than at 128 KiB.

### Inputs

0. the layout (below);
1. the bytes kept, one after another in the order of the file;
2. the contents' lengths, a varint each, in the order the frames are made;
3. the contents, one after another in that order.

### The frames

The frame of a content is what libzstd 1.5.x's `ZSTD_compress` writes of it at the level, with no
dictionary: its parameters those of the level for the content's size (the size known), a frame
header with the content's size and no checksum, blocks of 128 KiB, the double-fast match finder,
the literals and sequences coded as libzstd chooses. The emulation (`compress::compress`) is the
reference of these choices; frames of any other compressor are kept as bytes.

### The layout

A layout is steps one after another, each a varint `tag`, of `n = tag >> 2` bytes put, by
`tag & 3`:

- 0, bytes kept: the next `n` bytes of input 1;
- 1, a new frame: the next frame is made, of the next length of input 2 and that many bytes of
  input 3, and opened (the newest of the frames open); then a varint `delta`, and the `n` bytes of
  the frame from `delta` are put;
- 2, a piece of a frame open: varints `j` and `delta`; the `n` bytes of the `j`-th newest frame
  open (0 the newest) from `next + delta` are put, where `next` is where the last piece put of it
  ended.

`delta` is zigzag (`(v >> 1) ^ -(v & 1)`). The bytes put must be in the frame. A frame is closed
(it is no longer open, and the frames after it move up a place) once as many bytes are put of it
as it has. Once the layout is done, every input must be taken whole, no frame open, and the
bytes put the output's length; anything else is corrupt.

The frames open are held in a pool of `pool` bytes: each takes a block of its length rounded up to
64 bytes, the first free span (by place) that it fits, else the end of those taken; a closed frame
gives its block back, joined with the free spans next to it. A layout whose frames would take more
than `pool` bytes of the pool, or more than `open` frames open at once, is corrupt. (The encoder
tells the most its layout takes, of the same steps.)

### Memory

The module lays its memory out as a stack (aqr-api's `stack`): the params, the readers' pieces of
its inputs, the frames open and the free spans, the pool, a content and a frame, and a compressor
for contents of up to `content` bytes (its tables and buffers, made once). Its plan
(`frames::plan`) is of the params alone.

### Encoder choices (informative)

- aqr-compress takes a SQLite database whose blobs are mostly frames (`sqlite-zstd`: a quarter of
  the first 1024 pages walked of its b-trees at least), its blobs found by aqr-sqlite's `blobs`
  (each blob's pieces: on its b-tree page, then on its overflow pages).
- A frame is taken where it decompresses, and the emulation makes it again byte for byte, and its
  pieces are apart from the others'. Where the frames open at once would be more than 96 MiB (or
  their pool more than 160 MiB), those closing last are left out, as bytes kept.
