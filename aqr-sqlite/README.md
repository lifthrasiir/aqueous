# aqr-sqlite (`sqlr`)

Lossless SQLite 3 database recompression: a database file is stored smaller, and restored
bit for bit. Meant as a component of a general-purpose compressor, so speed comes first:
the target is 100 MB/s single-threaded on an Apple M4, both ways.

```sh
cargo build --release
sqlr bench [-n ITERS] [-t N] [-m] [-v] [-x] [-c|-d] FILE...  # round trip check, archive size, CPU-time throughput (wall time with -t > 1)
sqlr fuzz [-n N] [-s SEED] FILE...                           # mutated databases and corrupted streams
```

```rust
let f = aqr_sqlite::formula::encode(&db, 1);      // None where the transform does not apply
```

The module `sqlite-recons` (`aqr_sqlite::formula`) rebuilds the database from the streams of
the transform; archiving (`.aqr`) and the compression of the streams are aqr-formula's. Any
page or record that cannot be coded is kept as it is; a file that is not a database has no
formula.

## Results

Measured on 2026-10-23 at `2abe944` on an Apple M4, one thread, through `aqr-show` (see the
[top-level README](../README.md#how-it-was-measured)): each file archived at level `mid` with
its round-trip check, then extracted. Ratios are tentative, since the decoder modules are not
in the archives yet. Memory is the peak RSS of that process (input, archive and output
included) over the input size.

| set | files | MB | ratio | zstd -3 | compress | decompress |
|---|---|---|---|---|---|---|
| public | 9 | 86.4 | 0.222 | 0.368 | 72.6 MB/s | 374.4 MB/s |
| synthetic | 25 | 114.7 | 0.215 | 0.310 | 33.7 MB/s | 450.6 MB/s |
| system | 50 | 107.5 | 0.269 | 0.445 | 29.9 MB/s | 363.0 MB/s |
| all | 84 | 309 | **0.236** | 0.373 | **38 MB/s** | **395 MB/s** |

2 files are kept as they are. `scripts/baseline.sh` gives the sizes under zstd -3, zstd -19
and xz -9.

### Corpus

`corpus/` is not in the repository; the scripts rebuild it.

- `scripts/fetch-corpus.sh`: public databases (Chinook, Northwind, Sakila, datasette
  databases, GeoPackage and MBTiles samples).
- `scripts/collect-system.sh`: SQLite files under /System, /Library, /Applications (no user
  data).
- `scripts/gen-synthetic.py`: deterministic databases covering page sizes 512-65536, overflow
  chains, update/delete churn (freeblocks, fragmentation, freelists, with and without
  secure_delete), auto_vacuum (pointer maps), UTF-16le/be, reserved bytes, schema formats
  1-3, WITHOUT ROWID, FTS5, R*Tree, WAL.

### Memory

Through `aqr-show` (above), the peak RSS is 8.9× the file (median of files of 8 MB or
more; at most 11.0×). `sqlr bench -m` reports the Rust heap beyond input and output,
encoding and decoding. Build with `--features memprof` for heap use per
phase. Everything is in memory: there is no streaming over large files.

### Threads

`-t N` (`formula::Options::threads`, and the `threads` of `formula::encode` and
`formula::decode`; default 1 for the library and for `bench`): the streams are the same
whatever the number of threads, and one thread runs the serial code unchanged.
`std::thread::scope`, a small work queue (`src/par.rs`), no dependency. A panic on a worker
means no formula, like any other.

- Encoding: the b-tree groups are coded in parallel (each makes its directory entry and
  pieces, put together in group order; an index group reads its table's group, which is
  freed after its last index as before). The page pass and the index key discovery (first
  match wins, in schema order) stay serial. Verification (the file is rebuilt from the
  streams and compared) is a separate pass.
- Decoding: the first pass over the tables and the index orders run in parallel. The page
  loop and the record decoding stay on one thread.

With more than one thread `bench` reports wall time instead of thread CPU time.

## Format

The format is specified in [FORMAT.md](FORMAT.md).

## How

### Pages (`src/transform.rs`, `src/page.rs`)

Every page gets a kind; freelist trunks, pointer maps and anything that fails a check are
raw. A b-tree page is parsed into fields that rebuild it exactly; anything SQLite would
not have written that way (non-minimal varints, overlapping cells, broken chains) makes the
page raw.

- Cell positions: SQLite fills the content area downwards, so a cell mostly ends where an
  earlier one starts. A cell is coded as the rank of its end among the open ends, or as an
  offset (`page::Ends`).
- Child pointers, interior keys and overflow page numbers are deltas.

### Records (`src/record.rs`)

Pages are grouped into b-trees by their child pointers; the records of each tree are coded
by columns; serial types, header lengths and payload lengths follow from the values.

- Integers, per column: as they are, against the previous value of the column, or against
  the previous integer of the record (wide rows of related numbers), whichever is
  cheapest.
- Reals: as m / 10^e when that reproduces the double exactly (most stored decimals), with
  a column's mantissas scaled to its largest exponent when that pays; else as floats when
  exact; else 8 raw bytes.
- Text and blobs: lengths and bytes in their own streams; a value equal to an earlier one
  of the column is coded by recency.
- Integers SQLite would have stored with another serial type, and records that do not add
  up, are kept as they are.

### Indexes (`src/index.rs`)

An index entry of a rowid table is a row's indexed columns and its rowid. The index's key
is found from the schema and the entries themselves (the table column of each indexed
column, or the rowid for an INTEGER PRIMARY KEY; ASC/DESC and BINARY/NOCASE per column).
The decoder sorts the table's rows by that key, as SQLite compares them, and an entry that
matches its row is coded as its rank in that order against the previous entry's rank plus
one. Leaves hold runs of consecutive entries, so an index costs next to nothing.

Sorting uses a radix sort over the prefix bytes that vary (packed with the row number when
they are few), with full comparisons only for ties. The decoder keeps only the indexed
columns of such a table: it decodes the table once for them, and again for its pages.

### Measured, not kept

- Integers byte-transposed or fixed-width: 1% smaller int stream, not worth it.
- Reals byte-transposed or XOR-delta'd: larger under zstd than raw doubles.
- An adaptive binary model for the integer streams: 9% larger than zstd and 5× slower
  (zstd's gain there is exact repeats).
- WITHOUT ROWID secondary indexes: none in the real-world sets.

## Testing

```sh
cargo test --release
../target/release/sqlr fuzz -n 200 $(find corpus -type f -size -4M)
```

- `tests/roundtrip.rs`: databases built with the `sqlite3` tool (page sizes, indexes with
  DESC/NOCASE, overflow, churn, secure_delete, auto_vacuum, UTF-16, WITHOUT ROWID, FTS5),
  non-databases, cut and padded files, the same streams for every number of threads, and
  that verification catches altered streams.
- `tests/formula.rs`: the formula of such databases evaluated with this crate's module,
  the inputs with no formula, and corrupted params and streams (errors, never panics).
- Unit tests: varints, records, decimals, cell placement.
- `sqlr fuzz`: mutated databases (bytes, page types, cell pointers, truncation, junk,
  copied pages, page numbers, header fields) must round-trip with the safety nets off, and
  corrupted streams must fail cleanly or decode.

## Layout

| File | Contents |
|---|---|
| `FORMAT.md` | The module's format, in full |
| `src/formula.rs` | The module `sqlite-recons`: encoding to a formula, decoding |
| `src/transform.rs` | Page classification, streams, encoder and decoder |
| `src/page.rs` | Geometry, b-tree page parsing, cell placement |
| `src/record.rs` | Records, column coding |
| `src/index.rs` | Schema, index keys, index order |
| `src/varint.rs` | SQLite varints, stream cursor (LEB128 from `aqr-common`) |
| `aqr_common::mem` | Heap accounting (`bench -m`, feature `memprof`) |
| `src/bin/sqlr.rs` | Bench, fuzz |
| `scripts/` | Corpus, baselines, profiling (`profile.py` over samply) |

## Next steps

- Memory: payload slices as 32-bit offsets, page-level streams decoded as they are read,
  smaller index sort keys for text columns, chunked processing of large files.
- Speed headroom for compression: the synthetic set and verification.
- WAL and rollback journal files; WITHOUT ROWID secondary indexes.
