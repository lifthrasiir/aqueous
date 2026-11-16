# aqr-b2t

Binary-to-text encodings in text files, cut out of the text and decoded, so that the bytes are
recompressed as the files they are (a JPEG by aqr-jpeg, a PNG by aqr-png, a PDF, HTML as text)
and the text without them as text. Two modules write the file again bit for bit:

- `b2t-recons`: base64 blocks (the attachments of MIME messages, mbox and EML, PEM, data URIs in
  HTML, SVG and CSS, base64 in JSON as Jupyter notebooks and HAR files have it, XML, LDIF) and
  hexadecimal blocks (RTF's `\pict`, `--hex-blob` and bytea dumps of SQL, SQLite's `X'..'`,
  PostScript `image` data, `xxd -p`, Type 1 fonts' `eexec`), of the text, the decoded bytes and a
  layout (where each block lies, the length of its lines, the separator between them, padding,
  alphabet).
- `b2t-recons-quopri`: quoted-printable bodies of MIME messages (mbox, EML, MHTML), written again
  by a canonical encoder from the decoded bytes, the exceptions of the real encoder in the
  layout (none for Python's `quopri` and `email`, Perl's MIME::QuotedPrint, and Ruby's
  `pack("M")` on text of lines to cut and escapes).

`FORMAT.md` specifies the modules and tells how the encoders find the blocks. aqr-compress
(`Format::B2t`, `Format::Quopri`) tries them on every text (a quick look first: a line of 40
alphabet characters, a `quoted-printable` header), the text left by one looked at by the other
a level deeper; the formula is kept where it is smaller than the text compressed as it is.
Developer options: `-Z b2t:min`, `b2t:hex`, `b2t:share`, `b2t:apart`, `b2t:quopri-min`,
`b2t:quopri-share`, `b2t:quopri-apart`, `b2t:quopri-exceptions`.

## Results

Synthetic files made of local corpora (codec-corpus's photographs and PNGs, aqr-pdf's PDFs,
the system's `/etc/ssl/cert.pem`), `aqr -c` on an M4, the same binary with the blocks taken
and not (`-Z b2t:min=1024G`); instructions retired of compression, and of `aqr -K`:

| file | size | without | with | compression | test |
|---|---|---|---|---|---|
| mbox of 20 messages (JPEG, PNG, PDF attachments, 76 columns CRLF) | 5,982,017 | 4,428,293 | 3,830,783 (-13.5%) | 3.29 -> 11.39 G | 1.01 -> 1.55 G |
| PEM bundle (`/etc/ssl/cert.pem`) | 333,483 | 142,218 | 131,878 (-7.3%) | 1.90 -> 1.66 G | 0.95 -> 0.95 G |
| HTML with 8 data URIs | 2,693,307 | 2,014,742 | 1,704,420 (-15.4%) | 1.50 -> 3.19 G | 0.98 -> 1.36 G |
| SVG with 4 data URIs | 1,615,792 | 1,212,798 | 1,138,128 (-6.2%) | 1.40 -> 2.75 G | 0.96 -> 1.09 G |
| ipynb of 8 base64 PNG outputs (76 columns, `\n` escaped) | 3,660,356 | 2,759,305 | 2,242,897 (-18.7%) | 2.09 -> 3.87 G | 0.99 -> 1.46 G |

The time is that of recompressing what the blocks hold (JPEG, PNG, PDF).

On the 10 GB corpus it takes 36 files (PEM files, mail in Python's tests, RFCs, OpenOffice's
templates, Firefox's `omni.ja`): with the zstd levels picked alike (`-Z zstd:higher=9`), the
archive is 314 KB smaller (2,443,380,365 -> 2,443,066,049 B). At the default levels it is
550 KB larger, as a block of the mingw 7z, 44 KB shorter, gets level 6 rather than 9 (the
sample's sizes tie in the picker's cost of size and time): not of the blocks.

Hexadecimal and quoted-printable (synthetic files from the same corpora, `aqr -c` on an M4, the
same binary with `-Z b2t:min=1024G -Z b2t:quopri-min=1024G` and without; compression and
`aqr -K` instructions retired):

| file | size | without | with | compression | test |
|---|---|---|---|---|---|
| RTF, 6 `\pict` of 128-column lowercase hex | 3,134,493 | 1,662,408 | 1,333,362 (-19.8%) | 4.31 -> 2.70 G | 0.07 -> 0.36 G |
| mysqldump `--hex-blob`, 10 images, one uppercase line | 5,252,914 | 2,627,717 | 2,196,293 (-16.4%) | 2.17 -> 3.45 G | 0.07 -> 0.54 G |
| pg_dump COPY, `\\x` bytea | 5,252,763 | 2,627,667 | 2,196,207 (-16.4%) | 2.11 -> 3.41 G | 0.07 -> 0.54 G |
| SQLite `.dump`, `X'..'` | 5,252,969 | 2,627,901 | 2,196,225 (-16.4%) | 2.17 -> 3.46 G | 0.07 -> 0.54 G |
| mbox, 20 messages: Python `email` quoted-printable HTML, JPEG and PNG attachments | 5,009,462 | 3,514,225 | 2,920,553 (-16.9%; quoted-printable alone -8 KB) | 3.98 -> 5.14 G | 0.09 -> 0.67 G |
| mbox of 40 quoted-printable HTML messages | 794,903 | 86,938 | 76,666 (-11.8%) | 1.32 -> 1.24 G | 0.06 -> 0.12 G |

On the 10 GB corpus (`-Z zstd:higher=9 -Z zstd:higher-speeds=100e6`): 2,443,380,365 off, 2,443,069,771
with base64 and quoted-printable only, 2,443,297,333 with hexadecimal too (hexadecimal +227 KB
there, though the subtrees that take it, run alone, are smaller with it: cygwin -290 KB,
progs -13 KB, cygwin with each other top directory -29 to -156 KB, Ghostscript's fonts -49 KB).
The sum is the layout's: by the stats' files, the directories whose formulas did not change at
all moved by as much (hg19 -161 KB, cygwin's perl5 +174 KB, progs +128 KB). Type 1 fonts (`.pfa`), Tcl's encodings (`.enc`: 16-bit tables in hex),
PostScript's images are what it takes.

## Status

Left: uuencode and yEnc; base64 with `\/` (PHP's JSON); a block whose first line is shorter than 40
characters (LDIF's attributes of long names); hexadecimal in groups (`<0123 4567>`), Intel HEX;
quoted-printable of a header's encoded words (RFC 2047), encoders that escape every space
(Python's `quotetabs`) or write trailing whitespace otherwise than the canonical (Perl's `=20=20`,
Ruby's soft break after a space: an exception each), Gmail's, Outlook's and Thunderbird's own
(none seen in the corpora).
