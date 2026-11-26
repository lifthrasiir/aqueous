# aqr-mkv

Matroska and WebM files (`.mkv`, `.webm`, `.mka`, `.mk3d`, `.mks`) taken apart by their frames,
the frames seen before held once. There is no module: the file is slices of two streams, the
keyframes of its video tracks and the rest of its bytes, and a frame seen before is a slice of where
it was first. `FORMAT.md` specifies the streams, and how it works.

What it is for is the video of a still picture, as music is often uploaded to YouTube: the
encoder codes the same picture again at each keyframe, byte for byte the same, and those keyframes
are most of the video's bytes. The frames are not decoded (VP9 and AV1 alike).

```sh
cargo run -r -p aqr-mkv --example aqr-mkv-check -- FILE...   # frames, repeated, streams
```

## What is taken

Matroska and WebM files (`.mkv`, `.webm`, `.mka`, `.mk3d`, `.mks`) whose frames seen before (of 256
bytes or more) come to a hundredth of the file or 64 KiB, whichever is more, and to more than the
formula's description takes; else the file is stored as it is. Not taken: MP4.

## Results

Apple M4, all threads, 2026-10-31, `cargo aqr -c` on 11 WebM files from YouTube of 2025 (VP9 or
AV1 with Opus; 9 still pictures with music, 2 moving), 738.08 MB:

| | before (stored) | after |
|---|---|---|
| archived | 738.08 MB | 392.26 MB (0.531; `zstd -3 --long=27` 0.532) |
| compressed in | 0.65 s | 1.81 s |
| tested (`aqr -K`, then `-t`) | | 0.28 s |
| peak resident | 1.47 GB | 1.94 GB |

A still picture's file comes to 0.32 to 0.67 of itself; most of what is left is its Opus audio.
The two moving videos are not taken.

## Tests

- `tests/edge.rs`: lacing, blocks in groups, unknown sizes, tracks numbered by 2 or more bytes,
  several Segments, bytes after the last, damaged files.
- Unit tests of the walk and the formula (`cargo rt -p aqr-mkv`); aqr-compress's
  `mkv_keyframes_once` archives a WebM file of a still picture.

## Left

- **Opus audio.** It is most of what remains of a still picture's file (68.6 MB of the 75.6 MB of
  the longest); there is no recompressor (aqr-ogg leaves Opus as it is too).
- **Keyframes alike but not the same.** Near-duplicate pictures beyond what zstd finds need the
  codec decoded.
- **Unweighed formula.** Taking the formula without weighing it by compressing both halves the
  compression instructions but archives 1 MB more on the 11 files; why is not known.
- **MP4.** `.mp4`, `.m4v`, `.mov`: the same idea over `stsz`/`stco`, its samples by track.
