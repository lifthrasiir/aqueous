# The formula of an image

aqr-iso has no module: the formula of an ISO 9660 or UDF image is its bytes cut into pieces,
the root a concatenation of slices, each of a file's contents or of the rest. This document
tells what the streams hold, for whoever reads or recompresses the formula.

## Streams

| stream | class | content | what |
|---|---|---|---|
| 0 | `raw` | yes | the contents of the files of kind `Unknown` |
| 1 | `iso/rest` | no | the rest of the image: the system area, descriptors, path tables, directories, file entries, the padding of sectors, what no file points at |
| 2… | `iso/{kind}`, `iso/{format}` | yes | the contents of the files of each other kind, in the order of their first file |

Each stream is its pieces one after another, in the order of the image. A content stream's
pieces are files, a slice each (contents of the same file in contiguous extents one slice); its
data is the files' as they are, recompressed in turn by whoever takes the formula
(aqr-compress, by the kind of the stream).

## Pieces *(encoder)*

`aqr_iso::files` reads the directory trees (README "What it reads") into files and their extents
in bytes, each extent up to the file's length. `aqr_iso::pieces` lays them out: extents within
the image, sorted by where they start (the longest first, then UDF's, Joliet's, ISO 9660's),
each taken where it starts at or after the end of the one taken before, and the bytes between
them the rest. An image whose trees give no extent has no formula.
