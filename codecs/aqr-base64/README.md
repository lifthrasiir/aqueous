# aqr-base64

Base64 blocks in text files (the attachments of MIME messages, mbox and EML, PEM, data URIs in
HTML, SVG and CSS, base64 in JSON as Jupyter notebooks and HAR files have it, XML, LDIF) cut
out of the text and decoded, so that the bytes are recompressed as the files they are (a JPEG
by aqr-jpeg, a PNG by aqr-png, a PDF, HTML as text), and the text without them as text. The
module `base64-recons` writes the file again bit for bit: the text, the decoded bytes of all
the blocks, and a layout (where each block lies, the length of its lines, the separator
between them, padding, alphabet).

`FORMAT.md` specifies the module and tells how the encoder finds the blocks. aqr-compress
(`Format::Base64`) tries it on every text where a line of 40 alphabet characters is found; the
formula is kept where it is smaller than the text compressed as it is.

## Results

Synthetic files made of local corpora (codec-corpus's photographs and PNGs, aqr-pdf's PDFs,
the system's `/etc/ssl/cert.pem`), `aqr -c` on an M4, the same binary with the blocks taken
and not (`-Z base64:min=1024G`); instructions retired of compression, and of `aqr -K`:

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

## Status

Left: hexadecimal (RTF's `\pict`, SQL dumps' `0x…`: another alphabet, its flags reserved),
quoted-printable, uuencode and yEnc; base64 with `\/` (PHP's JSON); a block whose first line is
shorter than 40 characters (LDIF's attributes of long names).
