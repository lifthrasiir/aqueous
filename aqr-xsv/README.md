# aqr-xsv

Delimited text tables rebuilt bit for bit from their fields, stored column by column: CSV,
TSV, the `;` and `|` variants, and USV (U+241F and U+241E, or their ASCII forms). Text fields
are kept as they are, for the archive to compress; numbers are coded as integers in the way
they were printed ([`aqr-num`](../aqr-num)), as they are or as differences from the column's
previous one. Records that are not of the table (a quote not closed, a zero byte) are kept as
they are, so any file round-trips.

```rust
use aqr_xsv::formula::{self, MODULE};
// None where the file is not a table this crate takes.
let f = formula::encode(&csv, 1 /* threads */).unwrap();
// `f` rebuilds `csv` when evaluated with `formula::decode` as the module `MODULE`.
```

aqr-compress takes a file as a table by its text (`formula::starts`: its first 64 KiB cut into
records of the same number of fields, nine in ten times), before aqr-wrt, and where its formula
is smaller than the file kept raw; a file named as text (`.csv`, `.tsv`, `.log`, ...) is tried
as a table too. Without the default feature `encode`, the crate is the module's code alone
(`module`), which aqr-modules builds into the Wasm module `xsv-recons`.

The format is specified in [FORMAT.md](FORMAT.md).

## Results

Measured on 2026-10-28 on an Apple M4, one thread, with `aqr-xsv-bench` (the streams as
zstd -3 compresses them, against the file through zstd -3).

| file | MB | zstd -3 | streams | ratio | encode | decode |
|---|---|---|---|---|---|---|
| cmz `airport-codes.csv` | 8.82 | 3,080,865 | 2,513,334 | 0.816 | 98 MB/s | 733 MB/s |
| 10gb `2011/2011aug*.csv` (GPS logs, 7) | 1.06 | 298,151 | 122,039 | 0.409 | 39–94 MB/s | 584–719 MB/s |
| 10gb `UnicodeData.txt` (perl 5.10) | 1.12 | 172,623 | 156,475 | 0.906 | 153 MB/s | 701 MB/s |
| 10gb `Name.pl` (perl 5.10, TAB) | 0.65 | 127,455 | 120,906 | 0.949 | 479 MB/s | 1,219 MB/s |

Through `aqr` at level mid (10 threads): `airport-codes.csv` alone is 2.74 -> 2.32 MB, and
cmz's 27 test files together 39.32 -> 38.83 MB. The 10gb corpus is 2,630,733,924 ->
2,629,027,404 B (−1.71 MB): of 106 files that start as tables, 85 are coded as tables (and 4
tables inside other files), Unicode tables, CMaps, AFM fonts and the like, and some C sources
of tables of numbers; the rest go to aqr-wrt or are kept as they are; compression 137.9 -> 141.7 s, its peak
footprint 5.79 -> 5.55 GB.

## Testing

```sh
cargo rt -p aqr-xsv
```

Unit tests: dialects, records (quotes, CR LF, fields over lines, records kept as they are),
round trips of edge cases and of every dialect, numbers of several writers, and damaged
streams (errors, never panics).

## Layout

| File | Contents |
|---|---|
| `src/formula.rs` | The module: formula of a file (columns, numbers, predictors) |
| `src/parse.rs` | Dialect from a sample, records and fields (encoder) |
| `src/table.rs` | Params, stream codes, the table rebuilt (the module's code) |
| `src/module.rs` | The module's entry |
| `examples/bench.rs` | Sizes of the streams, speeds, round trip |

## Next steps

- Numbers within text fields (`"40.07, -74.93"`, dates, times): a field's template and its
  numbers, as aqr-obj does with lines.
- Hexadecimal fields (`UnicodeData.txt`'s code points).
- The numbers coded better than varints for zstd (rANS per column, as aqr-obj does).
- JSON (a separate crate): the values by key path, as columns are here.
