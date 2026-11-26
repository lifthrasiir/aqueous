# aqr-snappy

Snappy recompression. A Snappy stream is rebuilt bit for bit from the data it holds, so the
archive stores the data instead. Its compressor is Snappy 1.2.2's, reimplemented to give the
same bytes, with either of its hashes. LevelDB tables, whose blocks Snappy compresses, are
rebuilt whole from the contents of their blocks: the trailers (types and masked CRCs) and the
footer are derived.

```sh
cargo run --release -p aqr-snappy --example aqr-snappy-check -- FILE_OR_DIR...   # tables rebuilt, zstd -3 before and after, speed
python3 codecs/aqr-snappy/scripts/gen-fixtures.py MUL_LIB CRC_LIB      # tests/data from the reference Snappy
```

```rust
use aqr_snappy::formula::{self, TABLE_MODULE};
// None where the file is not a LevelDB table, or none of its blocks is Snappy's
let f = formula::encode(&table, 1).unwrap();
// `f` rebuilds `table` when evaluated with `formula::decode_table` as the module `TABLE_MODULE`
```

`FORMAT.md` specifies the modules `snappy-recons` (a part module) and `leveldb-recons`,
with the compressor in full, and how it works.

## What is taken

- LevelDB tables (Chrome's LevelDB stores) whose Snappy blocks are Snappy 1.1.10's or later, with either
  hash (multiplication, or CRC-32C as Homebrew's arm64 builds); raw blocks and blocks of other
  compressors are kept as they are. Snappy streams alone are `snappy-recons`'s (a part module).
- Not taken: a LevelDB table found in the middle of data (it is known by its end), and tables of
  Snappy 1.1.5-1.1.9 (other hashes).

## Results

The LevelDB tables of one Chrome profile on macOS, 2026-10-23: 33 tables (13.3 MB), 5108 Snappy
blocks, all 1.2.2 with multiplication, all rebuilt, no block kept.

| | zstd -3 |
|---|---|
| tables | 6.06 MB |
| contents of the blocks (trailers and footers derived) | 4.21 MB (−30.5%) |

The module makes the tables at 315 MB/s; the compressor runs at about 1 GB/s with multiplication and
0.9 GB/s with CRC-32C, on blocks of 4 KiB, on an M4, single-threaded (`bench_compress`).

## Tests

- `tests/formula.rs`: the fixtures reproduced, damaged tables, streams and module inputs, and
  compressor round trips around the thresholds of the tables.
- aqr-compress `tests/compress.rs` (`leveldb_tables`): the fixture tables, and that no table is
  found in the middle of data.

## Left

- **Older Snappy.** The hashes of 1.1.5-1.1.8 and 1.1.9 are measured but not reproduced; tables written by
  them are kept as they are.
