# aqr-div3

The video of AVI files made by DivX 3 (MS-MPEG4 v3, `DIV3`/`MP43` and kin): each frame parsed down
to its macroblock types, motion vectors, DC values and AC coefficients, which are coded again by an
adaptive context model, and written back bit for bit as the original variable-length codes. The
archive then stores the coded frames, the AVI's other bytes (the skeleton), and the places of the
frames and audio chunks (the layout). An AC-3 audio stream goes through [aqr-ac3](../aqr-ac3/README.md)
at the same time. [FORMAT.md](FORMAT.md) specifies the streams and the module.

The two frame types of DivX 3 files, I and P (there are no B frames), are walked by one function
over [aqr-walk](../../crates/aqr-walk/README.md)'s `Io`, which serves coding, decoding and a dry
parse. A frame that does not parse to its exact end, with at most a short tail, is stored as it is
(a flag, then its bytes). The module is `div3-recons`, a part module (windows of fixed pages), so its
memory does not grow with the file; coding is in chunks of 16 MiB of frames, each with a model of
its own, so natively they are coded and decoded at once.

No DivX 3 specification exists. The VLC tables (DC, intra-frame CBP, six AC tables, the P-frame
macroblock types, two motion vector tables) were extracted by treating an installed ffmpeg decoder
and encoder as a black box and probing it with crafted bit strings (`scripts/div3-probe/`); no
decoder's code or tables were read or copied. `src/tables.rs` is generated from the probed
tables by `scripts/div3-gen.py`.

```sh
cargo run --profile release-test -p aqr-div3 --example aqr-div3-check -- FILE.avi [FIRST [COUNT]]  # code, decode, compare; sizes by frame kind
cargo run --profile release-test -p aqr-div3 --example aqr-div3-speed -- FILE.avi [FRAMES [CHUNK_MB]]  # check / encode / decode speed, loss of chunking
AQR_DIV3_AVI=FILE.avi cargo rt -p aqr-div3                                # the roundtrip test on real frames
```

```rust
use aqr_div3::formula;
// None where `avi` has no DivX 3 stream, or its frames do not come to less than 97% by the model.
let f = formula::encode(&avi, threads).unwrap();
// `f` rebuilds `avi` when evaluated with `formula::decode` as the module `formula::MODULE`.
```

## Results

Measured on 2026-11-11 on `mari.avi` (a truncated 640×352 DivX 3 movie with 5.1 AC-3 audio
from a CD image, 675 MB; Nandub 1.0rc2; corpus `F:\Data\aqr-corpus\aqr-div3`, for measurements only):

| | before | after | |
|---|---|---|---|
| video, 20,000 frames | 99.80 MB | 88.53 MB | 88.7% |
| audio (AC-3, 206.3 MB) | 206,306,429 | 180,634,109 | 87.6% |
| the file | 675.28 MB | 593.08 MB | 87.8% |

`aqr -c` took 17.8 s (37.9 MB/s, all threads), the archive extracted byte for byte. One thread
decodes the frames at 15.9 MB/s and codes them at 11.2 MB/s (the dry parse included); cutting the
stream in 16 MiB chunks costs 0.12% of the video.

## Left

- Frames that do not parse: about 0.5% of P frames (always with motion vector table 0) are stored
  as they are. The table probably lacks codes; see `docs/div3-design.md`.
- Quantizer 2 P frames use the tables of 3–5, unconfirmed.
- The model uses small contexts only (neighbouring macroblock types, motion vectors being zero, the
  scan index and the previous level). Neighbour blocks' coefficients and predicted motion vectors
  as contexts, and mixing, would gain more but cost speed.
- Only DivX 3's syntax was probed. The handler fourccs `div3`, `mp43`, `div4`, `div5` and `div6`
  are accepted, but nothing but DIV3 files was seen; frames of other syntaxes do not parse and
  are stored as they are (and a file where they come to over 97% is left alone).
