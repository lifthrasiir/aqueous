# aqr-alz

ALZ archives (ESTsoft's ALZip, common in Korea) taken apart into the contents of their entries:
DEFLATE bodies are rebuilt bit for bit from their data by aqr-flate's module, bzip2 bodies by
aqr-bz2's `bz2-alz-recons` (libbzip2's compressor in ALZip's framing), stored bodies are their
contents. The rest of the archive (the headers, encrypted bodies) is kept as it is. What it is
for is the files: each entry's contents are a content of the archive, recompressed by their
formats (a PE file by aqr-exe) and stored once where the same files are archived along with it.

```sh
cargo run --release -p aqr-alz --example aqr-alz-check -- [-v] [-t N] [-P PROFILE] FILE_OR_DIR...   # rebuilt, sizes, speed
```

```rust
use aqr_alz::formula;
// None where it is no ALZ archive, or no body is rebuilt
let f = formula::encode(&alz, threads).unwrap();
// `f` rebuilds `alz` with aqr-flate's module and aqr-bz2's `bz2-alz-recons` (`formula::apply` runs them)
```

`FORMAT.md` describes the archive as read and the formula, after OctopusET's
[ALZ Archive Format Spec](https://gist.github.com/OctopusET/6a88b6bb3f4eed682c031e704012cfc0)
(CC0). No ALZ decoder's code was read.

## How

The bodies follow the local headers, each of the size its header tells, so the formula is the
bytes kept and the bodies rebuilt in turn, with no module of its own: a module carrying the
headers' CRC-32 and sizes as predictions (aqr-zip's way) would save a few bytes an entry and
cost a module in every archive. ALZip's bzip2 is libbzip2's compressor with the stream header,
the blocks' CRCs, the randomised bit and the combined CRC left out and the magic numbers made 4
bytes (`DLZ\x01`, `DLZ\x02`), always at level 9; the blocks are unaligned as bzip2's are. Its
DEFLATE is zlib's: aqr-flate's adaptive profile (zlib emulation, aqr-compress's levels 7 to 9) leaves
a record of about 0.01% of the streams, the default profile 1-10% (it has no zlib emulation).

## Status

Not taken, kept as they are: encrypted entries (ZIP 2.0's traditional encryption); bodies
running past the data (the next volumes of a split archive are not joined). Left: the volumes of
a split archive as one; the entries' CRC-32 and sizes for aqr-compress's probe of what the files
archived along hold (`entry_sums`, as for ZIP and 7z); bzip2 of several blocks checked against
ALZip itself (only one-block streams were at hand, reproduced exactly).

## Results

Measured on 2026-11-01 on an M4 on the 16 ALZ files at hand (7.31 MB: the 15 test archives of
[alkegi/unalz](https://github.com/alkegi/unalz) and openwall's john-samples' archive of ALZip
11.25, 6.46 MB, one PE file), `aqr` on all of them in one archive, all threads:

| level | before | after | compressing | testing |
|---|---|---|---|---|
| mid | 7,193,922 B | 5,544,794 B (−22.9%) | 0.18 → 1.33 s, 37 → 353 MB peak RSS | 0.31 s, 96 MB |
| 9 | 7,189,640 B | 5,051,268 B (−29.7%) | 0.18 → 4.02 s, 44 → 499 MB | 0.35 s, 100 MB |

`aqr-alz-check -t 10`: every body that is neither encrypted nor cut off by a volume's end is
rebuilt, but tiny DEFLATE bodies whose records are no shorter; the archive of ALZip 11.25's
DEFLATE body (6.46 MB) at 92 MB/s of data with a record of 56,276 B at profile 6, 24 MB/s and
716 B at profile 10.

## Tests

- `tests/formula.rs`: the fixtures of `tests/data` (alkegi/unalz's, CC0: stored, DEFLATE, bzip2,
  directories, encrypted entries, a split archive's first volume) rebuilt; the contents by kind;
  the budget, bzip2 turned down, archives cut at every 997th byte; an archive in other data;
  `src/alz.rs`: records written by hand (a comment section, a cut, a width of no record).
- aqr-compress `src/lib.rs` (`alz_taken_apart`): the DEFLATE and bzip2 fixtures through
  aqr-compress, at the start of a file and found in data; aqr-decompress `tests/wasm.rs`:
  `bz2-alz-recons` natively and as Wasm.
