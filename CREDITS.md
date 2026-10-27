# Credits

Aqueous is meant for the public domain. No code in it is taken from other projects, except
where a file says so (tables generated from public-domain sources). The projects below are
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
  each length).
- **LZO** (Markus F.X.J. Oberhumer; GPL): the LZO1X stream format, in aqr-lzo, written from the
  Linux kernel's description of it (`Documentation/staging/lzo.rst`, Willy Tarreau and Dave
  Rodgman) and from liblzo2's and lzop's output used as black boxes, without LZO's code; the
  lzop file structure from libarchive's reader (BSD).
- **lpaq** and **PAQ** (Matt Mahoney and others): the style of binary arithmetic coding and
  context mixing.
- **ryg_rans** (Fabian Giesen): rANS coding and division by a reciprocal, in aqr-common and
  elsewhere.

## Data

- **minimp3** (lieff, CC0): aqr-mp3's Huffman tables are generated from `minimp3.h`.
