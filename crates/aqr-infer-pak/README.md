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

## Usage

```sh
cargo build -r -p aqr-infer-pak --examples
target/release/examples/aqr-infer-pak-probe FILE...   # AQR_PAK_VERBOSE=1: each table's tests and times
target/release/examples/aqr-infer-pak-batch PATH...   # groups by magic, templates (directories: a level deep)
.venv/Scripts/python -I crates/aqr-infer-pak/scripts/check.py EXPECT  # samples against what is known of them
AQR_PAK_VERBOSE=1 target/release/examples/aqr-infer-pak-probe FILE 2>&1 | python crates/aqr-infer-pak/scripts/costs.py
```

`aqr-infer-pak-batch` runs a batch as aqr would trigger it: files that start with the same 8
bytes (3 at least, not text or zeros) are a group, each with its family; the biggest family is
inferred, the others verify its template (or are inferred, and learned from, where it does not
hold), those left with nothing retried by the templates learned later; a group's inferences
stop where those since its last directory found tested more rows of tables than 2 million and
than one for each 64 bytes of their families.

## Results

Not used by aqr-compress: the plan of the paks cost up to 45 s on a game of 10 GB (Ori DE:
158 -> 35 s, Outer Wilds 90 -> 39 s with it trimmed) and gained only where the members are of a
codec aqr rebuilds and the scan cannot find (`docs/file-formats.md` has the numbers; the last
version with it is `4a93bf8`, `src/pak.rs`). The crate stays with its probe and batch.

- Directories found: 15 of 15 on the test set (The Crew, No Man's Sky, four files of other
  formats; `scripts/check.py` against the counts the headers give), none found wrongly.
- The Crew's `data_win32` (174 files, 87 families): all 87, 85 by templates in 0.1 s in all, 2
  inferred in 2.4 s. The Crew, 14 files, 766.21 MB (LZO1X members): 442.72 MB archived with the
  paks, 617.14 MB without (-28.3%), 32.3 MB/s.
- No Man's Sky's `PCBANKS` (98 files): 95 of 97 paks, 93 by templates in 4.5 s, 4 inferred in
  6.0 s.
- A survey of a Steam library (about 150 games, 416 files of 66 kinds, by
  `aqr-infer-pak-probe`): Wwise banks, XACT wave banks, Unreal 3 packages, Unity files and
  others found with their headers' counts. Unreal 4 paks, Godot's, Source's VPKs (records of names), Chromium's paks and CRI's AFS2 (offsets with no sizes) are not looked for: those many games share have recompressors of their own, aqr-bundle's `packs`.

## Tests

- `tests/synthetic.rs`: directories of made-up formats (offsets and sizes, sizes end to end, templates, chunked targets, random bytes), found and checked.
- `tests/overflow.rs`: a last member of size `u64::MAX` (no overflow, no panic).

The samples and their counts are in an expectations file of the user's (the docstring of
`scripts/check.py` has its format), not in the repository.

## Left

- **Directories of fewer than 8 rows.** Not inferred; templates find them where a family has
  a template.
- **Stored paks.** The two uncompressed paks of No Man's Sky's `PCBANKS` are missed (their
  entries' table is labelled the wrong stride).
- **Gain.** An inference anchored on the members a scan finds; a measure of members repeated
  across one game's paks (`docs/file-formats.md`).
