# The formula of a disk image

aqr-disk has no module: the formula of a disk image is its bytes cut into pieces, the root a
concatenation of slices of the files' contents and of the rest. It is the same whatever file
system the files are of (aqr-iso's ISO 9660 and UDF, …) and whatever partition it is on. This
document tells what the streams hold, for whoever reads or recompresses the formula.

## Streams

| stream | class | content | what |
|---|---|---|---|
| 0 | `raw` | yes | the contents of the first file of kind `Unknown` (empty where there is none) |
| 1 | `disk/rest` | no | the rest of the image: the partition tables, the file systems' own structures (descriptors, directories, file entries), the padding of blocks, what no file points at |
| 2… | `raw`, `disk/{kind}`, `disk/{format}` | yes | the contents of each other file, a stream each: those of kind `Unknown` first, then those of each other kind, the kinds in the order of their first file |

The rest is its pieces one after another, in the order of the image. A content stream is a
file's contents as they are (its pieces in their order, those of contiguous extents one),
recompressed in turn by whoever takes the formula (aqr-compress, by the kind of the stream).
Where the file is one piece, the root takes all of its stream, a slice. Where it is several, the
root takes each from an application of several outputs ([`aqr_formula::Of::Node`]: a Split
instruction of all of the stream, into its pieces), each in its place in the image; the stream
is still one content, recompressed as a whole.

Where the image is mapped (aqr-compress's input, known to `aqr_formula::store`), a file of one
piece shares its bytes rather than copying them; those of several pieces are copied, put
together. The streams of a class are merged as the archive is written (`Formula::merged`), in
the order of their indices, the class where its first stream is: the archive holds a stream of
each class, of its files in the order of their first pieces in the image, as if the streams were
made so.

## Files stored compressed

A file of WOF's (`Compressed::Wof`) is its data, a content stream as any file's, of its kind by
its name. Its compressed bytes (the chunk table, then the chunks) are the outputs of applications
of aqr-lzx's module `lzx-recons` (LZX) or aqr-xpress's `xpress-recons` (XPRESS4K, 8K, 16K; their
FORMAT.md), each of 32 MiB of data at most, of streams of chunks one after another (of XPRESS, of 8
MiB at most each): the first keeping the chunk table before its chunks (a stream of class
`disk/kept`), its data a slice of the file's stream, its record and index streams of the
module's. Split as any file's bytes
into its pieces where it is in several.

## Pieces *(encoder)*

`aqr_disk::files` reads the volumes of the disk (the image, or the virtual disk of its container:
README) (all of it, then each partition of its table but one at its start) with the file systems
given, the first that starts a volume taking it,
into files and their extents in bytes of the image (of a container, where it holds the blocks of
the disk's extents; blocks it does not hold are no part of any file). `aqr_disk::pieces` lays them out: extents
within the image, sorted by where they start (the longest first, then of the lowest rank), each
taken where it starts at or after the end of the one taken before, and the bytes between them
the rest. A file's pieces are those of its extents taken, in the order of its extents. An image
whose file systems give no extent has no formula.
