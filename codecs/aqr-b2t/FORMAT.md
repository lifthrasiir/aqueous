# aqr-b2t format

This document specifies two modules of aqr-formula entry formulas, binary-to-text encodings in
text files: `b2t-recons` (§2 to §6: base64 and hexadecimal blocks) and `b2t-recons-quopri` (§7
to §11: quoted-printable bodies). Each writes a text file again from the text without the
encoded parts, the bytes those parts decode to, and a layout of where they lie and what their
encoder chose. It is meant to be complete: the modules can be implemented from it alone.
Sections marked *(encoder)* describe the choices this implementation makes when it builds
formulas; the modules do not depend on them.

## 1. Conventions

- Integers are unsigned. **varint**: unsigned LEB128, least significant group first, at most
  10 bytes (the tenth at most 1).
- A **run** is one block of the file: its characters (those of the alphabet, and the `=` of the
  padding), with a separator between its lines where it has more than one. (§7 has bodies.)

## `b2t-recons`: base64 and hexadecimal blocks

## 2. The module

- **Module**: name `b2t-recons`, version 1, a part module (spec, "Module execution"): its
  inputs are taken, and the file put out, in order, through windows of 4 pages.
- **Params**: none (empty).
- **Resources**: none.
- **Inputs**: exactly 3:
  0. `text` (class `b2t/text`, a content, kind text): the file with the runs cut out.
  1. `data` (class `b2t/data`, a content): the bytes of all the runs, one after another.
     *(encoder)* In the formula it is a concatenation of slices: one for each run of
     `b2t:apart` bytes or more (16 KiB by default), a content of its own, and one for the
     runs between such runs together.
  2. `layout` (class `b2t/layout`): §3.
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
    (`A-Za-z0-9+/`), 1 the URL-safe (`A-Za-z0-9-_`), 2 lowercase hexadecimal (`0-9a-f`), 3
    uppercase hexadecimal (`0-9A-F`); bits 4 to 7 are reserved and rejected. A hexadecimal run
    is not padded: bit 0 must be 0 with them, or the layout is rejected.

## 4. Decoding

The output is, for each run in order, `gap` bytes of `text`, then the run; at the end, the rest
of `text`. The decoder rejects, with an error, anything of the following not held (and never
reads past an input or writes past `len`): `text`, `data` and `layout` all used up exactly,
the output exactly `len` bytes.

The run of `n` bytes is of `C` characters: base64 with padding `4 ⌈n/3⌉`, without `⌈4n/3⌉`;
hexadecimal `2n`. A base64 run is the base64 of the bytes in the alphabet of the flags, in the
usual way: each three bytes four characters of their 24 bits as four 6-bit values, a last group
of one byte two characters (and `==` where padded), of two bytes three (and `=` where padded),
the unused low bits of the last character zero. A hexadecimal run is two characters for each
byte, the high nibble first, each a digit of the alphabet of the flags.

Where `first` is 0, `line` and `sep` must be 0, and the run's `C` characters follow one
another. Otherwise `first` must be less than `C`, `line` at least 1 and `sep` less than the
count of separators; the first `first` characters, then the rest cut into lines of `line`
characters (the last of what is left, the shorter), a separator before each of them, the
separator `sep`. There is none after the last character. (The padding's `=` are characters of
the last line.)

## 5. *(encoder)* Finding runs

`encode` goes over the file once for each alphabet it has a line of 40 characters of (a quick
look, `starts`, that most files fail: it is asked of every text), and takes a run where (the
rules are base64's; hexadecimal's are set apart below):

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
- **What a run is not.** Fewer than `b2t:min` bytes (`-Z b2t:min`, 192 by default; of
  hexadecimal as well);
  characters without a capital, a lowercase letter or a digit among them; characters that are
  hexadecimal digits alone. A run of the URL-safe alphabet must have a `-` or `_`.
- **Hexadecimal.** A line of hexadecimal is a maximal string of the digits of one case: `0-9a-f`
  or `0-9A-F` (a string of both cases is of neither, and a base64 run is never taken of
  hexadecimal digits alone, so that the two do not meet). There are no signs of padding. Lines
  and separators are found as base64's are. A run of an odd number of characters gives its
  last back to the text (a run of one line that is odd is not taken: the character after its
  end is no delimiter; a last line of that one character is left to the text with its separator). A run needs a digit and
  a letter among its characters (a number or a word is not data). A run of one line must also
  have the prefix of a literal before it: the `x` of `0x` or `0X` (MySQL's `--hex-blob`, C),
  `\x` (PostgreSQL's bytea, in a string or in COPY's text), or `X'` or `x'` (SQLite's `.dump`,
  SQL), and a delimiter after it as base64's. Where runs of two alphabets overlap, the later
  goes, as base64's two do. Where it is found: RTF's `\pict` (Word's lines of 128 characters),
  those dumps, PostScript's `image` data (lines of 64 to 80), `xxd -p` (60). A list of hashes,
  a line each, is taken as wrapped hexadecimal: the bytes are as random as the characters and
  the share rule leaves it where it is a small part of the file.
- **Share.** The runs taken decode to at least `b2t:share` percent of the file (1 by
  default), or none is: the formula holds all of the text again.
- **Separators.** At most 64 distinct, as the bytes are; a run past them is not taken.
- **Check.** The formula is evaluated before it is returned, and it must give the file.

The formula is the application of the module to `text` (a content of kind text), a concatenation
of slices of `data` (a content of kind unknown: each slice is recompressed as the file it is; a
slice for each run of `b2t:apart` bytes or more, and one for the smaller runs between them),
and `layout`.

## 6. Memory

The module's working memory is a constant: its plan is the buffers it takes, whatever the
lengths: 4096 bytes of the layout, 2048 of separators, 24576 of text or data, 32772 of
characters and 65536 of output, each rounded up to 16 bytes on the stack (`aqr_api::stack`).
(A hexadecimal run is taken half a piece at a time, so that its characters fit.)

## `b2t-recons-quopri`: quoted-printable bodies

A quoted-printable body is the text of a MIME body (RFC 2045, section 6.7) of bytes in printable
ASCII, escapes `=XX` and soft line breaks. Encoders differ in where they cut lines and what they
escape, so the module does not store the characters: it writes the body again from the bytes it
decodes to by the rules of a canonical encoder, with the exceptions of an encoder listed in the
layout, as varint deltas. An encoder of one rule has none.

## 7. The module

- **Module**: name `b2t-recons-quopri`, version 1, a part module (spec, "Module execution"):
  its inputs are taken, and the file put out, in order, through windows of 4 pages.
- **Params**: none (empty). **Resources**: none.
- **Inputs**: exactly 3:
  0. `text` (class `b2t-quopri/text`, a content, kind text): the file with the bodies cut out.
  1. `data` (class `b2t-quopri/data`, a content): the bytes of all the bodies, one after
     another. *(encoder)* Of kind text where nearly all the bytes are text (at most 1 in 64 a
     control byte but tab, CR, LF and form feed), and then all one content, so that aqr-wrt
     (of text of 256 KiB or more) takes the bodies of a mailbox together; else a concatenation
     of slices as `b2t-recons`'s (`b2t:quopri-apart`).
  2. `layout` (class `b2t-quopri/layout`): §8.
- **Output**: the file, `len` bytes (the declared length).

## 8. The layout

The layout is, to its end, the bodies, each: `varint gap`, `varint n`, a byte `flags`, `varint
line`, `varint count`, and `count` varints of exceptions:

- `gap`: the bytes of `text` before the body, after the text taken for the one before;
- `n`: the bytes of `data` the body decodes to, at least 1, and not more than are left;
- `flags`: bit 0, the line breaks are LF (else CRLF); bit 1, the escapes' digits are lowercase
  (`=c3`; else uppercase); bit 2, the body ends in a soft break (§9); bit 3, `start` (§9); bits
  4 to 7 are reserved and rejected;
- `line`: the length of a line, with the `=` of a soft break, from 4 to 65536;
- an exception: `index << 1 | kind` as a varint, `index` being the token's number from the
  one before's (from 0 for the first), `kind` 0 for an escape decision flipped, 1 for a soft
  break decision flipped. They are in order of token and kind; two are of one token only as
  an escape's then a soft break's (the delta 0), else the layout is rejected.

## 9. Decoding

The output is, for each body in order, `gap` bytes of `text`, then the body; at the end, the
rest of `text`. The decoder rejects, with an error, anything of the following not held (and
never reads past an input or writes past `len`): `text`, `data` and `layout` all used up
exactly, the output exactly `len` bytes, every exception used on a token.

The body's `n` bytes are cut into **tokens**: a line break is one (CRLF, or the LF where the
flags say LF), any other byte one by itself. For each token in order, with `col` the characters
on the line so far (0 at the body's start), *before end* whether the token is the last of the
body or the next token is a line break:

1. A line break token. It is written as the line break itself, and `col` is 0; where its
   escape decision is flipped, it is written as `=XX` for each of its bytes (`=0D=0A`, or
   `=0A`) and `col` grows by their width (6 or 3).
2. Another byte `b`. It is **escaped** (`=` and two hexadecimal digits of the flags' case,
   width 3) where it is not printable ASCII (33 to 126) but a space or tab, or is `=`, or is a
   space or tab *before end*; else it is written as it is (width 1). A flipped escape decision
   is the other. A byte written as it is must be printable ASCII but `=`, or a space or tab.
3. A **soft break** `=` and a line break is written before the token where `col + w > limit`,
   `w` the width the token is written in (1 where the flag `start` is set), and `limit` `line`
   where *before end* and `line - 1` otherwise (room for the `=`); and `col` is 0 after it. A
   flipped soft break decision is the other.

Where the flags have bit 2, a soft break is written after the last token. A token's number
counts from 0 in each body. (A line break token flipped to escapes is an exception that the
canonical encoder would not write; so is every other decision that differs.)

## 10. *(encoder)* Finding bodies

- **Where.** A body follows a header line `Content-Transfer-Encoding: quoted-printable` (any
  case, whitespace around the colon, `;` or whitespace after) and the blank line that ends
  its header block (lines that are `Name:` or start with whitespace; else it is not a header
  block and nothing is taken). It ends before the line break that precedes the next line of
  `--` and a boundary named anywhere in the file by `boundary="…"` or `boundary=token`, else a
  line break before a line starting with `From ` that has a `:` (mbox), else at the end of the
  file. Not at any `--` line (signatures are `-- `).
- **Valid.** Every `=` is followed by two hexadecimal digits of one case or by a line break
  (not two in a row, in the soft breaks); the other bytes are printable ASCII, spaces, tabs
  and line breaks that are all CRLF or all LF (no lone CR); there is at least one `=`; and
  walking the tokens along the text, each token's text is what the rules of §9 give but for
  the exceptions (a body with an `=0D` before a literal LF is not one).
- **Settings.** The line breaks and the case are the text's. The line length is tried as the
  longest line that ends in a soft break, and 76, 78 (Python's `email` policies), 75, 74, 73,
  72, 70, 68, 64 and 60, with `start` unset and then set (Ruby's `pack("M")` breaks a line
  where a token starts past 72 characters); the one with the fewest exceptions is taken.
- **Exceptions.** More than `b2t:quopri-exceptions` percent (2 by default) of the tokens: not
  taken, as an encoder not worth modeling (it would cost).
- **What a body is not.** Fewer than `b2t:quopri-min` bytes decoded (256 by default).
- **Share.** The bodies taken decode to at least `b2t:quopri-share` percent of the file (1 by
  default), or none is.
- **Check.** The formula is evaluated before it is returned, and it must give the file.

What real encoders need (§9 is made of it): Python's `quopri.encodestring`, `email`'s
`quoprimime.body_encode` and message API (policies `default`, `SMTP` with a line of 78, and
`compat32`), and Perl's MIME::QuotedPrint have no exceptions on text of lines to cut and
escapes (`quopri` writes a lone `.` as `=2E`, and Perl two trailing spaces as `=20=20`: an
exception each); Ruby's `pack("M")` needs `start` and line 74, and none, but where a line ends in a space or tab:
it writes the space as it is and then a soft break before the line break (two exceptions).

## 11. Memory

The module's working memory is a constant: its plan is the buffers it takes, whatever the
lengths: 4096 bytes of the layout, 24580 of text or data and 65536 of output, each rounded up
to 16 bytes on the stack (`aqr_api::stack`).
