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
- **bzip2** (Julian Seward; BSD-style): its compressor (where blocks are cut, the Huffman
  tables, their selectors and code lengths), in aqr-bz2, written from memory of its behaviour
  and checked against libbzip2 1.0.8 (Python's `bz2`) used as a black box, without its code.
- **libjpeg** (the Independent JPEG Group): its progressive Huffman encoder, in aqr-jpeg.
- **RAR** (Alexander Roshal, RARLAB): its compressors' block cuts, code lengths, filter records
  and PPMd escape bytes, and the RAR5 and RAR 2.9 stream formats, in aqr-rar. Written in a clean
  room: the formats were described in `docs/rar5-decoding.md` and `docs/rar3-decoding.md` by an
  agent that read libarchive's RAR readers (BSD) and RARLAB's technotes and wrote no code, and
  the code was written from those notes by others; rar 7.23 and 6.24 and bsdtar were used as
  black boxes. unrar's code (whose licence forbids using it towards a RAR compressor) was read by
  no one.

## Ideas

- **ZPAQ** (Matt Mahoney; the ZPAQ open standard format): an archive carrying the programs
  that decode it (ZPAQL), so that a decompressor needs no knowledge of the methods that made
  it, in Aqueous's formulas and modules; an archive extended by appending, in its segments.
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
- **ALZ** (ESTsoft's ALZip): the archive format and ALZip's framing of bzip2, in aqr-alz and
  aqr-bz2 (`bz2-alz-recons`), written from OctopusET's ALZ Archive Format Spec (CC0) without
  any ALZ decoder's code; checked on alkegi/unalz's test archives (CC0) and an archive of
  ALZip 11.25 (openwall's john-samples).
- **EGG** (ESTsoft's ALZip): the archive format and its compressor AZO, in aqr-egg and aqr-azo,
  written from OctopusET's EGG Archive Format Specification (CC0) without any EGG or AZO
  decoder's code; checked on the test archives of akolman/EggDotNet (MIT, some of them aqr-egg's
  fixtures), seb3773/alzgg (ALZip's output, made under Wine) and blurfx/unegg.
- **LHA** (Haruyasu Yoshizaki's LHA, after Haruhiko Okumura's ar002): the archive format and
  the streams of `-lh4-` to `-lh7-`, in aqr-lha, written from the format as LHA's decoders read
  it, without any LHA code; the compressors' Huffman coder (its lengths and tables) and their
  block cuts from their output used as black boxes: the test archives of fragglet/lhasa (ISC,
  some of them aqr-lha's fixtures) and archives of Aminet. Okumura's LZSS binary search tree
  (public domain) is one of the record's match finders.
- **GIF** (CompuServe's GIF89a specification): the container and its LZW, in aqr-gif, whose
  tests take the output of ImageMagick, ffmpeg, Pillow and macOS's ImageIO used as black boxes.
- **lpaq** and **PAQ** (Matt Mahoney and others): the style of binary arithmetic coding and
  context mixing (aqr-gif's model of palette indices: hashed contexts in buckets of a nibble,
  mixers of stretched probabilities, an APM).
- **FITS** (the IAU FITS Working Group's FITS Standard 4.0) and its tiled image compression
  (William Pence, Richard White, Rob Seaman and others): the format, in aqr-fits, written from
  the Standard; the Rice tiles' layout checked against fpack and funpack (cfitsio, NASA) used
  as black boxes, whose code is not used. The encoders' choice of a block's code is not
  reproduced but kept.
- **LOCO-I** (Marcelo Weinberger, Gadiel Seroussi, Guillermo Sapiro): the median predictor and
  the bias correction by context, in aqr-fits's pixel model.
- **ryg_rans** (Fabian Giesen): rANS coding and division by a reciprocal, in aqr-common and
  elsewhere.
- **age** (Filippo Valsorda; the age v1 format, C2SP): Aqueous's encrypted archives, in
  aqr-formula (`aqr/envelope.rs`), written from the specification and checked against rage
  (pyrage) used as a black box (`scripts/age-interop.py`).
- **Archive encryption**: ZIP's traditional encryption (PKWARE's APPNOTE §6.1), WinZip's AES
  (its AE-1 and AE-2 description) and 7-Zip's AES-256 coder, decrypted in aqr-unpack
  (`crypt.rs`), written from the formats' descriptions and checked against Info-ZIP's zip and
  7-Zip's 7zz used as black boxes. The ciphers, hashes and key derivations are the RustCrypto
  crates'.

## Code

- **LZMA SDK** (Igor Pavlov, public domain): aqr-bcj's filters (the branch converters, Delta,
  BCJ2's decoder) are ported from its `Bra.c`, `Bra86.c`, `BraIA64.c`, `Delta.c` and `Bcj2.c`.
- **PPMd var.H** (Dmitry Shkarin, public domain), through the **LZMA SDK**'s `Ppmd7.c`,
  `Ppmd7Dec.c` and `Ppmd7Enc.c` (Igor Pavlov, public domain): aqr-rar's PPMd model and range
  coder (`ppmd.rs`) are ported from them.

## Data

- **minimp3** (lieff, CC0): aqr-mp3's Huffman tables are generated from `minimp3.h`.
