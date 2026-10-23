# aqr-lz4

LZ4 recompression. An LZ4 stream is rebuilt bit for bit from the data it holds, so the archive
stores the data instead. Its compressor is liblz4 1.10.0's fast compressor, reimplemented to
give the same bytes. LZ4 compresses lightly and hides the data from everything after it.
Once it is undone, zstd compresses the data better, aqr-compress recompresses what the data
holds, and files written again and again (sessions, telemetry pings) share their redundancy
in an archive.

```sh
cargo run --release -p aqr-lz4 --example check-lz4 -- FILE_OR_DIR...   # reproduced, zstd -3 before and after, speed
python3 aqr-lz4/scripts/gen-fixtures.py                            # tests/data from the reference liblz4
```

```rust
use aqr_lz4::formula::{self, MODULE};
// None where the file is not a container taken, or its stream is not liblz4's
let f = formula::encode(&file, 1).unwrap();
// `f` rebuilds `file` when evaluated with `formula::decode` as the module `MODULE`
```

`FORMAT.md` specifies the module `lz4-recons`, with the compressor in full.

## What is taken

- **mozlz4**: Firefox's `mozLz40\0` container around one block, made by
  `LZ4_compress_default`. Examples are the session store (`sessionstore-backups/*.jsonlz4`),
  `addonStartup.json.lz4`, `search.json.mozlz4`, bookmark backups and the archived telemetry
  pings (`datareporting/archived/*/*.jsonlz4`).
- **LZ4 frames** with independent blocks at a fast level: `LZ4F_compressFrame` and the `lz4`
  command line by default, and `--fast=N` too.

The following are not reproduced: frames with linked blocks (`lz4 -BD`), HC levels
(`lz4 -3` and above), frames with a dictionary, and the legacy frame. They stay as they are.

## Results

Measured on 2026-10-23 with the reference liblz4 1.10.0 (through ctypes, before this crate
existed), on one Firefox profile on Windows. The session store, `addonStartup.json.lz4` and 178
telemetry pings over two months were all `LZ4_compress_default`, all reproduced.

| | files | lz4 → zstd -3 | data → zstd -3 |
|---|---|---|---|
| session store, addons | 3 | 1.43 MB | 1.30 MB (−9.1%) |
| ditto, window 2^27 + long matching | 3 | 1.43 MB | 1.17 MB (−17.8%) |
| telemetry pings, each alone | 178 | 2.10 MB | 1.73 MB (−17.5%) |
| telemetry pings, solid | 178 | 1.075 MB | 0.465 MB (−56.7%) |

This crate reproduces the fixtures of `tests/data` (blocks at accelerations 1-65537, and
frames with checksums, kept tables, stored blocks and `--fast=3`), which
`scripts/gen-fixtures.py` makes with the reference liblz4. It also reproduced 4 of the files
above (the session store, `addonStartup.json.lz4` and a telemetry ping, 2.36 MB) with `check-lz4`.

The module (the compressor) runs at 650-1150 MB/s of data on an M4, single-threaded, on the
fixtures (`check-lz4`). That is liblz4's speed class. Frames could be made in parallel, since a
block of 4 KiB or more clears the state. That is not done.

## Tests

- `tests/formula.rs`: the fixtures reproduced and turned down as they should be, what
  follows a container, damaged containers and module inputs, and compressor round trips
  around the thresholds of the tables.
- aqr-compress `tests/compress.rs` (`lz4_nested`): a mozlz4 around gzip around FLAC, and a
  mozlz4 found in data.
