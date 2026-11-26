# aqr-gif

GIF files whose images' LZW data is rebuilt bit for bit from the images' pixels and a record of
the encoder's choices given them. The archive stores the file's skeleton (the file without the
LZW data: headers, color tables, extensions) and a record, in which a context model codes the
pixels, palette indices, far better than LZW did (about half the size of the LZW data).

No encoder is reproduced: any LZW code sequence is taken, the usual ones (longest match, clear
when the dictionary is full) for a few bytes a file. `FORMAT.md` specifies the module
`gif-recons`, and how it works (written from the GIF89a specification).

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
// (the output written within its capacity, the pixels an image holds bounded)
let mut out = Vec::with_capacity(file.len());
recons::join_onto(&sp.skeleton, &sp.record, sp.held, &mut out)?;
assert_eq!(out, file);
```

## What is taken

- Every image of a minimum code size of 2 to 8 whose data decodes to a pixel or more: as far as
  it decodes (codes not defined, and bytes after the end code, are kept in the record), with as
  many pixels as its codes give (fewer or more than the image's), interlaced or not.
- Files cut in an image's data (a download stopped): the sub-block cut is kept.
- Images of other minimum code sizes, and what follows the blocks, stay in the skeleton.
- aqr-compress takes GIF files at the start of a file and found in other data, up to their
  trailer, by their magic bytes (not by the name `.gif`), from level 4 only (`format_on`; kept
  as they are below, as the model is slow for its bytes).
- Not files of lossy encoders (Photoshop's "Lossy"), whose pixels are made to fit LZW's
  dictionary: the model codes them larger than LZW, so the encoder gives them up midway (FORMAT.md,
  design notes).

## Results

Apple M4, 2026-10-31, `aqr -c` of the directory (level mid, all threads) against zstd -3:

| set | files | MB | LZW data MB | aqr without aqr-gif | aqr | zstd -3 |
|---|---|---|---|---|---|---|
| GIF files of the 10 GB corpus (`corpus/10gb`) | 488 | 9.247 | 9.044 | 9.127 | 4.826 | 9.072 |
| other GIF files (`corpus/misc`) | 4 | 1.345 | 1.257 | 1.311 | 0.497 | 1.270 |

The 488 files are those of `~/tmp/10gb` (`docs/10gb-todo.md`) of distinct contents: three
photographs dithered to 256 colors (3.61 MB of the 4.70 MB of records) and icons and web
graphics. `corpus/misc` is an animation of 42 frames, an image of Tk's demos, one of
codec-corpus, and a file named `.gif` that is not one.

- Records are 0.50 of the LZW data of all 521 files of `corpus/` (0.27 for ffmpeg's animation
  to 0.69 for a 32 by 32 icon).
- `aqr -c` of `corpus/10gb`: 3.4 s (13.9 s of threads), 0.5 GB peak; `aqr -K` then `-t` 1.9 s
  (0.4 GB), 2.6 s with the modules as Wasm.
- The model codes 3.5 megapixels a second of a dithered photograph on one thread, 6 of a map
  of flat colors and 15 of an animation; decoding is a fifth slower. Working memory is up to
  32 MB of tables and 69 MB for an image of 4 megapixels.
- The module is 34 KB of Wasm (a block of 11 KB in an archive), which files below 30 KB or so
  do not pay for alone.

## Tests

```sh
cargo rt -p aqr-gif    # fixtures of encoders, test encoders of every choice, cut files, damaged records
```

The fixtures are `scripts/gen-fixtures.py`'s (of ImageMagick's built-in images, under the
ImageMagick License: `tests/data/NOTICE.md`), and `dup-box2.gif`, Apache's icon `box2.gif` (in the
public domain, as its comment says), of an encoder that defines codes again.

`cargo test` (debug) catches an overflow from a damaged record as a panic. aqr-compress's
`tests/compress.rs` (`gif_files`) takes the fixtures alone and found in other data, and
aqr-decompress's `tests/wasm.rs` (`gif_same_as_wasm`) runs them as Wasm.

## Left

- **Animation frames against the image shown.** The model has the last frame's indices as a
  context, not the image shown after disposal and transparency; frames of animations would
  code smaller.
- **Palette order and color contexts.** A palette sorted so that near indices are near colors,
  and contexts of colors beyond the neighbours'.
- **Other LZW.** TIFF's (MSB-first codes, early change of width), PDF's `LZWDecode`, Unix
  `compress` (`.Z`).
