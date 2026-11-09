# aqr-zstd format

aqr-zstd has no module and stores nothing of its own: a Zstandard file ([RFC 8878]) stays as it
is, and what its frames hold is taken from them by the archive's own instructions (spec, "Patch
instruction", "Split instruction"). This document describes the formulas it makes, so that what
they hold is clear; a decoder needs nothing beyond the spec.

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
