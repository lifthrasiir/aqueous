# aqr-egg

EGG archives (ESTsoft's ALZip, the successor of ALZ, common in Korea) taken apart into the
contents of their files: DEFLATE blocks are rebuilt bit for bit from their data by aqr-flate's
module, bzip2 blocks by aqr-bz2's `bz2-recons`, LZMA blocks by aqr-lzma's `lzma-recons`, AZO
blocks (ESTsoft's own compressor) by aqr-azo's `azo-recons`, stored blocks are their contents.
The rest of the archive (the headers, encrypted files) is kept as it is. What it is for is the files: each file's contents are a content of the archive,
recompressed by their formats and stored once where the same files are archived along with it.

```sh
cargo run --release -p aqr-egg --example aqr-egg-check -- [-v] [-t N] [-P PROFILE] FILE_OR_DIR...   # rebuilt, sizes, speed
```

```rust
use aqr_egg::formula;
// None where it is no EGG archive, or no block is rebuilt
let f = formula::encode(&egg, threads).unwrap();
// `f` rebuilds `egg` with aqr-flate's, aqr-bz2's and aqr-lzma's modules (`formula::apply` runs them)
```

`FORMAT.md` describes the archive as read, the formula and how it works (where the archives
seen differ from the spec too), after OctopusET's
[EGG Archive Format Specification](https://gist.github.com/OctopusET/0ad7da5da238b9153960d0bc4dfe2a50)
(CC0). No EGG decoder's code was read.

## What is taken

Not taken, kept as they are: encrypted files (ZIP 2.0's traditional encryption, AES, LEA);
blocks running past the data, and continuation volumes (the volumes of a split archive are not
joined).

## Results

Measured on 2026-11-01 on an M4 on the 327 EGG files at hand (49.5 MB): the test archives of
[akolman/EggDotNet](https://github.com/akolman/EggDotNet) (MIT),
[seb3773/alzgg](https://github.com/seb3773/alzgg) and
[blurfx/unegg](https://github.com/blurfx/unegg), mostly the same few files archived by every
method, level, solid or not, encrypted or not and split, so that much of the gain is the files
stored once. `aqr` on all of them in one archive, all threads (the archive before is of `aqr`
without aqr-egg):

| level | before | after | compressing | testing |
|---|---|---|---|---|
| mid | 33,188,274 B | 16,656,772 B (−49.8%) | 1.02 → 2.63 s, 264 → 963 MB peak RSS | 0.63 s, 221 MB |
| 9 | 31,871,586 B | 16,422,048 B (−48.5%) | 3.04 → 3.69 s, 685 → 1174 MB | 0.65 s, 216 MB |

`aqr-egg-check -t 10`: every block that is neither encrypted, cut off by a volume's end nor over
the budget (zeros of 1 and 5 GB) is rebuilt, but tiny DEFLATE blocks whose records are no
shorter. Of the 41.0 MB of blocks, 8.35 MB of DEFLATE and nearly all of the bzip2 (0.16 MB) are over
the budget; stored, LZMA and AZO blocks are all rebuilt. Records: DEFLATE 0.79% at profile 6, 0.08%
at profile 10; LZMA 1.22%; AZO 1.03%.

## Tests

- `tests/formula.rs`: the fixtures of `tests/data` (EggDotNet's, MIT, `tests/data/NOTICE.md`: DEFLATE, LZMA, AZO of
  text and of digits, solid, a comment of the archive, a name of a locale, an encrypted file)
  rebuilt; a solid block's contents by kind; bzip2 and stored blocks among others kept (archives
  written by `egg::write`); the budget, bzip2 turned down, archives cut at every 97th byte; an
  archive in other data; `src/egg.rs`: records written by hand (a cut, a block header without
  its end marker).
- aqr-compress `src/lib.rs` (`egg_taken_apart`): DEFLATE and LZMA archives through aqr-compress,
  at the start of a file and found in data.

## Left

- **Split archives.** The volumes of a split archive are not joined into one.
- **Solid archives' files for the probe.** `egg::entry_sums` gives aqr-compress's probe none of a
  solid archive's files.
