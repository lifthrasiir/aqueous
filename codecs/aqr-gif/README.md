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

## Results

Measured on 2026-10-31 on an Apple M4.

| set | files | MB | LZW data MB | records MB | aqr before | aqr now | zstd -3 |
|---|---|---|---|---|---|---|---|
| GIF files of the 10 GB corpus (`corpus/10gb`) | 488 | 9.247 | 9.044 | 4.653 | 9.127 | 4.776 | 9.072 |
| other GIF files (`corpus/misc`) | 4 | 1.345 | 1.257 | 0.443 | 1.311 | 0.489 | 1.270 |

"aqr before" and "now" are `aqr -c` of the directory (level mid, all threads), at `285e66a` and
with aqr-gif; "LZW data" is the files less their skeletons. The 488 files are those of
`~/tmp/10gb` (`docs/10gb-todo.md`) of distinct contents: three photographs of 2.2–2.4 MB
dithered to 256 colors (their records 3.58 MB of 6.91 MB), and icons and web graphics.
`corpus/misc` is an animation of 42 frames from an application (1.1 MB), an image of Tk's demos,
one of codec-corpus, and a file named `.gif` that is not one (a Perl module's table).

- `aqr -c` of `corpus/10gb` took 10.0 s (0.3 s before; 21.7 s of threads, 3 to 4 s of them each
  photograph's model), with a peak of 1.2 GB (55 MB before); `aqr -t` 2.5 s (1.0 GB), and 7.1 s
  with the modules as Wasm.
- The model codes about 2 megapixels a second of 8-bit indices on one thread (4 of an animation
  of few colors), and as many decoding. Its tables are 8 slots of 16 bits for a pixel, up to
  4 megapixels (100 MB).
- Records are 0.49 of the LZW data of all 521 files of `corpus/` (also the encoders' files
  of `corpus/enc`, made as `scripts/gen-fixtures.py` makes its own from four larger pictures:
  0.28 for ffmpeg's animation to 0.71 for an icon of 32 by 32).
- The codes cost 4.2 KB of the 521 files, most of it the 107 icons of Apache's encoder, which
  defines codes again (§9 of FORMAT.md); the 3 photographs' 1.1 KB.

The module `gif-recons` is 42 KB of Wasm (a block of 14 KB in an archive), which files below
30 KB or so do not pay for alone.

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
