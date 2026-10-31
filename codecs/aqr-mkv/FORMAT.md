# The formula of a Matroska file

aqr-mkv has no module: the formula of a Matroska or WebM file is slices of two streams, the
root a concatenation of them. This document tells what the streams hold, for whoever reads or
recompresses the formula.

## Streams

| stream | class | content | what |
|---|---|---|---|
| 0 | `mkv/data` | no | the file's bytes in order, but for the keyframes of its video tracks and the frames seen before |
| 1 | `mkv/keys` | no | the keyframes of its video tracks, each the first time it is seen, in order |

A frame is a block's bytes after its track number, timecode and flags (README "What it reads").
Of the frames of 256 bytes or more, in the file's order:

- one with the same bytes as one before is a slice of where that one is, in either stream;
- else a keyframe of a video track is put at the end of stream 1, and the root slices it there;
- else it stays in stream 0 with the bytes around it.

The root is the concatenation of those slices and the slices of stream 0 between them, adjacent
slices of stream 0 merged. Stream 1 may be empty.
