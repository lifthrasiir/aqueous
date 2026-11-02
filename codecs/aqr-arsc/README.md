# aqr-arsc

Android resource tables (`resources.arsc` in APKs) rebuilt bit for bit from streams the
archive compresses better (`FORMAT.md`): the strings of the string pools without their
offsets and lengths, as text; the entries of the type chunks by field, their offsets derived,
their keys and values predicted by the configurations before; every other chunk as it is.
Whatever is not laid out as the module rebuilds it is kept as it is.

Written from what is known of the format's chunks; no code of Android's resource tools was
read (`aapt2` was not used either).

```sh
cargo run --release -p aqr-arsc --example aqr-arsc-check -- FILE_OR_DIR...   # rebuilt, streams under zstd
```

## How it does

Measured on 2026-11-02 on an M4, on the `resources.arsc` of 7 APKs from F-Droid (44.7 MB:
Organic Maps, K-9 Mail, AntennaPod, F-Droid, Fennec, NewPipe, VLC; `corpus/fdroid`, not in
git).

- `aqr-arsc-check` (zstd -9): the files 10.60 MB, the module's 6 inputs 6.76 MB (−36.2%).
  xz -9e takes the files to 7.75 MB.
- Through `aqr` (level mid) on the files alone: 9.41 -> 6.78 MB (−27.9%), tested at 2.1 GB/s.
- The 7 APKs through `aqr` (level mid): 188.77 -> 186.27 MB (−1.3%; with aqr-dex, 202.53 MB
  before both); compressing takes 4% fewer instructions (two rounds: 1583 -> 1513 G),
  testing the same (275 G).
- The module's Wasm block is 7.7 KB.

| part | under zstd -9 | as it is, under zstd -9 |
|---|---|---|
| strings | 5.53 MB | 8.29 MB (data and offsets) |
| entries (presence, flags, keys, value types) | 0.11 MB | 2.08 MB (entries and offsets) |
| string indices | 0.94 MB | |
| other values, complex entries | 0.16 MB | |

## Status

Left:

- The string indices (0.94 MB): the global pool is sorted by configuration and value (runs of
  about one configuration each), so the indices an entry refers to in it are a permutation.
  Strings stored in the order of their first reference, the pool's order derived by sorting
  each run, would drop most of it, if the run of every string can be told (strings shared by
  configurations).
- Binary XML (`AndroidManifest.xml`, `res/**/*.xml`; 6,914 files of 9.2 MB in the 7 APKs) was
  tried and not taken (2026-11-02): the same chunks, its nodes coded by field (lines in
  deltas, ends from the starts open, attributes by name, raw value, type and data) in a module
  of 3 inputs. Its streams of all files as one came to 0.91 MB under zstd -9 against 1.21 MB,
  but a formula for each file costs more than that: a ZIP of K-9's XML archived to 332 KB
  instead of 249 KB with every file taken (its files are 1.3 KB on average), and the files
  loose to 171 KB with those that are smaller alone taken, which none in a ZIP is (aqr-zip
  sends `.xml` entries as text, one content of all). It would pay as one application for all
  of an archive's binary XML, as flate-set does for DEFLATE bodies.
- Compact entries (flag 8, aapt2's `--enable-compact-entries`), sparse and 16-bit offsets
  (type flags 1 and 2): kept as they are.
- Type specs as they are.
