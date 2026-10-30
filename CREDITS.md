# Credits

Aqueous is dedicated to the public domain (MIT-0 where that is not possible, see
`LICENSE.txt`). No code in it is taken from other projects, except
where a file says so (tables generated from public-domain sources, and public-domain code
listed under Code below). The projects below are
where its ideas, or the behaviour it reproduces, come from.

## Encoders whose output is reproduced

Some recompressors reproduce an encoder's output bit for bit, so that a stream it made costs
little more than its parameters. Each such crate's `FORMAT.md` describes the behaviour in its
own terms; the code is written from that description, not from the encoder's code.

- **zlib** (Jean-loup Gailly, Mark Adler): its deflate parsers and code lengths, in aqr-flate.
- **zopfli** (Google) and **ECT** (Felix Hanau): their length-limited code lengths,
  smoothing of counts and run-length coding of code lengths, in aqr-flate.
- **Brotli** (Google): its smoothing of counts, as ECT uses it, in aqr-flate.
- **libdeflate** (Eric Biggers): its run-length coding of code lengths, in aqr-flate.
- **LZ4** (Yann Collet): its block compressor, in aqr-lz4.
- **Snappy** (Google): its compressor, in aqr-snappy.
- **libjpeg** (the Independent JPEG Group): its progressive Huffman encoder, in aqr-jpeg.

## Ideas

- **preflate** (Dirk Steinke): estimating zlib's parameters from a stream, in aqr-flate's
  detection.
- **Lepton** (Dropbox; Daniel Reiter Horn and others) and Ken Lakhani's predictors of DCT
  coefficients, and **packJPG** / **brunsli**: modelling JPEG coefficients, in aqr-jpeg.
- **packMP3** (Matthias Stirner): modelling MP3's Huffman-coded data, in aqr-mp3.
- **XWRT** (Przemysław Skibiński): word replacement for text, in aqr-wrt.
- **LZMA** (Igor Pavlov; the LZMA SDK's specification, public domain): the format, in aqr-lzma,
  whose records predict a parse by the matches a binary-tree match finder offers (the nearest of
  each length); LZMA2's chunks, in aqr-lzma; the 7z format (the LZMA SDK's `7zFormat.txt`,
  public domain), in aqr-7z, checked against 7-Zip's archives used as black boxes.
- **XZ Utils** (Lasse Collin and others): the `.xz` format (`xz-file-format.txt`, public
  domain), in aqr-lzma; its filters used as black boxes for aqr-bcj's tests.
- **LZO** (Markus F.X.J. Oberhumer; GPL): the LZO1X stream format, in aqr-lzo, written from the
  Linux kernel's description of it (`Documentation/staging/lzo.rst`, Willy Tarreau and Dave
  Rodgman) and from liblzo2's and lzop's output used as black boxes, without LZO's code; the
  lzop file structure from libarchive's reader (BSD).
- **LZFSE** and **LZVN** (Apple; BSD): the stream formats, the
  LZFSE compressor's normalization of frequencies and its table of match candidates (which
  aqr-bvx's record takes as a context), in aqr-bvx, written from the output of Apple's
  libcompression used as a black box, without the code of Apple's implementation.
- **lpaq** and **PAQ** (Matt Mahoney and others): the style of binary arithmetic coding and
  context mixing.
- **FITS** (the IAU FITS Working Group's FITS Standard 4.0) and its tiled image compression
  (William Pence, Richard White, Rob Seaman and others): the format, in aqr-fits, written from
  the Standard; the Rice tiles' layout checked against fpack and funpack (cfitsio, NASA) used
  as black boxes, whose code is not used. The encoders' choice of a block's code is not
  reproduced but kept.
- **LOCO-I** (Marcelo Weinberger, Gadiel Seroussi, Guillermo Sapiro): the median predictor and
  the bias correction by context, in aqr-fits's pixel model.
- **ryg_rans** (Fabian Giesen): rANS coding and division by a reciprocal, in aqr-common and
  elsewhere.

## Code

- **LZMA SDK** (Igor Pavlov, public domain): aqr-bcj's filters (the branch converters, Delta,
  BCJ2's decoder) are ported from its `Bra.c`, `Bra86.c`, `BraIA64.c`, `Delta.c` and `Bcj2.c`.

## Data

- **minimp3** (lieff, CC0): aqr-mp3's Huffman tables are generated from `minimp3.h`.
