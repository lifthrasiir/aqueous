# aqr-git

git pack recompression. A pack is rebuilt bit for bit from the data of its objects, so the
archive stores the data instead: it compresses better, is recompressed in turn (a PNG or a
gzip file committed is found as such), and a blob's data is the file checked out, which the
archive stores once. The zlib streams of the entries are rebuilt by aqr-flate's module
(`flate-set`, one application for all of them), from records of aqr-flate's profile as
aqr-compress picks it for any DEFLATE stream: the default (static, fast) below level 7, the
adaptive one with zlib emulation from 7 on. With the adaptive one, the zlib settings git writes
every object with are found once on a few of the largest streams and tried first on the
others, which are then emulated exactly, for next to no record. The module `git-pack-recons` derives the
entries' headers from the sizes of their data, the zlib headers and checksums, and the pack's
trailer (SHA-1), and keeps what is not what git writes as it is.

```sh
cargo run --release -p aqr-git --example aqr-git-check -- [-v] [-t N] [-p PROFILE] [-z] [-d DIR] FILE_OR_DIR...
```

```rust
use aqr_git::formula::{self, MODULE};
// None where it is not a pack
let f = formula::encode(&pack, 8).unwrap();
// `f` rebuilds `pack` when evaluated with `formula::apply` (`MODULE` and aqr-flate's)
```

`FORMAT.md` specifies the module `git-pack-recons`.

## The rest of a checkout

aqr-git used to take a whole checkout (the working tree and `.git`) into ten streams of its
own, with its own container; everything it did besides the packs is now what the archive does
for any files:

- **Loose objects** are zlib files: aqr-compress takes them as such (aqr-flate).
- **The working tree** is the blobs' data again: the archive finds it in the packs' data where
  they are archived together (in solid blocks with long-distance matching, or as slices of
  their own for blobs of 256 KiB or more), and loose objects' data the same way.
- **Files of the same content**, paths, symlinks, per-file checksums: the archive's.

Not taken (yet): `.idx` and `.rev` files, which follow from their pack but only with every
object's id (SHA-1 of the resolved objects, deltas applied): they stay as they are (the ids do
not compress). SHA-256 repositories (ids of 32 bytes in ref-deltas) are not taken as packs.

## Results

`corpus/local` (not in the repository): 15 repositories, the whole `.git` and the files git
would track or not ignore, 1195.8 MB in all. Archived with `aqr` (level `mid`, 10 threads,
Apple M4, 2026-10-28; the round-trip check included), against the former aqr-git (its own
container: the whole checkout in ten streams, zstd once over each):

| | output | of input |
|---|---|---|
| `tar \| zstd -19 --long=27` | 778.6 MB | 0.651 |
| former aqr-git, `-l 3` | 587.5 MB | 0.491 |
| former aqr-git (default: zstd -19, 100 MB/s budget) | 540.9 MB | 0.452 |
| **`aqr`** | **543.1 MB** | **0.454** |
| `aqr`, packs at aqr-flate's adaptive profile (zlib emulated) | 514.7 MB | 0.430 |
| former aqr-git, `-A` (every zlib stream rebuilt, zstd -19) | 304.8 MB | 0.255 |

| repository | input | `aqr` | adaptive | former, `-l 3` | former, default | compress / decompress |
|---|---|---|---|---|---|---|
| pyre | 664.9 MB | 326.3 MB | 302.8 MB | 327.1 MB | 313.5 MB | 13.4 / 126 MB/s |
| SaturnTranslatorInternal | 142.9 MB | 50.3 MB | 47.6 MB | 49.1 MB | 44.1 MB | 27.3 / 265 MB/s |
| wah | 125.6 MB | 22.1 MB | 21.8 MB | 61.0 MB | 36.7 MB | 20.7 / 172 MB/s |
| loupe | 119.0 MB | 50.8 MB | 50.8 MB | 49.4 MB | 49.4 MB | 75.3 / 248 MB/s |
| unison | 59.6 MB | 37.3 MB | 35.9 MB | 35.0 MB | 33.3 MB | 23.7 / 86 MB/s |
| aqueous | 38.7 MB | 31.4 MB | 31.3 MB | 32.7 MB | 32.4 MB | 24.2 / 277 MB/s |
| sudachi.rs | 20.1 MB | 14.9 MB | 14.6 MB | 18.1 MB | 17.8 MB | 12.7 / 96 MB/s |
| wasm3-rs | 12.0 MB | 6.4 MB | 6.2 MB | 7.9 MB | 7.7 MB | 14.0 / 109 MB/s |
| duc | 9.0 MB | 1.9 MB | 1.9 MB | 5.8 MB | 4.5 MB | 22.5 / 180 MB/s |

The six smallest (0.003-1.5 MB in all 3.9 MB) come to 1.72 MB, against 1.43 MB: the archive's
blocks of the modules they need weigh on them.

- "adaptive": the packs at aqr-flate's adaptive profile with zlib emulation, as from level 7.
- The former aqr-git's `-A` is smaller mostly as it compressed all the packs' data of a
  repository in one stream: the archive puts streams of a kind in solid blocks of at most 16
  MiB (the bound of what a file read alone decodes), with long-distance matching within a
  block. unison (adaptive): 36.0 MB, 28.2 MB with blocks of 256 MiB, 27.0 MB by the former `-A -l 3`
  (which also derived the 0.66 MB of `.idx`). Splitting the deltas into copies and inserts, as
  the former did, was worth 3% of their bytes.
- Packs alone (unison's 6, 36.0 MB, `aqr-git-check`, of the packs; their data is three times
  as much): at the default profile 16.9 / 27.2 MB/s on one thread, the records 1.73 MB (1.54 MB
  at zstd -3); at the adaptive one with zlib emulation (`-p 10 -z`) 7.9 / 18.8 MB/s on one
  thread, 24.5 / 88 MB/s on 10, the records 254 KB (79 KB) for 23,721 entries and 105.7 MB of
  data. Through `aqr` on all 15, where the rest takes most of the time, the adaptive profile
  took 75.1 s and 8.43 s against 69.8 s and 8.29 s, for 28.4 MB less.
- A loose object's data is its header and the blob: aqr-compress keeps the header with the
  container's bytes, so that the blob is a slice of its own, found where it is a file checked
  out (and in the same batch, `together`), and taken as a file of its format (text, a gzip
  file). loupe's two loose objects of 16 and 33 MB, which are its two largest files: 98.0 MB
  before, 50.8 MB.

## Tools

- `examples/check.rs`: the packs in files rebuilt, zstd -3 of them and of the formulas'
  streams by class, the encoder's and modules' speeds; `-d` writes the streams out.
- `tests/formula.rs` builds repositories with the git CLI (ofs- and ref-deltas, compression
  levels 0, 1 and 9, a tag), checks every pack rebuilt with every entry taken and next to no
  record, the budget of inflated bytes, wrong trailers and data after the pack, and flips
  bytes in every input of the module.
