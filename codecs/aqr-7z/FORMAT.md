# aqr-7z formula

aqr-7z has no module of its own. The formula of a 7z archive concatenates, in the order of the
archive, the bytes it keeps as they are and the packed streams it rebuilds by aqr-lzma's
modules, specified in [aqr-lzma's FORMAT.md](../aqr-lzma/FORMAT.md), over aqr-bcj's
([its FORMAT.md](../aqr-bcj/FORMAT.md)) for the filters. This document tells which streams those
are, and what their inputs hold.

External reference: the 7z format as Igor Pavlov's LZMA SDK describes it (`7zFormat.txt`,
public domain): the signature header, the header's properties, folders, coders and substreams.

## The archive

A 7z archive is a signature header of 32 bytes, the packed streams, and the header at the
offset and of the size the signature header tells (with its CRC-32). The header is plain
(property `kHeader`), or encoded (`kEncodedHeader`): streams info of one folder, whose output is
the plain header.

aqr-7z takes an archive where its signature header and header are valid (their CRC-32s right)
and parse, the files with data are as many as the substreams, and an encoded header's folder
is one it decodes (below).

## The streams rebuilt

A folder is a graph of coders: each input of a coder is a packed stream or (by a bind pair)
another coder's output, and the output no bind pair takes is the folder's. A folder of at most
2^31 bytes of data is rebuilt where it is one of these shapes:

- **A chain**: filters, each of one input and one output, from the folder's output down to a
  *compressor* coder (below) reading a packed stream. A filter is a branch converter (BCJ
  `03 03 01 03`, PPC `03 03 02 05`, IA64 `03 03 04 01`, ARM `03 03 05 01`, ARMT `03 03 07 01`,
  SPARC `03 03 08 05`, ARM64 `0A`, RISCV `0B`; properties none or the start address, le32) or
  Delta (`03`; properties the distance less one). The compressor's input 0 is the folder's
  data under aqr-bcj's modules of the filters, the innermost (the folder's output's) applied
  first.
- **BCJ2** (`03 03 01 1B`, four inputs, one output) as the folder's output: its inputs 0 to 2
  (main, call, jump) compressor coders reading packed streams, its input 3 (the range coder's)
  a packed stream. One application of `bcj2` takes the folder's data and that packed stream
  (a stream of the formula, of class `bcj2/rc`, which is also the packed stream in place) and
  is split into the three streams, each the input 0 of its compressor.

The compressors are:

- **LZMA** (`03 01 01`; properties: the properties byte and the dictionary size, le32): one
  application of `lzma-recons` of a single stream (the index empty), the stream ending at the
  folder's size. Its record's header holds the properties and the dictionary size.
- **LZMA2** (`21`; properties: the dictionary byte): one application of `lzma2-recons`, a
  segment for each dictionary reset.
- **Copy** (`00`): no application: the packed stream is the data itself.

Encoding the data must give back every packed stream exactly, and the folders' data is at most
2^31 bytes in all; other folders are kept. Where the filters' encoding does not give back what
the compressors decode to, the compressors' streams are rebuilt from that alone (their data not
the files', no filter applied), and stored ones are kept. The packed stream of an encoded header's
folder, of LZMA or LZMA2, is rebuilt likewise, its data the plain header.

## The data

A folder's data is cut into its substreams, the contents of the files that take them (the files
with data, in order). Each file's contents are sliced into a stream of the kind its name tells
(the archive's application's choice; by default all unknown): a file of at least 1 KiB is a
slice of its own, those shorter are slices joined with their neighbours of the kind. The input 0
of the folder's compressor (under the filters) or of `bcj2` is the concatenation of those slices
in order; for Copy alone, the slices themselves are in place of the packed stream, of streams of
contents.

## Encoder choices (informative)

- Folders are split side by side on the threads given (a folder of LZMA2 alone: its segments).
- The kept bytes are one stream, in order: the signature header, the packed streams not
  rebuilt, the header and anything after it.
