# aqr-isc formula

aqr-isc has no module of its own. The formula of an InstallShield cabinet is the cabinet's bytes
in order, with the DEFLATE stream of each chunk the output of an application of aqr-flate's
`flate-set` ([its FORMAT.md](../aqr-flate/FORMAT.md)) over the chunk's data and record, and the
rest slices of the formula's own stream. Decoding needs nothing but `flate-set` and the formula's
nodes; this document tells what the formula holds, and is *informative*.

The cabinet's format is in [docs/installshield-cab.md](../../docs/installshield-cab.md); what the
formula reads of it is below.

## The cabinet

A data volume (`data1.cab`, `data2.cab`, ...) starts with a **common header** of 20 bytes: the
signature `ISc(`, a version (4 bytes), 4 bytes of volume information, the offset and the size of the
cab descriptor (4 bytes each; the size is 0 in a data volume beside a header file). The **volume
header** follows; its first field, 4 bytes at offset 20 (the low half of 8 in the versions with
64-bit fields), is where the file data starts. A header file (`data1.hdr`) has no volume header:
the same 4 bytes are its size.

The file data is the files of the volume one after another, from that offset. A compressed file is
a sequence of **chunks**: the length `n` of the chunk (2 bytes, little-endian, not 0), then `n` bytes,
a complete raw DEFLATE stream (RFC 1951) that decodes to at most 32768 bytes (a file of more is
cut in chunks of 32768, the last of what is left). Every chunk is a stream of its own, with no
window carried over from the one before.

## What is taken

A cabinet is taken where it starts with the signature, its data starts at least 60 bytes in
(after the headers) and within it, and a first chunk of a non-zero length lies within the cabinet
and begins with a DEFLATE block of a type that exists (not 3). Chunks are read one after another
from where the data starts, to the first whose length is 0 or runs past the cabinet's end. What
follows is kept, as is any chunk that does not rebuild.

A chunk is **rebuilt** when aqr-flate deconstructs it (the profile and zlib's emulation as the
caller chooses; at most 65536 bytes of data), and its record is shorter than the chunk. The
chunks within the limit of the data (the caller's, by 32768 bytes a chunk) are tried.

## The formula

Streams:

| stream | class | content |
|---|---|---|
| 0 | `isc` | the bytes kept: the headers, the chunks' lengths, the chunks not rebuilt, the end |
| 1 | `flate/record` (kind Compressed) | the records of the chunks rebuilt, one after another |
| 2 | `isc/small` | the data of the files too small (under 4096 bytes) to be contents of their own |
| 3.. | `isc` (content) | the data of each other file, one stream a file (unknown kind), scanned in turn |
| | `flate/index` | the lengths (data, record, chunk) of each chunk for the application |

A **file** is the run of the data of chunks rebuilt, each of 32768 bytes of data but the last: a chunk of
less ends it, so does one that is not rebuilt (and the end). So a file whose size is a multiple of
32768 runs into the next, which is one content with it.

The root is the concatenation, in order, of a slice of stream 0 (the bytes before a chunk's
stream: the previous chunk's end to this one's start, its length among them) and the output of
the chunk, for each chunk rebuilt, then a slice of stream 0 to the end. The outputs are those of
one application of `flate-set` (of the profile) split into the chunks' lengths; a single chunk is
the application itself. Its inputs are the data (the nodes of the chunks' data, slices of the
contents), the records and the index.
