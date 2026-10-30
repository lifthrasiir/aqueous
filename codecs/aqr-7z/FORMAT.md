# aqr-7z formula

aqr-7z has no module of its own. The formula of a 7z archive concatenates, in the order of the
archive, the bytes it keeps as they are and the packed streams it rebuilds by aqr-lzma's
modules, specified in [aqr-lzma's FORMAT.md](../aqr-lzma/FORMAT.md). This document tells which
streams those are, and what their inputs hold.

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

A folder of one coder, one packed stream and of at most 2^31 bytes of data is rebuilt where its
coder is:

- **LZMA** (`03 01 01`; properties: the properties byte and the dictionary size, le32): one
  application of `lzma-recons` of a single stream (the index empty), the stream ending at the
  folder's size. Its record's header holds the properties and the dictionary size.
- **LZMA2** (`21`; properties: the dictionary byte): one application of `lzma2-recons`, a
  segment for each dictionary reset.
- **Copy** (`00`): no application: the packed stream is the data itself.

In either case encoding the data must give back the packed stream exactly, and the folders' data
is at most 2^31 bytes in all; other folders are kept. The packed stream of an encoded header's
folder, of LZMA or LZMA2, is rebuilt likewise, its data the plain header.

## The data

A folder's data is cut into its substreams, the contents of the files that take them (the files
with data, in order). Each file's contents are sliced into a stream of the kind its name tells
(the archive's application's choice; by default all unknown): a file of at least 1 KiB is a
slice of its own, those shorter are slices joined with their neighbours of the kind. The input 0
of the folder's application is the concatenation of those slices in order; for Copy, the
slices themselves are in place of the packed stream, of streams of contents.

## Encoder choices (informative)

- Folders are split side by side on the threads given (a folder of LZMA2 alone: its segments).
- The kept bytes are one stream, in order: the signature header, the packed streams not
  rebuilt, the header and anything after it.
