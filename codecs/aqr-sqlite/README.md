# aqr-sqlite

Lossless SQLite 3 database recompression: a database file is stored smaller, and restored bit
for bit. Speed comes first: the target is 100 MB/s single-threaded on an Apple M4, both ways.
Pages are parsed, records coded by columns, and index entries predicted from their tables, so
that an index costs next to nothing. `FORMAT.md` specifies the module `sqlite-recons`, and how it
works.

```sh
cargo build --release --example aqr-sqlite   # target/release/examples/aqr-sqlite
aqr-sqlite bench [-n ITERS] [-t N] [-m] [-v] [-x] [-Z OPTION] [-c|-d] FILE...  # round trip check, archive size, CPU-time throughput (wall time with -t > 1)
aqr-sqlite fuzz [-n N] [-s SEED] FILE...                           # mutated databases and corrupted streams
```

```rust
let f = aqr_sqlite::formula::encode(&db, 1);      // None where the transform does not apply
```

Archiving (`.aqr`) and the compression of the streams are aqr-formula's. Any page or record
that cannot be coded is kept as it is; a file that is not a database has no formula.

## What is taken

SQLite 3 database files (rowid tables and their indexes, page sizes 512-65536, overflow chains,
freelists, auto_vacuum, UTF-8 and UTF-16, FTS5 and R*Tree tables as ordinary b-trees); whatever
a page or record holds that SQLite would not have written that way is kept raw. Not taken: WAL
and rollback journal files, WITHOUT ROWID secondary indexes (none in the real-world sets).

## Results

Measured on 2026-10-24 at `da98d11` on an Apple M4, one thread, through `aqr-show` (see the
[file formats overview](../../docs/file-formats.md#how-it-was-measured)): each file archived at
level `mid` with its round-trip check, then extracted. Ratios are tentative (the decoder modules
were not in the archives then).

| set | files | MB | ratio | zstd -3 | compress | decompress |
|---|---|---|---|---|---|---|
| public | 9 | 86.4 | 0.222 | 0.368 | 75.1 MB/s | 373.8 MB/s |
| synthetic | 25 | 114.7 | 0.215 | 0.310 | 43.2 MB/s | 476.6 MB/s |
| system | 54 | 123.2 | 0.241 | 0.418 | 41.5 MB/s | 301.1 MB/s |
| all | 88 | 324 | **0.227** | 0.366 | **48 MB/s** | **368 MB/s** |

Measured again with the modules in the archives on 2026-11-01 (`docs/file-formats.md`): 0.210-0.219.

2 files are kept as they are. `scripts/baseline.sh` gives the sizes under zstd -3, zstd -19
and xz -9.

## Corpus

`corpus/` is not in the repository; the scripts rebuild it.

- `scripts/fetch-corpus.sh`: public databases (Chinook, Northwind, Sakila, datasette
  databases, GeoPackage and MBTiles samples).
- `scripts/collect-system.sh`: SQLite files under /System, /Library, /Applications (no user
  data).
- `scripts/gen-synthetic.py`: deterministic databases covering page sizes 512-65536, overflow
  chains, update/delete churn (freeblocks, fragmentation, freelists, with and without
  secure_delete), auto_vacuum (pointer maps), UTF-16le/be, reserved bytes, schema formats
  1-3, WITHOUT ROWID, FTS5, R*Tree, WAL.

Memory is 24 MB + 6.7x the file (peak RSS, a linear fit; everything is in memory, no streaming
over large files). `-t N` codes the b-tree groups in parallel (encoding) and the tables and index
orders (decoding); the streams are the same whatever the number of threads.

## Tests

```sh
cargo test --release
../../target/release/examples/aqr-sqlite fuzz -n 200 $(find corpus -type f -size -4M)
```

- `tests/roundtrip.rs`: databases built with the `sqlite3` tool (page sizes, indexes with
  DESC/NOCASE, overflow, churn, secure_delete, auto_vacuum, UTF-16, WITHOUT ROWID, FTS5),
  non-databases, cut and padded files, the same streams for every number of threads, and
  that verification catches altered streams.
- `tests/formula.rs`: the formula of such databases evaluated with this crate's module,
  the inputs with no formula, and corrupted params and streams (errors, never panics).
- Unit tests: varints, records, decimals, cell placement.
- `aqr-sqlite fuzz`: mutated databases (bytes, page types, cell pointers, truncation, junk,
  copied pages, page numbers, header fields) must round-trip with the safety nets off, and
  corrupted streams must fail cleanly or decode.

## Layout

| File | Contents |
|---|---|
| `src/formula.rs` | The module `sqlite-recons`: encoding to a formula, decoding |
| `src/transform.rs` | Page classification, streams, encoder and decoder |
| `src/page.rs` | Geometry, b-tree page parsing, cell placement |
| `src/record.rs` | Records, column coding |
| `src/index.rs` | Schema, index keys, index order |
| `src/varint.rs` | SQLite varints, stream cursor (LEB128 from `aqr-common`) |
| `aqr_common::mem` | Heap accounting (`bench -m`, feature `memprof`) |
| `src/main.rs` | Bench, fuzz |
| `scripts/` | Corpus, baselines, profiling (`profile.py` over samply) |

## Left

- **Memory.** Payload slices as 32-bit offsets, page-level streams decoded as they are read,
  smaller index sort keys for text columns, chunked processing of large files (6.7x the file now).
- **Compression speed.** Headroom on the synthetic set (43 MB/s) and in verification.
- **Other files.** WAL and rollback journal files; WITHOUT ROWID secondary indexes.
