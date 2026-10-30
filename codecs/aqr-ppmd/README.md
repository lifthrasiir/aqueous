# aqr-ppmd

PPMd var. J files (Dmitry Shkarin's PPMd, `.pmd`) taken apart into their header and their data
(`FORMAT.md`). The file is the concatenation of the two, so there is no module: what it is for
is that the data of a file whose text starts another's is stored once, as a part of the
other's data. PPMd's coder is adaptive, so two files of texts one of which starts the other
have the same data up to the end of the shorter (but for its last bytes); their headers differ
in the file's time and name, which cut off leaves the data starting alike.

The data is not decoded or coded again. A decoder and an encoder of var. J would each run at a
few MB/s (7-Zip's Ppmd8 of the same model runs at 3.6–3.9 MB/s on an M4), and a model whose
memory is used up with restore method 1 cannot be cut, so the 10gb corpus's two files alone
would go past its time budgets (`docs/10gb-todo.md`, item 5).

## What it reads

The header, as the files seen tell it: the signature 0x84ACAF8F, the file's attributes
(4 bytes), the model (2 bytes: the order less 1 in 4 bits, the memory in MB less 1 in 8, the
variant from `A` in 4), the length of the name and the restore method (2 bytes: 14 bits and
2), the file's time (4 bytes), then the name. A file is taken where it starts with a header of
var. J (order 2 or more, restore method 0 to 2) followed by data. Only the first header is cut
off: a file of several files has the others within its data. Files of other variants are not
taken; none was seen.

## How it does

Measured 2026-10-30 on an Apple M4 (10 cores; all threads, level mid) through `cargo aqr -c` on
the 10gb corpus, whose `enwik8.pmd` (21.4 MB) and `enwik9.pmd` (184.0 MB) are var. J, order 10,
256 MB, restore method 1, of `enwik8` and `enwik9` (the first 100 MB of the other):

| | before | after |
|---|---|---|
| the corpus | 2,579,204,100 B | 2,557,815,082 B (−21.39 MB) |
| `enwik8.pmd` | 21.39 MB | 0 (all of its data within `enwik9.pmd`'s; the archive's report gives each half the shared blocks) |
| instructions retired | 7.788 T | 7.810 T |

The two differ in 12 bytes: the time (3 bytes) and the name's last letter, and the last 8 bytes
of `enwik8.pmd` (where its coder ends).

aqr-compress takes a file's formula whatever it costs alone (one of these costs no more than
the file raw), and puts files that start alike past their headers in the same batch (by pieces
of their first 64 KiB).

## Tests

`cargo rt -p aqr-ppmd`: the header of `enwik9.pmd`, and headers that are none. aqr-compress's
`ppmd_held_once` archives two files made alike.
