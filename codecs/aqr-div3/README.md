# aqr-div3

The video of AVI files made by DivX 3 (MS-MPEG4 v3, `DIV3`/`MP43` and kin): each frame parsed down
to its macroblock types, motion vectors, DC values and AC coefficients, which are coded again by an
adaptive context model, and written back bit for bit as the original variable-length codes. The
archive then stores the coded frames, the AVI's other bytes (the skeleton), and the places of the
frames and audio chunks (the layout). An AC-3 audio stream goes through [aqr-ac3](../aqr-ac3/README.md)
at the same time, an MP3 one through [aqr-mp3](../aqr-mp3/README.md). An AVI of other video (DivX 4
and later, XviD) is taken for its audio alone, its frames kept as they are.
[FORMAT.md](FORMAT.md) specifies the streams and the module, and how it works.

The module is `div3-recons`, a part module (its memory does not grow with the file), whose frames
are coded in chunks of 16 MiB, so natively they are coded and decoded at once. A frame that does not
parse to its exact end is stored as it is. No DivX 3 specification exists: the tables were found
by probing an ffmpeg as a black box, no decoder's code read (FORMAT.md, *Design notes*).

```sh
cargo run --profile release-test -p aqr-div3 --example aqr-div3-check -- FILE.avi [FIRST [COUNT]]  # code, decode, compare; sizes by frame kind
cargo run --profile release-test -p aqr-div3 --example aqr-div3-speed -- FILE.avi [FRAMES [CHUNK_MB]]  # check / encode / decode speed, loss of chunking
AQR_DIV3_AVI=FILE.avi cargo rt -p aqr-div3                                # the roundtrip test on real frames
```

```rust
use aqr_div3::formula;
// None where neither the frames of `avi` (DivX 3) come to 97% by the model nor its audio (AC-3, MP3) to less.
let f = formula::encode(&avi, threads).unwrap();
// `f` rebuilds `avi` when evaluated with `formula::decode` as the module `formula::MODULE`.
```

## What is taken

AVI files of DivX 3 video (`DIV3`, `MP43` and kin), with AC-3 or MP3 audio or none. Not taken: other
video's frames (DivX 4 and later, XviD), kept as they are, only their audio taken.

## Results

Measured on `mari.avi` (a truncated 640x352 DivX 3 movie with 5.1 AC-3 audio, 675 MB;
corpus `F:\Data\aqr-corpus\aqr-div3`, for measurements only), 2026-11-11, `aqr -c` all threads:

| | before | after | |
|---|---|---|---|
| video, 20,000 frames | 99.80 MB | 88.53 MB | 88.7% |
| audio (AC-3) | 206.3 MB | 180.6 MB | 87.6% |
| the file | 675.28 MB | 593.08 MB | 87.8% |

Compressing ran at 37.9 MB/s (17.8 s). On one 74 s 640x480 video encoded three times with the
same MP3 audio (`cargo aqr -c`, 16 threads, 2026-11-15): DivX 3 low-motion 14.73 -> 13.27 MB
(90.1%), fast-motion 10.60 -> 9.48 MB (89.4%), DivX 4 (audio only) 11.99 -> 11.65 MB (97.1%), the
three in one archive 37.32 -> 30.32 MB (81.2%: the audio is held once). One thread decodes the
frames at 15.9 MB/s and codes at 11.2 MB/s.

## Tests

- `AQR_DIV3_AVI=FILE.avi cargo rt -p aqr-div3`: the roundtrip on real frames.
- `tests/avi.rs`: the roundtrip on real frames (`AQR_DIV3_AVI`).
- `tests/edge.rs`: a committed DivX 3 AVI end to end, AVIs of odd structure, the module's plans at
  large pictures, damaged or lying module inputs.
- `tests/overread.rs`: a coded stream cut short ends in an error; many small chunks cost what
  they hold.

## Left

- **Frames that do not parse.** About 0.5% of P frames (always with motion vector table 0) are
  stored as they are; the table probably lacks codes (`docs/div3-design.md`).
- **Quantizer 2 P frames.** They use the tables of 3-5, unconfirmed.
- **Richer model.** Contexts are small (neighbouring macroblock types, zero vectors, scan index,
  previous level); neighbour blocks' coefficients, predicted motion vectors and mixing would gain
  more at a cost in speed.
- **Other syntaxes.** Only DIV3 files were seen, though `mp43`, `div4`, `div5` and `div6` are
  accepted; frames of other syntaxes do not parse and are kept (a file over 97% is left alone).
