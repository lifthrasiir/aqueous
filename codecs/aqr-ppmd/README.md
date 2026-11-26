# aqr-ppmd

PPMd var. J files (Dmitry Shkarin's PPMd, `.pmd`) taken apart into their header and their data, so that
the data of a file whose text starts another's is stored once, as a part of the other's. There is no
module: the file is the concatenation of the two. PPMd's coder is adaptive, so two files of texts one of
which starts the other have the same data up to the end of the shorter (but for its last bytes); their
headers differ in the file's time and name, which cut off leaves the data starting alike. `FORMAT.md`
specifies the header and the formula, and why the data is not decoded.

## What is taken

- A file starting with a header of var. J (order 2 or more, restore method 0 to 2) followed by data.
  Only the first header is cut off: a file of several files has the others within its data.
- aqr-compress takes the formula whatever it costs alone (it costs no more than the file raw), and puts
  files that start alike past their headers in the same batch (by pieces of their first 64 KiB).
- Not taken: other variants (none was seen).

## Results

Apple M4 (10 cores, all threads, level mid), 2026-10-30, `cargo aqr -c` on the 10gb corpus, whose
`enwik8.pmd` (21.4 MB) and `enwik9.pmd` (184.0 MB) are var. J, order 10, 256 MB, restore method 1, of
`enwik8` and `enwik9` (the first 100 MB of the other):

| | before | after |
|---|---|---|
| the corpus | 2,579,204,100 B | 2,557,815,082 B (−21.39 MB) |
| `enwik8.pmd` | 21.39 MB | 0 (all of its data within `enwik9.pmd`'s) |

Instructions retired 7.788 T -> 7.810 T. The two files differ in 12 bytes: the time (3 bytes) and the
name's last letter, and the last 8 bytes of `enwik8.pmd` (where its coder ends).

## Tests

`cargo rt -p aqr-ppmd`: the header of `enwik9.pmd`, and headers that are none. aqr-compress's
`ppmd_held_once` archives two files made alike.

## Left

- **The data itself.** A PPMd var. J file that is not a prefix of another costs what it is (a decoder and an
  encoder of var. J would each run at a few MB/s, so rebuilding it is not attempted).
