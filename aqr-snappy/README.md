# aqr-snappy

Snappy recompression. A Snappy stream is rebuilt bit for bit from the data it holds, so the
archive stores the data instead. Its compressor is Snappy 1.2.2's, reimplemented to give the
same bytes, with either of its hashes. LevelDB tables, whose blocks Snappy compresses, are
rebuilt whole from the contents of their blocks: the trailers (types and masked CRCs) and the
footer are derived.

```sh
cargo run --release -p aqr-snappy --example check -- FILE_OR_DIR...   # tables rebuilt, zstd -3 before and after, speed
python3 aqr-snappy/scripts/gen-fixtures.py MUL_LIB CRC_LIB             # tests/data from the reference Snappy
```

```rust
use aqr_snappy::formula::{self, MODULE};
// None where the file is not a LevelDB table, or none of its blocks is Snappy's
let f = formula::encode(&table, 1).unwrap();
// `f` rebuilds `table` when evaluated with `formula::decode` as the module `MODULE`
```

`FORMAT.md` specifies the module `snappy-recons`, with the compressor in full.

## The hash

Snappy's output depends on its version and, since 1.1.10, on its build. The families
measured with the reference libraries are 1.1.5-1.1.8, 1.1.9, and 1.1.10 and later. The
last splits in two: builds hashing with the CRC-32C instruction (SSE4.2, ARMv8 with CRC, as
Homebrew's on Apple silicon) and builds hashing by multiplication. 1.2.0 through the main
branch of 2026-09 are one within each. This crate does both hashes of the last family.

## Results

Measured on 2026-10-23 with the reference libraries (through ctypes, before this crate
existed), on the LevelDB tables of one Chrome profile on macOS. There were 33 tables (13.3 MB)
and 5108 Snappy blocks. All were 1.2.2 with multiplication, none with CRC-32C.

| | zstd -3 |
|---|---|
| tables | 6.06 MB |
| contents of the blocks | 4.24 MB (−30.0%) |

This crate rebuilt all 33 tables with `check`, keeping no block as it is. Its inputs take
4.21 MB under zstd -3 (−30.5%), the trailers and footers being derived. The module made the
tables at 315 MB/s.

It also reproduces the fixtures of `tests/data` (streams with both hashes, over fragment
boundaries, and tables with raw blocks, a block of two fragments and a block by the other hash,
kept). `scripts/gen-fixtures.py` makes them with the reference libraries. The compressor runs
at about 1 GB/s with multiplication and 0.9 GB/s with CRC-32C, on blocks of 4 KiB, on an M4,
single-threaded (`bench_compress`).

## Tests

- `tests/formula.rs`: the fixtures reproduced, damaged tables, streams and module inputs, and
  compressor round trips around the thresholds of the tables.
- aqr-compress `tests/compress.rs` (`leveldb_tables`): the fixture tables, and that no table is
  found in the middle of data.
