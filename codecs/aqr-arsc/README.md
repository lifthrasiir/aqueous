# aqr-arsc

Android resource tables (`resources.arsc` in APKs) rebuilt bit for bit from streams the
archive compresses better (`FORMAT.md`): the strings of the string pools without their
offsets and lengths, as text; the entries of the type chunks by field, their offsets derived,
their keys and values predicted by the configurations before; every other chunk as it is.
Whatever is not laid out as the module rebuilds it is kept as it is.

Written from what is known of the format's chunks; no code of Android's resource tools was read
(`aapt2` was not used either). `FORMAT.md` specifies the module `arsc-recons`, the streams, and how it
works.

```sh
cargo run --release -p aqr-arsc --example aqr-arsc-check -- FILE_OR_DIR...   # rebuilt, streams under zstd
```

## What is taken

- `resources.arsc` of APKs: tables whose string pools, type chunks and packages are laid out as
  `FORMAT.md` rebuilds them; any chunk not so laid out is kept as it is.
- Not taken: compact entries (flag 8, aapt2's `--enable-compact-entries`), sparse and 16-bit offsets
  (type flags 1 and 2), which are kept as they are; binary XML (`AndroidManifest.xml`, `res/**/*.xml`).

## Results

Apple M4, 2026-11-02, the `resources.arsc` of 7 APKs from F-Droid (44.7 MB: Organic Maps, K-9 Mail,
AntennaPod, F-Droid, Fennec, NewPipe, VLC; `corpus/fdroid`, not in git):

| | files as they are | `arsc-recons`' streams |
|---|---|---|
| zstd -9 | 10.60 MB | 6.76 MB (−36.2%; xz -9e: 7.75 MB) |
| `aqr` level mid, the files alone | 9.41 MB | 6.78 MB (−27.9%), tested at 2.1 GB/s |
| `aqr` level mid, the 7 APKs | 188.77 MB | 186.27 MB (−1.3%) |

Compressing the APKs takes 4% fewer instructions (1583 -> 1513 G, two rounds), testing the same
(275 G). The module's Wasm block is 5.0 KB.

## Tests

`cargo rt -p aqr-arsc`; `aqr-arsc-check` rebuilds files and prints the streams under zstd.

## Left

- **String indices (0.94 MB of the 6.76 MB).** The global pool is sorted by configuration and value (runs
  of about one configuration each), so the indices an entry refers to are a permutation. Storing the
  strings in the order of their first reference and deriving the pool's order by sorting each run
  would drop most of it, if the run of every string can be told (strings shared by configurations).
- **Binary XML.** Measured and not taken (2026-11-02): the same chunks with the nodes coded by field, in a
  module of 3 inputs, all files as one stream 0.91 MB against 1.21 MB under zstd -9, but a formula for
  each file costs more than that (a ZIP of K-9's XML: 332 KB against 249 KB; its files average 1.3 KB;
  aqr-zip sends `.xml` entries as text, one content of all). It would pay as one application for all of
  an archive's binary XML, as flate-set does for DEFLATE bodies.
- **Compact and sparse entries.** Kept as they are, as are type specs.
