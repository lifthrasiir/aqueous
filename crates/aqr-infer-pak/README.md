# aqr-infer-pak

Directories of unknown archive formats (game paks: `.pak`, `.dat`/`.fat`, ...) inferred with
nothing about the format known: [docs/pak-inference-design.md](../../docs/pak-inference-design.md)
has the method, `scripts/pak_probe.py` its prototype. A directory is a table of fixed-size
records whose fields point into a *target* (a file of the family, the files of one stem in one
directory, or one decoded in place), recognised by what the pointers do there: the members
tile the target.

- [`tables`](src/tables.rs): tables of records in a file's head (grown while tables from the
  head run on) and tail: strides by byte autocorrelation in tiles, regions where one repeats,
  each region's stride by KT column code lengths, its ends by a maximum-sum run.
- [`fields`](src/fields.rs): a table's candidate fields, from its bit profile (where a field's
  lowest bit rises after a constant one) and the whole 2-, 4- and 8-byte fields at every byte.
- [`tiling`](src/tiling.rs): the tests. Offsets whose rooms (up to the next distinct offset)
  sizes fill, on the longest run of rows that fit; or sizes laid end to end from right after
  the table to the end of the target exactly, the run starting after the last row that is no
  size. Evidence in bits against chance and against a control (sizes paired with the wrong
  members); members must fill the target.
- [`codec`](src/codec.rs): the members' codecs (zstd, zlib, DEFLATE, LZO1X, LZ4 blocks), each
  member decoded whole or not at all.
- [`target`](src/target.rs): targets decoded in place, which the tables are tested on in turn
  (a pak of zstd chunks whose directory points into the chunks decoded).

## Status

Not used by aqr yet (docs/pak-inference-design.md, section 8). On the test set (The Crew,
No Man's Sky, four files of other formats), `scripts/check.py` against the counts the headers
give: 15 of 15 directories found, none found wrongly. Directories of fewer than 8 rows are not
looked for (a layout learned from a bigger file of the family would be: section 7).

## Usage

```sh
cargo build -r -p aqr-infer-pak --examples
target/release/examples/aqr-infer-pak-probe FILE...   # AQR_PAK_VERBOSE=1: each table's tests and times
.venv/Scripts/python -I crates/aqr-infer-pak/scripts/check.py        # the test set (local paths)
AQR_PAK_VERBOSE=1 target/release/examples/aqr-infer-pak-probe FILE 2>&1 | python crates/aqr-infer-pak/scripts/costs.py
```
