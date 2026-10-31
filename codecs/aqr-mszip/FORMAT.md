# aqr-mszip formula and module

The formula of a CAB file concatenates, in the order of the file, the bytes it keeps as they are
and the spans of the folders it rebuilds, which are the outputs of one application of
`mszip-recons` (split by the spans' sizes where there are several). That application takes the
DEFLATE streams of the MSZIP folders from an application of aqr-flate's `flate-set`
([its FORMAT.md](../aqr-flate/FORMAT.md)), and the LZX folders' streams from applications of
aqr-lzx's `lzx-recons`. The formula of a CHM file is its LZX section's content rebuilt by
`lzx-recons` between the bytes it keeps (below).

External references: MS-CAB (the Cabinet file format) and MS-MCI (the MSZIP format), Microsoft's
Open Specifications; LZX as MS-PATCH describes it; CHM's container as public descriptions of it
tell (the ITSF header, the ITSP directory of PMGL chunks, the LZXC control data and reset table).

## The file

A CAB file is a header (`CFHEADER`), its folders (`CFFOLDER`), its files (`CFFILE`) and each
folder's data blocks (`CFDATA`: a checksum, the bytes of the block and of its data, a reserved
field of the size the header tells, then the block). A folder's *span* is its data blocks, one
after another. A folder is rebuilt where:

- its blocks follow each other, each with some data, and it does not go on in another cabinet;
- it is **stored** (compression type 0), each block its data; or
- it is **MSZIP** (type 1), each block `CK` and a DEFLATE stream (RFC 1951) whose last block is
  final, padded with zeros to a byte and ending with the block; a stored DEFLATE block padded
  with zeros too. The streams may refer to the data of the blocks before (up to 32 KiB back); or
- it is **LZX** (type 3, the window's bits in bits 8-12 of `typeCompress`, 15 to 21), its blocks'
  bytes one after another an LZX stream that aqr-lzx splits into its data and record (the CAB
  variant: the e8 header, blocks of 24-bit sizes over frames of 32 KiB, the stream aligned to a
  word after each frame, e8 translation frame by frame).

The MSZIP and LZX folders' data is at most what the archive's budget allows, in all; other
folders are kept.

## Joined streams

The *joined stream* of an MSZIP folder is the DEFLATE blocks of all its blocks' streams, one
after another, the final flag cleared but on the very last one, and the bits of each stored
block's padding as many as its place in the joined stream takes (zero). It ends padded with
zeros to a byte. It is a DEFLATE stream whose data is the folder's.

## `mszip-recons`

No resources and no parameters. Inputs:

0. the joined streams of the MSZIP folders, one after another;
1. the raw blocks' bytes, one after another: a stored folder's data, an LZX folder's stream (the
   output of `lzx-recons`, of parameters the window's bits, with no bytes kept);
2. the bytes kept: for each block, the bytes before it (its `CFDATA` header), then the bytes
   after the last;
3. the index: for each block, two varints: the bytes kept before it, then `2n` for an MSZIP
   block of `n` DEFLATE blocks or `2n + 1` for a raw block of `n` bytes.

The output is, for each block of the index in order, the bytes kept before it, then:

- an MSZIP block: `CK`, then the next `n` DEFLATE blocks of input 0 (`n` at least 1), the last
  final, the others not, each stored block padded with zeros to a byte, the block padded with
  zeros to a byte. Where the last of those is final in input 0 (the end of a joined stream),
  input 0 goes on from its next byte (the padding skipped must be zero).
- a raw block (a stored or LZX folder's data block): the next `n` bytes of input 1.

The bytes kept after the last block follow. Inputs 0 and 1 must be used up exactly, and the
output be the application's size.

## The data

An MSZIP folder's data is input 0 of `flate-set`, given its record; one application takes all
the MSZIP folders, its index cutting them. An LZX folder's data (e8 undone) is a stream's data
of `lzx-recons`; one application takes the LZX folders of a window, split into their streams
where there are several. The data of each folder is cut into the contents of its files
(`uoffFolderStart`, `cbFile`; those of `iFolder` 0xFFFD in the first folder and 0xFFFE, 0xFFFF in
the last), in order of their offsets, gaps between them unknown; where they overlap or run past
the data, it is one piece. Each file's contents are
sliced into a stream of the kind its name tells, those of at least 1 KiB a slice of their own.

## CHM files

A CHM file (ITSF version 3) is taken where its LZX section (section 1) has control data of
version 2 (`LZXC`: the reset interval and the window in frames of 32 KiB, a window of 2^15 to
2^21 bytes) and a reset table of version 2 telling where each frame of 32 KiB starts in the
content, the data's length and the content's (at most the entry `Content`'s; the bytes after it
are kept). Each reset interval is an LZX stream of its own (the CAB variant, its frames' starts
those the table tells), the last one of whole frames: the section's data, then zeros. The
budget bounds the section's data.

The formula is the bytes before the content, one application of `lzx-recons` (parameters the
window's bits; a stream for each interval, no bytes kept), then the bytes after the content. The
applications' data is the section's data and the zeros after it, cut into the contents of the
section's files (by their directory entries, in order of their offsets, gaps unknown; where they
overlap or run past the data, one piece), each sliced into a stream of the kind its name tells.

