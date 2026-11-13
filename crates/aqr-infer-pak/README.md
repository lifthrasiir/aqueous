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
- [`template`](src/template.rs): the layout of the directories inferred in one family (where
  each table starts, the header field counting its rows, stride, fields, files, base, codec),
  verified in another of the same format: no search and no evidence of its own, so that a
  directory of a single row is found where its template holds.

## Status

Used by aqr-compress (`src/pak.rs`: families of a file of 4 MiB or more inferred, the others
verified by templates). On the test set (The Crew, No Man's Sky, four files of other formats),
`scripts/check.py` against the counts the headers give: 15 of 15 directories found, none found wrongly. The samples and their counts are in an
expectations file of the user's (the script's docstring has its format), not in the repository.
The inference does not look for directories of fewer than 8 rows; templates find them.

`aqr-infer-pak-batch` runs a batch as aqr would trigger it: files that start with the same 8
bytes (3 at least, not text or zeros) are a group, each with its family; the biggest family is
inferred, the others verify its template (or are inferred, and learned from, where it does not
hold), those left with nothing retried by the templates learned later; a group's inferences
stop where those since its last directory found tested more rows of tables than 2 million and
than one for each 64 bytes of their families (as aqr-compress's do). The Crew's `data_win32`
(174 files, 87 families): all 87, 85 by templates in 0.1 s in all, 2 inferred in 2.4 s. No Man's
Sky's `PCBANKS` (98 files): 95 of 97 paks, 93 by templates in 4.5 s (mostly the first touch of
the mapped files), 4 inferred in 6.0 s; the two paks stored uncompressed are missed (their
entries' table is labelled the wrong stride).

Other games (a survey of a Steam library: about 150 games, 416 files of 66 kinds, by
`aqr-infer-pak-probe`): Wwise banks, XACT wave banks, Unreal 3 packages, Unity files and others
found with their headers' counts; Unreal 4 paks, Godot's, Source's VPKs (records of names),
Chromium's paks and CRI's AFS2 (offsets with no sizes) are not looked for. aqr's plan of the
paks on 22 of the games (`AQR_PAK_VERBOSE=1`, before the archive's batches): 0.1 to 49 s, the
most for Unity games of textures (Ori DE, 10 GB: 158 -> 35 s; Outer Wilds 90 -> 39 s).

## Usage

```sh
cargo build -r -p aqr-infer-pak --examples
target/release/examples/aqr-infer-pak-probe FILE...   # AQR_PAK_VERBOSE=1: each table's tests and times
target/release/examples/aqr-infer-pak-batch PATH...   # groups by magic, templates (directories: a level deep)
.venv/Scripts/python -I crates/aqr-infer-pak/scripts/check.py EXPECT  # samples against what is known of them
AQR_PAK_VERBOSE=1 target/release/examples/aqr-infer-pak-probe FILE 2>&1 | python crates/aqr-infer-pak/scripts/costs.py
```
