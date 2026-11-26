# aqr-lha

LHA archives (`.lzh`, `.lha`: LHarc's successors, common in Japan and on the Amiga) taken apart
into the contents of their files: the streams of `-lh4-` to `-lh7-` are split into the data they
decode to and a record of how their compressor wrote them, and rebuilt bit for bit by the module
`lha-recons`; stored files are their contents. The rest of the archive (the headers, the data
of other methods) is kept as it is. What it is for is the files: each file's contents are a
content of the archive, recompressed by their formats and stored once where the same files are
archived along with it.

```sh
cargo run --release -p aqr-lha --example aqr-lha-check -- [-v] FILE_OR_DIR...   # streams decoded, encoded again, split and joined
```

```rust
use aqr_lha::{formula, recons};
// None where it is no LHA archive, or nothing is rebuilt
let f = formula::encode(&lzh, threads).unwrap();
// a stream alone: its data and record
let sp = recons::split(&stream, size, method)?;
assert!(recons::join(&sp.data, &sp.record)? == stream);
```

`FORMAT.md` specifies the streams, the record and the module, describes the archive as read, and
says how they work. No LHA code was read: the streams' format is ar002's as LHA's decoders read it, and the
compressors (the test archives of [lhasa](https://github.com/fragglet/lhasa), Aminet's
archives) were the black boxes.

## What is taken

Taken at the start of a file only (by its first header, or by its name's extension): a header
has no signature to look for in other data, so self-extracting archives and archives within
other files are not taken. Not taken, kept as they are: `-lh1-` (LHarc's adaptive Huffman),
`-lh2-`, `-lh3-`, UNLHA32's `-lhx-`, LArc's `-lzs-` and `-lz5-`, PMarc's `-pm1-` and `-pm2-`,
LHARK's `-lh7-` (another format under the same id), and data past the archive's end byte.

## Results

Measured on 2026-11-01 on an M4.

`aqr-lha-check` on Aminet's sample (119 archives drawn at random from the index, 20 to 900 KB,
of LhA for AmigaOS mostly): 2,022 streams of `-lh5-` and `-lh6-`, 21.47 MB, 42.53 MB of data, all
split and joined again; records 81,702 B, 0.381% of the streams. Split 8.8 s, join 8.5 s on one
thread (4.8 MB/s and 5.0 MB/s of data).

On lhasa's test archives (77 streams of 15 compressors, 855 KB): all split and joined; records
0.875% in all: LhA for AmigaOS 1.22 0.17%, LHa for UNIX 1.14i 0.60%, ExpLZH, LHMelt, MorphOS,
Tascal 0.7–0.95%, LHA 2.13 0.91%, LHA 2.55e and its ports (OS/2, OS-9/68k, Human68K, MacLHA) 1.87%.

`aqr` on Aminet's sample, all of it in one archive, all threads (the archive before is of `aqr`
without aqr-lha, which stores the archives):

| level | before | after | compressing | testing |
|---|---|---|---|---|
| mid | 24,950,134 B | 22,849,402 B (−8.4%) | 0.30 → 4.20 s, 124 → 285 MB peak RSS | 0.16 → 1.17 s, 142 MB |
| 9 | 24,889,536 B | 22,038,446 B (−11.5%) | 0.34 → 6.17 s, 187 → 612 MB | 0.15 → 1.65 s, 150 MB |

Much of Aminet's archives is already compressed (pictures, sounds, stored files): 3.64 MB of the
25.2 MB are stored files, and the data of the streams is often executables and pictures.

## Tests

- `tests/recons.rs`: the streams of the fixtures of `tests/data` (lhasa's, ISC, `tests/data/NOTICE.md`: LHa for UNIX, LHA
  2.55e, LhA for AmigaOS, LHA for OS/2) decoded, checked by their CRC-16, every header as
  predicted, split and joined; random parses (matches neither the nearest nor whole, of any
  method) in blocks of random sizes, headers written otherwise, padding and bytes after it,
  split and joined, their records damaged failing or giving a stream; streams refused.
- `tests/formula.rs`: the fixtures rebuilt (headers of levels 0 to 3, stored files,
  directories); contents by kind; the budget, archives cut at every 997th byte, data after the
  archive.
- aqr-compress `src/lib.rs` (`lha_taken_apart`); aqr-decompress `tests/wasm.rs`
  (`lha_same_as_wasm`): natively and as Wasm alike, damaged records alike.

## Left

- **Other methods.** `-lh1-` first, as old archives have it; then the others listed above.
- **Archives in other data.** Self-extracting programs: a scan for `-lh?-` two bytes into a header.
- **LHA 2.x's choice among the longest matches.** 3% of its matches, most of its record.
- **Speed.** 5 MB/s of data a thread to split and to join; an archive's streams go side by side.
- **The module's size.** 20 KB as a block in every archive, which small archives do not pay back.
