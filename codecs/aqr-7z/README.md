# aqr-7z

7z archives rebuilt bit for bit: the packed stream of each folder of LZMA, LZMA2 or Copy alone
from the contents of the files it holds, by aqr-lzma's modules (`lzma-recons`,
`lzma2-recons`), and an encoded header of LZMA likewise; the rest of the archive (the signature
header, folders of other coders, the header) is kept as it is. There is no module of its own:
`FORMAT.md` tells what the formula is.

What it is for is the files: a folder's data is cut into the files' contents, each a slice of
its own (from 1 KiB) in a stream of its kind by its name, so that they are recompressed in turn
by their formats and stored once where the same files are archived along with the archive. The
records cost about a sixth of the packed streams, so a 7z archive alone gains little or
nothing.

```sh
cargo run --release -p aqr-7z --example aqr-7z-check -- [-v] [-t N] FILE_OR_DIR...   # rebuilt, sizes, speed
python3 -I codecs/aqr-7z/scripts/gen-fixtures.py   # tests/data (needs 7zz)
```

```rust
use aqr_7z::formula;
// None where it is no 7z archive, or no folder is taken
let f = formula::encode(&archive, threads).unwrap();
// `f` rebuilds `archive` with aqr-lzma's modules (`aqr_lzma::formula::{MODULE, LZMA2}`)
let sums = formula::entry_sums(&mut |span| read(span)); // the files' CRC-32s and sizes, from the header
```

## Results

Measured on 2026-10-30 on an Apple M4 (10 cores), through `aqr-7z-check -t 4` and `aqr`
(all threads, level mid; the commit before as the baseline).

| | archives | rebuilt from | records | kept |
|---|---|---|---|---|
| the 10gb benchmark's four 7z archives (37.25 MB) | 4 of 4 | 251.1 MB of data | 4.33 MB (18.3% of 23.68 MB packed) | 13.58 MB |

- The large one (`mingw480/*.7z`, 37.10 MB) has an LZMA folder of 64 MiB dictionary (23.43 MB,
  249.4 MB of data, its record 4.32 MB) and a folder of BCJ2 (13.58 MB), kept. Splitting runs
  at 21 MB/s of data, rebuilding at 24 MB/s (one thread: a folder of LZMA is one stream).
- `mingw480` (411.0 MB: the tree of 8,344 files and that archive, whose files are those of the
  tree) archives into 84.45 -> 68.79 MB; the tree alone into 47.36 MB. Compressing takes 5.6 ->
  27.8 s and 1.44 -> 3.34 GB at peak; testing 0.3 -> 10.9 s and 0.39 -> 2.17 GB, the folder
  rebuilt alone.
- Of the 21.43 MB the archive costs beside its tree, 13.58 MB is the BCJ2 folder and 4.32 MB
  the record; about 3.5 MB is what sharing the files' contents leaves (a plain tar of the
  same 8,238 files costs 17.0 MB beside the tree).

## Tests

```sh
cargo rt -p aqr-7z   # 7-Zip's archives: LZMA2, LZMA, solid or not, header encoded or plain, Copy,
                     # LZMA2 blocks; and archives of `write`
```

## Status

Done: the container (headers encoded or plain), folders of LZMA, LZMA2 or Copy alone, and in
aqr-compress: 7z archives detected anywhere (a self-extractor's), their files' CRC-32s for
batching them with the same files (`together`). Left:

- Folders of filters: BCJ and BCJ2 (the 10gb archive's 13.58 MB), ARM64, Delta. Their data
  is the filter's output, not the files'; rebuilding them needs the filters' encoders (BCJ2
  with its range coder's decisions recorded).
- PPMd, BZip2, Deflate folders; encrypted archives are kept as they are.
- Rebuilding a folder of LZMA is one stream on one thread, slow for large folders (10.7 s for
  the 10gb archive's), and its match finder takes `12 × S` bytes (768 MiB for 64 MiB).
