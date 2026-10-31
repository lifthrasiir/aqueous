# aqr-mkv

Matroska and WebM files (`.mkv`, `.webm`, `.mka`, `.mk3d`, `.mks`) taken apart by their frames,
the frames seen before held once (`FORMAT.md`). There is no module: the file is slices of two
streams, the keyframes of its video tracks and the rest of its bytes, and a frame seen before is
a slice of where it was first.

What it is for is the video of a still picture, as music is often uploaded to YouTube: the
encoder codes the same picture again at each keyframe (every 3 to 7 seconds), byte for byte the
same, and those keyframes are most of the video's bytes; the frames between are a few dozen
bytes each. The frames are not decoded: what is stored once is the muxer's frame, the codec's
bytes as they are (VP9 and AV1 alike). The keyframes are a stream of their own so that those
alike but not the same (another picture now and then, the same one coded again at another
quality) are next to each other for zstd; the frames between and the audio stay where they are,
in the rest, as a slice for each would cost more than they compress to.

## What it reads

The EBML header (DocType `webm` or `matroska`), then the Segment's elements: the Tracks' entries
for the video tracks (TrackType 1), and the Clusters (of known or unknown size) for their
SimpleBlocks and the Blocks of BlockGroups. A block's frame is all of it after the track
number, timecode and flags (its lacing, if any, as it is); a SimpleBlock with the keyframe
flag of a video track is a keyframe. The walk stops at the first element that does not read or
passes the end of the data; the bytes from there are kept as they are.

A file is taken where its frames seen before (of 256 bytes or more) come to a hundredth of it or
64 KiB, whichever is more, and to more than the formula's description takes. Else it is stored as
it is, as it was before aqr-mkv, and not looked into.

## How it does

Measured 2026-10-31 on an Apple M4 (10 cores; all threads, level mid) through `cargo aqr -c` on
11 WebM files from YouTube of 2025 (VP9 or AV1 with Opus; 9 of them still pictures with music,
2 of them moving), 738.08 MB:

| | before (stored) | after |
|---|---|---|
| archived | 738.08 MB | 392.26 MB (0.531; `zstd -3 --long=27` 0.532) |
| compressed in | 0.65 s, 11.2 G instructions | 1.81 s, 105.7 G instructions |
| tested (`aqr -t`) | | 0.28 s, 2.4 G instructions |
| peak resident | 1.47 GB | 1.94 GB |

A still picture's file comes to 0.32 to 0.67 of itself; most of what is left is its Opus audio
(68.6 MB of the 75.6 MB of the longest), which nothing here compresses. The two moving videos
are not taken. Of the instructions, most are zstd's: aqr-compress weighs the formula against the
file by compressing both. Taking it unweighed (as aqr-ppmd's is) halved them but archived 1 MB
more, as the streams then compressed less (a file of 21.6 MB alone 12.43 → 13.42 MB); why
is not known yet.

A frame between keyframes, the audio and the Matroska elements could go to streams of their own
too: on these files a slice for each made the formula about 3 bytes a frame (2.1 MB for an hour),
and the result the same as with the keyframes apart alone.

```sh
cargo run -r -p aqr-mkv --example aqr-mkv-check -- FILE...   # frames, repeated, streams
```

## Status

Left:

- The Opus audio: no recompressor (aqr-ogg leaves Opus as it is too).
- Keyframes alike but not the same, beyond what zstd finds (near-duplicate pictures, coded
  again): that is decoding the codec.
- Why the formula unweighed archives larger (above).
- MP4 (`.mp4`, `.m4v`, `.mov`): the same idea over `stsz`/`stco`, its samples by track.

## Tests

`cargo rt -p aqr-mkv`: the walk over clusters of known and unknown sizes, a file cut short, and
the formula. aqr-compress's `mkv_keyframes_once` archives a WebM file of a still picture.
