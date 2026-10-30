# The formula of a PPMd file

aqr-ppmd has no module: the formula of a PPMd var. J file is its bytes cut in two, the root a
concatenation of two slices. This document tells what the streams hold, for whoever reads or
recompresses the formula.

## Streams

| stream | class | content | what |
|---|---|---|---|
| 0 | `ppmd/header` | no | the file's header: 16 bytes and the name (README "What it reads") |
| 1 | `ppmd/data` | no | the rest of the file, PPMd's coded data as it is |

The root is `Concat(Slice(0, 0, header), Slice(1, 0, data))`. A file with nothing after its
header has no formula.
