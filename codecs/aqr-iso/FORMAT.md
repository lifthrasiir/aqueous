# The formula of an image

aqr-iso has no module: the formula of an ISO 9660 or UDF image is its bytes cut into pieces,
the root a concatenation of slices, each of a file's contents or of the rest. This document
tells what the streams hold, for whoever reads or recompresses the formula.

## Streams

| stream | class | content | what |
|---|---|---|---|
| 0 | `raw` | yes | the contents of the first file of kind `Unknown` (empty where there is none) |
| 1 | `iso/rest` | no | the rest of the image: the system area, descriptors, path tables, directories, file entries, the padding of sectors, what no file points at |
| 2… | `raw`, `iso/{kind}`, `iso/{format}` | yes | the contents of each other file, a stream each: those of kind `Unknown` first, then those of each other kind, the kinds in the order of their first file |

The rest is its pieces one after another, in the order of the image. A content stream is a
file's contents (contents of the same file in contiguous extents one stream), a slice of all of
it in the root, as they are: recompressed in turn by whoever takes the formula (aqr-compress, by
the kind of the stream). Where the image is mapped (aqr-compress's input, known to
`aqr_formula::store`), the streams share its bytes rather than copy them. The streams of a class
are merged as the archive is written (`Formula::merged`), in the order of their indices, the
class where its first stream is: the archive holds a stream of each class, of its files in the
order of the image, as if the streams were made so.

## Pieces *(encoder)*

`aqr_iso::files` reads the directory trees (README "What it reads") into files and their extents
in bytes, each extent up to the file's length. `aqr_iso::pieces` lays them out: extents within
the image, sorted by where they start (the longest first, then UDF's, Joliet's, ISO 9660's),
each taken where it starts at or after the end of the one taken before, and the bytes between
them the rest. An image whose trees give no extent has no formula.
