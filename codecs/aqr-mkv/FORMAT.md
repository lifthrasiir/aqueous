# The formula of a Matroska file

aqr-mkv has no module: the formula of a Matroska or WebM file is slices of two streams, the
root a concatenation of them. This document tells what the streams hold, for whoever reads or
recompresses the formula.

## What it reads

The EBML header (DocType `webm` or `matroska`), then the Segment's elements: the Tracks' entries
for the video tracks (TrackType 1), and the Clusters (of known or unknown size) for their
SimpleBlocks and the Blocks of BlockGroups. A block's frame is all of it after the track
number, timecode and flags (its lacing, if any, as it is); a SimpleBlock with the keyframe
flag of a video track is a keyframe. The walk stops at the first element that does not read or
passes the end of the data; the bytes from there are kept as they are.

A file is taken where its frames seen before (of 256 bytes or more) come to a hundredth of it or
64 KiB, whichever is more, and to more than the formula's description takes.

## Streams

| stream | class | content | what |
|---|---|---|---|
| 0 | `mkv/data` | no | the file's bytes in order, but for the keyframes of its video tracks and the frames seen before |
| 1 | `mkv/keys` | no | the keyframes of its video tracks, each the first time it is seen, in order |

A frame is a block's bytes after its track number, timecode and flags (*What it reads*).
Of the frames of 256 bytes or more, in the file's order:

- one with the same bytes as one before is a slice of where that one is, in either stream;
- else a keyframe of a video track is put at the end of stream 1, and the root slices it there;
- else it stays in stream 0 with the bytes around it.

The root is the concatenation of those slices and the slices of stream 0 between them, adjacent
slices of stream 0 merged. Stream 1 may be empty.

## Design notes

- The keyframes are a stream of their own so that those alike but not the same (another picture now
  and then, the same one coded again at another quality) are next to each other for zstd; the frames
  between keyframes (a few dozen bytes each) and the audio stay in stream 0, as a slice for each
  would cost more than they compress to.
- Rejected: a stream each for the frames between keyframes, the audio and the Matroska elements: a
  slice for each made the formula about 3 bytes a frame (2.1 MB for an hour), the result the same
  as with the keyframes apart alone.
- aqr-compress weighs the formula against the file by compressing both; most of the compression
  instructions are zstd's. Unweighed (as aqr-ppmd's is) they halve but 1 MB more is archived, as
  the streams then compress less (a 21.6 MB file alone 12.43 -> 13.42 MB); why is not known.
