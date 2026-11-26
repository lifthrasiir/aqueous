# The formula of a PPMd file

aqr-ppmd has no module: the formula of a PPMd var. J file is its bytes cut in two, the root a
concatenation of two slices. This document tells what the streams hold, for whoever reads or
recompresses the formula.

## Streams

| stream | class | content | what |
|---|---|---|---|
| 0 | `ppmd/header` | no | the file's header: 16 bytes and the name ([below](#the-header)) |
| 1 | `ppmd/data` | no | the rest of the file, PPMd's coded data as it is |

The root is `Concat(Slice(0, 0, header), Slice(1, 0, data))`. A file with nothing after its
header has no formula.

## The header

As the files seen tell it: the signature 0x84ACAF8F, the file's attributes (4 bytes), the model (2
bytes: the order less 1 in 4 bits, the memory in MB less 1 in 8, the variant from `A` in 4), the length
of the name and the restore method (2 bytes: 14 bits and 2), the file's time (4 bytes), then the name.
A file is taken where it starts with a header of var. J (order 2 or more, restore method 0 to 2)
followed by data. Only the first header is cut off.

## Design notes

- The data is not decoded or coded again. A decoder and an encoder of var. J would each run at a few
  MB/s (7-Zip's Ppmd8 of the same model runs at 3.6-3.9 MB/s on an M4), and a model whose memory is
  used up with restore method 1 cannot be cut, so the 10gb corpus's two files alone would go past its
  time budgets (`docs/10gb-todo.md`, item 5).
