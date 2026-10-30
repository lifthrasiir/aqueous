# aqr-gif

GIF files whose images' LZW data is rebuilt bit for bit from the images' pixels and a record of
the encoder's choices given them. The archive stores the file's skeleton (the file without the
LZW data: headers, color tables, extensions) and a record, in which a context model codes the
pixels, palette indices, far better than LZW did (about half the size of the LZW data).

No encoder is reproduced. The record codes the LZW codes against what an encoder of the longest
match writes: where it clears, whether each code is the longest match and else how much
shorter, and whether it is the first code the dictionary defines for its pixels. The encoders
seen all write the longest match and clear when the dictionary is full, so this costs a few
bytes a file. The pixels' model mixes contexts of the neighbouring indices, of the colors the
palette gives them, and of the image before (animations), as lpaq does. The format is in
[FORMAT.md](FORMAT.md), written from the GIF89a specification.

```sh
cargo run --release -p aqr-gif --example aqr-gif-check -- FILE_OR_DIR...   # split, rebuilt, what the records cost
.venv/bin/python -I codecs/aqr-gif/scripts/gen-fixtures.py                  # tests/data from ImageMagick, ffmpeg, Pillow, sips
```

```rust
use aqr_gif::formula;
// None where `file` is not a GIF file, or none of its images decodes.
let f = formula::encode(&file, 1).unwrap();
// `f` rebuilds `file` when evaluated with `formula::decode` as the module `formula::MODULE`.

use aqr_gif::recons;
let sp = recons::split(&file)?;
let mut out = Vec::new();
recons::join_onto(&sp.skeleton, &sp.record, &mut out)?;
assert_eq!(out, file);
```

## What is taken

- Every image of a minimum code size of 2 to 8 whose data decodes to a pixel or more: as far as
  it decodes (codes not defined, and bytes after the end code, are kept in the record), with as
  many pixels as its codes give (fewer or more than the image's), interlaced or not.
- Files cut in an image's data (a download stopped): the sub-block cut is kept.
- Images of other minimum code sizes, and what follows the blocks, stay in the skeleton.
- aqr-compress takes GIF files at the start of a file and found in other data, up to their
  trailer, by their magic bytes (not by the name `.gif`).
- Not files whose record, once their images coded hold a quarter of the LZW data (of 1 Mbit at
  least), has taken more than 5/4 of its bits: the encoder gives them up there rather than at
  the end. These are the files of lossy encoders (Photoshop's "Lossy"), whose pixels are made
  to fit LZW's dictionary; the model codes them up to 2.3 times larger than LZW. Of the 117 GIF
  files of the 27 asar archives of aqr-asar's corpus, 11 of 12 such files are given up, none of
  the others (at most 1.16 there; the 12th is 1.22).

## Results

Measured on 2026-10-31 on an Apple M4.

| set | files | MB | LZW data MB | records MB | aqr before | aqr now | zstd -3 |
|---|---|---|---|---|---|---|---|
| GIF files of the 10 GB corpus (`corpus/10gb`) | 488 | 9.247 | 9.044 | 4.703 | 9.127 | 4.826 | 9.072 |
| other GIF files (`corpus/misc`) | 4 | 1.345 | 1.257 | 0.451 | 1.311 | 0.497 | 1.270 |

"aqr before" and "now" are `aqr -c` of the directory (level mid, all threads), at `285e66a` and
with aqr-gif; "LZW data" is the files less their skeletons. The 488 files are those of
`~/tmp/10gb` (`docs/10gb-todo.md`) of distinct contents: three photographs of 2.2–2.4 MB
dithered to 256 colors (their records 3.61 MB of 6.91 MB), and icons and web graphics.
`corpus/misc` is an animation of 42 frames from an application (1.1 MB), an image of Tk's demos,
one of codec-corpus, and a file named `.gif` that is not one (a Perl module's table).

- `aqr -c` of `corpus/10gb` takes 3.4 s (0.3 s before aqr-gif; 13.9 s of threads), with a peak
  of 0.5 GB (55 MB before); `aqr -t` 1.9 s (0.4 GB), and 2.6 s with the modules as Wasm. Of
  `corpus/misc`, 2.5 s and 1.4 s (2.0 s as Wasm).
- The model codes 3.5 megapixels a second of a dithered photograph on one thread, 6 of a map of
  flat colors and 15 of an animation (a pixel equal to the one on its left is a single bit), and
  decodes a fifth slower. Its tables are 4 slots of 16 bits a pixel, up to 32 MB for 0.5
  megapixels and more; the module's working memory is 69 MB for an image of 4 megapixels.
- Records are 0.50 of the LZW data of all 521 files of `corpus/` (also the encoders' files
  of `corpus/enc`, made as `scripts/gen-fixtures.py` makes its own from four larger pictures:
  0.27 for ffmpeg's animation to 0.69 for an icon of 32 by 32).
- The codes cost 4.2 KB of the 521 files, most of it the 107 icons of Apache's encoder, which
  defines codes again (§9 of FORMAT.md); the 3 photographs' 1.1 KB.

The module `gif-recons` is 46 KB of Wasm (a block of 14 KB in an archive), which files below
30 KB or so do not pay for alone.

### Speed against size

The first version of the model (12 contexts, tables of up to 2^22 slots, every index bit by bit)
gave records 1.1% smaller (4.776 MB for `corpus/10gb`) at about twice the instructions, and a
peak of 1.2 GB for `aqr -c` of `corpus/10gb`. What was given up, as `aqr-gif-check` measures
(records of all of `corpus/`, 5.74 MB at first; instructions of the three photographs and of
the other 485 files of `corpus/10gb`, 91 G and 78 G at first):

| change | records | photographs | other files |
|---|---|---|---|
| contexts of W, N and of W, N, NE, NNE dropped (the image before's is W, N where there is none); mixers of 16 lanes of `i32`, vectorized, weights within ±8 | +0.24% | −43% | −43% |
| first, whether the index is W's | +0.42% | −15% | −42% |
| tables of 2^21 slots at most, 4 a pixel; the APM's contexts one for 8 pixels | +0.43% (the animation of `corpus/misc` +2.9%) | ±0 | ±0 |

Then the APM (an adaptive probability map over the mix, by the context of W and N and the
bit's node) was dropped, measured on Windows (x86-64) on the 106 GIF files of aqr-asar's corpus
that are coded: records +0.65% (22.77 -> 22.92 MB; the fixtures' +0.08%), 89 -> 76 s on one
thread. A bit's time is a chain of dependent steps rather than arithmetic: tables of 2^16 slots
gave −7%, the mixers in AVX2 nothing. Then the pixels whose W, N, NW and NE are the same (88%
of those files', 99.5% of them W's) were coded by a small model of their own (two slots, of a
context of whether P, WW and NN are W's and of the run of such pixels, and of W and P hashed,
mixed), not the 10 contexts: records −0.67% (22.92 -> 22.77 MB; the fixtures' −0.26%), 76 ->
54 s.

Left out: a single mixer (+0.6%, −14%), and the W bit gated by how often pixels were W's (larger
at every threshold: the tree's tables learn from both cases).

## Tests

```sh
cargo rt -p aqr-gif    # fixtures of encoders, test encoders of every choice, cut files, damaged records
```

The fixtures are `scripts/gen-fixtures.py`'s, and `dup-box2.gif`, Apache's icon `box2.gif` (in the
public domain, as its comment says), of an encoder that defines codes again.

`cargo test` (debug) catches an overflow from a damaged record as a panic. aqr-compress's
`tests/compress.rs` (`gif_files`) takes the fixtures alone and found in other data, and
aqr-decompress's `tests/wasm.rs` (`gif_same_as_wasm`) runs them as Wasm.

## Status

Done: the module `gif-recons` (natively and as a Wasm module of aqr-modules), GIF files in
aqr-compress (at the start of a file and in other data).

Not yet:

- Pixels of an animation's frame coded against the frame shown before it as a disposal method
  and transparency make it (the model has the last frame's indices on the screen as a context,
  not the image shown).
- A palette sorted for the model (indices close in color as close values), and contexts of
  colors beyond those of the neighbours.
- LZW of other formats: TIFF (its LZW is GIF's of MSB-first codes and an early change of
  width), PDF's `LZWDecode`, Unix `compress` (`.Z`).
