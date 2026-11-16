# aqr-base64 format

This document specifies the module `base64-recons` of aqr-formula entry formulas, which writes
a text file again from the text without its base64 blocks, the bytes those blocks decode to,
and a layout of where they lie and how their lines are cut. It is meant to be complete: the
module can be implemented from it alone. Sections marked *(encoder)* describe the choices this
implementation makes when it builds formulas; the module does not depend on them.

## 1. Conventions

- Integers are unsigned. **varint**: unsigned LEB128, least significant group first, at most
  10 bytes (the tenth at most 1).
- A **run** is one base64 block of the file: its characters (those of the alphabet, and the
  `=` of the padding), with a separator between its lines where it has more than one.

## 2. The module

- **Module**: name `base64-recons`, version 1, a part module (spec, "Module execution"): its
  inputs are taken, and the file put out, in order, through windows of 4 pages.
- **Params**: none (empty).
- **Resources**: none.
- **Inputs**: exactly 3:
  0. `text` (class `base64/text`, a content, kind text): the file with the runs cut out.
  1. `data` (class `base64/data`, a content): the bytes of all the runs, one after another.
     *(encoder)* In the formula it is a concatenation of slices: one for each run of
     `base64:apart` bytes or more (16 KiB by default), a content of its own, and one for the
     runs between such runs together.
  2. `layout` (class `base64/layout`): §3.
- **Output**: the file, `len` bytes (the declared length).

## 3. The layout

The layout is a header and the runs, to its end:

- the header: `varint count` of separators, at most 64; then for each separator `varint len`
  (from 1 to 32) and its `len` bytes;
- then, up to the end of the layout, each run: `varint gap`, `varint n`, `varint first`,
  `varint line`, `varint sep`, and a byte `flags`:
  - `gap`: the bytes of `text` before this run, after the text taken for the one before;
  - `n`: the bytes of `data` that the run decodes to, at least 1, and not more than are left;
  - `first`: the characters on the run's first line; 0 where the run is on one line and is not
    cut at all;
  - `line`: the characters on each line after the first (0 where `first` is 0);
  - `sep`: the index of the separator between lines (0 where `first` is 0);
  - `flags`: bit 0, the run is padded with `=`; bits 1 to 3, the alphabet: 0 the standard
    (`A-Za-z0-9+/`), 1 the URL-safe (`A-Za-z0-9-_`); the others are reserved and rejected, and
    so are bits 4 to 7.

## 4. Decoding

The output is, for each run in order, `gap` bytes of `text`, then the run; at the end, the rest
of `text`. The decoder rejects, with an error, anything of the following not held (and never
reads past an input or writes past `len`): `text`, `data` and `layout` all used up exactly,
the output exactly `len` bytes.

The run of `n` bytes is of `C` characters: with padding `4 ⌈n/3⌉`, without `⌈4n/3⌉`. They are
the base64 of the bytes in the alphabet of the flags, in the usual way: each three bytes four
characters of their 24 bits as four 6-bit values, a last group of one byte two characters (and
`==` where padded), of two bytes three (and `=` where padded), the unused low bits of the last
character zero.

Where `first` is 0, `line` and `sep` must be 0, and the run's `C` characters follow one
another. Otherwise `first` must be less than `C`, `line` at least 1 and `sep` less than the
count of separators; the first `first` characters, then the rest cut into lines of `line`
characters (the last of what is left, the shorter), a separator before each of them, the
separator `sep`. There is none after the last character. (The padding's `=` are characters of
the last line.)

## 5. *(encoder)* Finding runs

`encode` goes over the file once for each alphabet it has a line of 40 characters of (a quick
look, `starts`, that most files fail: it is asked of every text), and takes a run where:

- **Lines.** A line is a maximal string of alphabet characters; a `\` before it with `n`, `r` or
  `t` after (a JSON escape) is not of it. A run starts at a line of 40 characters or more. The
  separator after a line is the non-alphabet bytes up to the next alphabet character (at most
  32; a `\` and the `n`, `r` or `t` after it are taken in, whichever the bytes after them, as
  JSON writes its line breaks); the lines of the run are those that follow, each at the
  separator, of the length of the second line (where it is followed by another line after the
  same separator; else the longer of the first and the second), the last of them shorter or
  as long. A line followed by `=` is the last: the one or two signs are of it. Lines after the
  first shorter than 40 characters are not of a run (encoders wrap at 64 or 76): the first
  line is then a run alone. A run of one line is `first` 0.
- **Validity.** The characters are of a number that a padded run has (a multiple of 4), or, for
  an unpadded one, not 1 more than a multiple of 4, and the low bits of the last character past
  the bytes are zero (any other text is not written again). Where they are not, the run is
  tried without the padding signs, then without its last line, and then not taken.
- **What a run is not.** Fewer than `base64:min` bytes (`-Z base64:min`, 192 by default);
  characters without a capital, a lowercase letter or a digit among them; characters that are
  hexadecimal digits alone. A run of the URL-safe alphabet must have a `-` or `_`.
- **Share.** The runs taken decode to at least `base64:share` percent of the file (1 by
  default), or none is: the formula holds all of the text again.
- **Separators.** At most 64 distinct, as the bytes are; a run past them is not taken.
- **Check.** The formula is evaluated before it is returned, and it must give the file.

The formula is the application of the module to `text` (a content of kind text), a concatenation
of slices of `data` (a content of kind unknown: each slice is recompressed as the file it is; a
slice for each run of `base64:apart` bytes or more, and one for the smaller runs between them),
and `layout`.

## 6. Memory

The module's working memory is a constant: its plan is the buffers it takes, whatever the
lengths: 4096 bytes of the layout, 2048 of separators, 24576 of text or data, 32772 of
characters and 65536 of output, each rounded up to 16 bytes on the stack (`aqr_api::stack`).
