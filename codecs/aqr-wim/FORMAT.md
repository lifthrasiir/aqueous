# aqr-wim formula

aqr-wim has no module of its own. The formula of a WIM file is the file's bytes in order, as
applications of aqr-lzx's `lzx-recons` ([its FORMAT.md](../aqr-lzx/FORMAT.md), the WIM variant)
and slices of its streams. Decoding needs nothing but `lzx-recons` and the formula's nodes; this
document tells what the formula holds, and is *informative*.

External reference: Microsoft's description of the Windows Imaging File Format (WIM).

## The file

A WIM file is a header of 208 bytes, then resources anywhere, each found by a **resource
header** of 24 bytes: its size in the file (7 bytes, little-endian), flags (1 byte: 0x2
metadata, 0x4 compressed, 0x8 spanned), its offset (le64) and the size of its data (le64).

- The header starts with `MSWIM\0\0\0` and its size, `le32` 208, at byte 8; its flags are at
  16 (0x2 compressed; 0x20000 XPRESS, 0x40000 LZX, 0x80000 LZMS), its chunk size at 20, and the
  lookup table's resource header at 48.
- The **lookup table** is entries of 50 bytes: a resource header, a part number, a reference
  count and the SHA-1 of the data. It lists the resources of files and of images' metadata.
- A compressed resource is a table of where its chunks start (all but the first, from the
  table's end; 4 bytes each, 8 where the data is over 4 GiB), then the chunks, each 32768 bytes
  of data but the last, compressed on its own: a chunk as long as its data is stored.

A file is taken where its header is as above, it is compressed by LZX alone with chunks of
32768 bytes (or not compressed), and its lookup table is not compressed and lies within the
file. Its resources (the lookup table's, spanned ones left out) are taken in order of their
offset (the longer first at the same offset), each where it is not empty, lies within the file,
starts after the header and after the last resource taken, and:

- is compressed (in a compressed file), its chunk table within it, its chunks one after
  another to its end, each no longer than its data; or
- is not compressed, its size in the file that of its data.

## The formula

Streams:

| stream | class | content |
|---|---|---|
| 0 | `raw` (a content) | the data of the resources taken apart, in the file's order |
| 1 | `wim/rest` | the bytes kept, in the file's order |
| 2, 3, … | `lzx/record`, `lzx/index` | of each application of `lzx-recons`, its records and index |

A compressed resource is cut into streams of `lzx-recons` of at most 1024 chunks (32 MiB of
data); it is taken apart where all of them split (aqr-lzx's encoder choices), else kept with
the bytes around it. Its data in `raw` is the resource's data, e8 translation undone (as the
file's own data).

The root is the concatenation, in the file's order, of:

- **applications** of `lzx-recons` (no parameters): each takes streams of consecutive compressed
  resources, of at most 32 MiB of data in all (but for a stream alone). Its inputs are the
  streams' data (a slice of `raw`), its records, the bytes kept (slices of `wim/rest`: before
  each stream, the header, chunk tables and resources not taken since the stream before; after
  the last, nothing, or the bytes up to a resource of no compression or the end of the file),
  and its index. Its output is that part of the file.
- **slices** of `raw`: a resource of no compression taken, whole (a WIM file stored in it is a
  content of its own, taken apart in turn);
- **slices** of `wim/rest`: bytes kept that no application holds (before a first resource of
  no compression, or after the last).

Each stream's data (and each resource of no compression) is a slice of `raw`, which
aqr-compress recompresses as a content of its own; the files within it are not told apart (the
images' metadata is not read). The data compresses better so than the chunks, compressed 32 KiB
at a time, so aqr-compress takes a WIM file whatever the formula's size.
