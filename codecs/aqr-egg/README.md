# aqr-egg

EGG archives (ESTsoft's ALZip, the successor of ALZ, common in Korea) taken apart into the
contents of their files: DEFLATE blocks are rebuilt bit for bit from their data by aqr-flate's
module, bzip2 blocks by aqr-bz2's `bz2-recons`, LZMA blocks by aqr-lzma's `lzma-recons`, stored
blocks are their contents. The rest of the archive (the headers, encrypted files, AZO blocks) is
kept as it is. What it is for is the files: each file's contents are a content of the archive,
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

`FORMAT.md` describes the archive as read and the formula, after OctopusET's
[EGG Archive Format Specification](https://gist.github.com/OctopusET/0ad7da5da238b9153960d0bc4dfe2a50)
(CC0). No EGG decoder's code was read.

## How

The blocks follow their headers, each of the size its header tells, so the formula is the bytes
kept and the blocks rebuilt in turn, with no module of its own (as aqr-alz). ALZip's DEFLATE is
zlib's: aqr-flate's adaptive profile (zlib emulation, aqr-compress's level 9) leaves a record of
0.08% of the blocks, the default profile 0.79%. Its bzip2 is libbzip2's, a stream a block. Its
LZMA is the LZMA SDK's (lc 3, lp 0, pb 2, a dictionary of 16 MiB, no end marker), whose parse
aqr-lzma records in 1.2% of the streams. A solid archive is one block after its last file holding
all the files' data, cut into their contents by their sizes.

The archives seen differ from the spec in two places, which `FORMAT.md` tells: a filename's
locale is told by bit 3 of its flags and not counted by its size, and an LZMA block's 4 bytes the
spec calls reserved are ZIP's header of LZMA.

## Status

Not taken, kept as they are: AZO blocks (ESTsoft's own LZ77 with a range coder: only its decoder
is specified, so rebuilding it needs its encoder's parse recorded, as aqr-lzo does for LZO);
encrypted files (ZIP 2.0's traditional encryption, AES, LEA); blocks running past the data, and
continuation volumes (the volumes of a split archive are not joined). Left: AZO by a module of
its own; the volumes of a split archive as one; the files' CRC-32 and sizes for aqr-compress's
probe of what the files archived along hold (`entry_sums`, as for ZIP and 7z).

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
| mid | 33,188,274 B | 17,759,716 B (−46.5%) | 1.02 → 1.33 s, 264 → 855 MB peak RSS | 0.49 s, 193 MB |
| 9 | 31,871,586 B | 17,525,744 B (−45.0%) | 3.04 → 2.47 s, 685 → 1139 MB | 0.50 s, 219 MB |

`aqr-egg-check -t 10`: every block that is neither encrypted, of AZO, cut off by a volume's end
nor over the budget (zeros of 1 and 5 GB) is rebuilt, but tiny DEFLATE blocks whose records are
no shorter. Of the 41.0 MB of blocks: stored 9.38 MB and LZMA 3.93 MB all rebuilt, DEFLATE
23.58 MB (15.23 MB rebuilt, the rest over the budget), bzip2 0.16 MB (all but 338 B over the
budget); AZO 3.12 MB and encrypted 0.85 MB kept. Records: DEFLATE 0.79% at profile 6 (299 MB/s
of data to encode, mostly zeros of solid archives), 0.08% at profile 10 (65 MB/s); LZMA 1.22%.

## Tests

- `tests/formula.rs`: the fixtures of `tests/data` (EggDotNet's, MIT: DEFLATE, LZMA, AZO, solid,
  a comment of the archive, a name of a locale, an encrypted file) rebuilt; a solid block's
  contents by kind; bzip2 and stored blocks among others kept (archives written by
  `egg::write`); the budget, bzip2 turned down, archives cut at every 97th byte; an archive in
  other data; `src/egg.rs`: records written by hand (a cut, a block header without its end
  marker).
- aqr-compress `src/lib.rs` (`egg_taken_apart`): DEFLATE and LZMA archives through aqr-compress,
  at the start of a file and found in data.
