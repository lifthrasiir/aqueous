# aqr-git

git pack recompression. A pack is rebuilt bit for bit from the data of its objects, so the
archive stores the data instead: it compresses better, is recompressed in turn (a PNG or a
gzip file committed is found as such), and a blob's data is the file checked out, which the
archive stores once. The zlib streams of the entries are rebuilt by aqr-flate's module
(`flate-set`, one application for all of them); the module `git-pack-recons` derives the
entries' headers from the sizes of their data, the zlib headers and checksums, and the pack's
trailer (SHA-1), and keeps what is not what git writes as it is. `FORMAT.md` specifies the
module `git-pack-recons` and `git-pack-index-recons` (the pack with its index and reverse index
after it, of which only the ids of the entries are stored), and how they work.

```sh
cargo run --release -p aqr-git --example aqr-git-check -- [-v] [-t N] [-p PROFILE] [-z] [-i] [-d DIR] FILE_OR_DIR...
sh codecs/aqr-git/scripts/gen-fixtures.sh    # tests/data, with the git CLI
```

```rust
use aqr_git::formula::{self, MODULE};
// None where it is not a pack
let f = formula::encode(&pack, 8).unwrap();
// `f` rebuilds `pack` when evaluated with `formula::apply` (`MODULE` and aqr-flate's)
```

## What is taken

- Packs of SHA-1 repositories (version 2 or 3), where each entry has a zlib stream that ends
  within the data; the entries rebuilt are those aqr-flate takes and whose record is short.
- `.idx` (version 2) and `.rev` (version 1) files follow from their pack and the ids of its
  entries: aqr-compress finds them by the pack's trailer, which they repeat, and codes the three
  as a set (`git-pack-index-recons`), storing only the ids (on 62 packs of 35 local
  repositories, 592 MB: 29.6 MB of indexes become 18.48 MB of ids, incompressible).
- Everything else of a checkout is the archive's own: loose objects are zlib files (aqr-flate),
  the working tree is the blobs' data again (found in the packs' data where archived together),
  and files of the same content, paths, symlinks and checksums are the archive's.
- Not taken: SHA-256 repositories (ids of 32 bytes in ref-deltas) are not packs here;
  `multi-pack-index` and `commit-graph` are kept as they are.

## Results

`corpus/local` (not in the repository): 15 repositories, the whole `.git` and the files git
would track or not ignore, 1195.8 MB in all. Archived with `aqr` (level `mid`, 10 threads, Apple
M4, 2026-10-28; the round-trip check included):

| | output | of input |
|---|---|---|
| `tar \| zstd -19 --long=27` | 778.6 MB | 0.651 |
| **`aqr`** | **543.1 MB** | **0.454** |
| `aqr`, packs at aqr-flate's adaptive profile (zlib emulated, as from level 7) | 514.7 MB | 0.430 |

| repository | input | `aqr` | adaptive | compress / decompress | peak RSS |
|---|---|---|---|---|---|
| pyre | 664.9 MB | 326.3 MB | 302.8 MB | 20.3 / 126 MB/s | 7.09 GB |
| SaturnTranslatorInternal | 142.9 MB | 50.3 MB | 47.6 MB | 30.9 / 265 MB/s | 1.41 GB |
| wah | 125.6 MB | 22.1 MB | 21.8 MB | 20.7 / 174 MB/s | 1.98 GB |
| loupe | 119.0 MB | 50.8 MB | 50.8 MB | 73.9 / 372 MB/s | 0.75 GB |
| unison | 59.6 MB | 37.3 MB | 35.9 MB | 22.7 / 86 MB/s | 0.86 GB |
| aqueous | 38.7 MB | 31.4 MB | 31.3 MB | 24.1 / 277 MB/s | 0.53 GB |
| sudachi.rs | 20.1 MB | 14.9 MB | 14.6 MB | 13.1 / 101 MB/s | 0.39 GB |
| wasm3-rs | 12.0 MB | 6.4 MB | 6.2 MB | 16.0 / 109 MB/s | 0.21 GB |
| duc | 9.0 MB | 1.9 MB | 1.9 MB | 21.9 / 180 MB/s | 0.22 GB |

The six smallest (0.003-1.5 MB, 3.9 MB in all) come to 1.72 MB: the archive's blocks of the
modules they need weigh on them. Packs alone (unison's 6, 36.0 MB; `aqr-git-check`): at the
default profile 16.9 / 27.2 MB/s on one thread with the records 1.73 MB; at the adaptive one
with zlib emulation (`-p 10 -z`) 7.9 / 18.8 MB/s on one thread (24.5 / 88 MB/s on 10) with the
records 254 KB, for 23,721 entries and 105.7 MB of data. pyre's pack (222 MB, 817k entries,
490 MB of data) archives at a peak of 2.48 GB of heap (3.9 GB resident). A loose object's blob
is a slice of its own, found where it is a file checked out: loupe's two largest files, loose,
came to 50.8 MB from 98.0 MB.

## Tests

- `tests/formula.rs` builds repositories with the git CLI (ofs- and ref-deltas, compression
  levels 0, 1 and 9, a tag), checks every pack rebuilt with every entry taken and next to no
  record, the budget of inflated bytes, wrong trailers and data after the pack, and flips bytes
  in every input of the module.
- `tests/part.rs`: the part modules against the whole decoding.
- `examples/check.rs` (`aqr-git-check`): the packs in files rebuilt, zstd -3 of them and of the
  formulas' streams by class, the encoder's and modules' speeds; `-d` writes the streams out.

## Left

- **Ids from the objects.** The ids follow from the objects (SHA-1 of each resolved, deltas
  applied) but at the cost of resolving every delta chain as `git index-pack` does; they are
  18.48 MB of incompressible bytes on the 592 MB of packs above.
- **`multi-pack-index`.** It follows from the indexes; kept as it is.
- **`commit-graph`.** Kept as it is.
- **Solid blocks of packs.** The archive's 16 MiB solid-block bound keeps packs' data from
  compressing as one stream (unison, adaptive: 36.0 MB, 28.2 MB with blocks of 256 MiB); pack
  blobs in `together`.
