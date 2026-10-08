# sqlite-recons (`sqlr`)

Lossless SQLite 3 database recompression: a database file is stored smaller, and restored
bit for bit. Meant as a component of a general-purpose compressor, so speed comes first:
the target is 100 MB/s single-threaded on an Apple M4, both ways. zstd is the entropy
backend.

```sh
cargo build --release
sqlr c [-l LEVEL] [-b LEVEL] [-x] [-L] in.db out.sqlr   # compress
sqlr d out.sqlr back.db                                 # decompress
sqlr bench [-n ITERS] [-m] [-v] [-c|-d] FILE...         # round trip check, ratio, CPU-time throughput
sqlr fuzz [-n N] [-s SEED] FILE...                      # mutated databases and containers
```

```rust
let packed = sqlite_recons::compress(&db);              // never fails: stores what it cannot code
assert_eq!(sqlite_recons::decompress(&packed)?, db);    // checked against an xxh3 of the original
```

Any input is accepted: a file that is not a database is zstd-compressed whole, and any page
or record that cannot be coded is kept as it is.

## Results

Apple M4, one thread, thread CPU time, best of 3 (`sqlr bench -n 3`). Output size / input
size; every file round-trips. zstd 1.5.7 (`--single-thread`) and xz 5.8 (`-T1`) on each
file, summed (`scripts/baseline.sh`).

| set | files | size | **sqlr** | compress | decompress | zstd -3 | zstd -19 | xz -9 |
|---|---|---|---|---|---|---|---|---|
| public (real databases) | 13 | 87 MB | **0.1708** | 122 MB/s | 386 MB/s | 0.3685 | 0.2542 | 0.2079 |
| system (macOS system databases) | 116 | 127 MB | **0.2358** | 119 MB/s | 367 MB/s | 0.4152 | 0.3292 | 0.2913 |
| synthetic (format features) | 25 | 115 MB | **0.2048** | 90 MB/s | 518 MB/s | 0.3115 | 0.2482 | 0.2300 |
| **all** | 154 | 328 MB | **0.2078** | **107 MB/s** | **415 MB/s** | 0.3666 | 0.2811 | 0.2478 |

16% smaller than xz -9 and 26% smaller than zstd -19 overall. Compression includes
verification (the file is rebuilt from the streams and compared; on a mismatch it is stored
whole), about a fifth of its time.

The synthetic set is below the speed target: its random text keeps zstd's level-9 match
finder busy. With `-l 5 -b 8` the whole corpus compresses at about 119 MB/s for 0.2092;
with `-l 3 -b 3` at 178 MB/s for 0.2181; `-x` skips verification.

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

Peak RSS (input and output included):

| file | size | compress | decompress |
|---|---|---|---|
| northwind.db | 24.7 MB | 131 MB | 75 MB |
| til.db | 21.1 MB | 105 MB | 56 MB |
| Certificates valid.sqlite | 13.4 MB | 76 MB | 47 MB |
| overflow_4096.db | 8.7 MB | 44 MB | 22 MB |

`sqlr bench -m` reports the Rust heap beyond input and output: at most 3.0× the input
compressing and 2.5× decompressing on the files above 5 MB (more on small files, from
fixed overheads). Build with `--features memprof` for heap use per phase. Everything is in
memory: there is no streaming over large files.

## Format

The format is frozen at this version: `scripts/format-ref.tsv` holds a hash of the output for
every corpus file, and `scripts/format-check.sh` checks that changes keep it.

```text
"SQLR" version(1) mode | varint(length) | u64 LE xxh3 of the original
mode 0: zstd(file)
mode 1: varint(page size) varint(usable size) u8(schema format >= 4)
        18 streams: varint(raw length) u8(0 stored | 1 zstd) varint(coded length) data
```

zstd is level 7 for every stream but text/blob content (level 9), with long-distance
matching (window 2^27) for inputs over 2 MB.

## How

### Pages (`src/transform.rs`, `src/page.rs`)

Every page gets a kind: a b-tree page, an overflow page (rebuilt with the cell that owns
it), an all-zero page, or a raw page (freelist trunks, pointer maps, anything that fails a
check). A b-tree page is parsed into fields that rebuild it exactly; anything SQLite would
not have written that way (non-minimal varints, overlapping cells, broken chains) makes the
page raw.

- Header fields; the content start against the lowest cell.
- Cell positions: SQLite fills the content area downwards, so a cell mostly ends where an
  earlier one starts. Open ends are kept most recent last, and a cell is coded as the rank
  of its end among them, or as an offset (`page::Ends`).
- Child pointers and interior keys as deltas; overflow chains as page number deltas, the
  payload put together first.
- Every byte no header, pointer or cell covers (free space, freeblocks, the database
  header, reserved bytes, the end of the last overflow page) goes to the filler stream as
  it is.

### Records (`src/record.rs`)

Pages are grouped into b-trees by their child pointers; the records of each tree are coded
by columns. Per value a tag (NULL, integer, real, text, blob, or one of the special forms
below); serial types, header lengths and payload lengths follow from the values.

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
- Higher levels for the small streams: little gain for the time.
- WITHOUT ROWID secondary indexes: none in the real-world sets.

## Testing

```sh
cargo test --release
scripts/format-check.sh
target/release/sqlr fuzz -n 200 $(find corpus -type f -size -4M)
```

- `tests/roundtrip.rs`: databases built with the `sqlite3` tool (page sizes, indexes with
  DESC/NOCASE, overflow, churn, secure_delete, auto_vacuum, UTF-16, WITHOUT ROWID, FTS5),
  non-databases, cut and padded files, 2,000 corrupted containers (errors, never panics),
  and that verification catches altered streams.
- Unit tests: varints, records, decimals, cell placement.
- `sqlr fuzz`: mutated databases (bytes, page types, cell pointers, truncation, junk,
  copied pages, page numbers, header fields) must round-trip with the safety nets off, and
  corrupted containers must fail cleanly; over 40,000 runs on the corpus without a failure.

## Layout

| File | Contents |
|---|---|
| `src/container.rs` | File format, compression (with verification), decompression |
| `src/transform.rs` | Page classification, streams, encoder and decoder |
| `src/page.rs` | Geometry, b-tree page parsing, cell placement |
| `src/record.rs` | Records, column coding |
| `src/index.rs` | Schema, index keys, index order |
| `src/varint.rs` | SQLite varints, LEB128 |
| `src/mem.rs` | Heap accounting (`bench -m`, feature `memprof`) |
| `src/bin/sqlr.rs` | Command-line tool, bench, fuzz |
| `scripts/` | Corpus, baselines, format check, profiling (`profile.py` over samply) |

## Next steps

- Memory: payload slices as 32-bit offsets, page-level streams decoded as they are read,
  smaller index sort keys for text columns, chunked processing of large files.
- Speed headroom for compression: the synthetic set and verification.
- WAL and rollback journal files; WITHOUT ROWID secondary indexes.
