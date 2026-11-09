# aqr-lzx

LZX streams split into their data and a record of their parse, and rebuilt bit for bit. The
archive then stores the data, which compresses better than the streams, which aqr-compress
recompresses in turn (a WIM image's files, a CAB's executables) and which other files may
share. Two variants are taken: chunks compressed apart, of WIM files' resources and of WOF's
LZX files on NTFS (32 KiB each), and the streams of CAB folders and CHM sections (windows of
32 KiB to 2 MiB, frames of 32 KiB). aqr-wim, aqr-disk and aqr-mszip take those containers apart
and make the applications.

No encoder is reproduced. Microsoft's encoders are closed and their parse optimal-style, so the
record codes the parse given the data, under contexts of what a match finder sees there (the
longest match, the repeat offsets' matches), a new offset by its rank among those of its length,
and the trees against Huffman codes of each block's frequencies. It was written from the public
description of LZX (MS-PATCH), without any implementation's code. The streams, the record and
the module `lzx-recons` are in [FORMAT.md](FORMAT.md).

```sh
cargo run --release -p aqr-lzx --example aqr-lzx-check -- [-r] [-s K] FILE [OFFSET LEN]   # a WIM's chunks read, written back; -r: split and rebuilt
cargo run --release -p aqr-mszip --example aqr-mszip-lzx -- [-x DIR] [-r] FILE...         # CAB folders and CHM sections
```

```rust
use aqr_lzx::{lzx, recons};
// a WIM resource's chunks, each its bytes and its data's length
let sp = recons::split(&chunks)?;
assert_eq!(recons::join(&sp.data, &sp.record)?, chunks.concat());
// a CAB stream of n bytes of data, window of 2^21
let v = lzx::Variant::cab(21).unwrap();
let sp = recons::split_stream(&stream, n, v)?;
```

## Results

install.wim of a Windows Server 2008 R2 DVD (2,538.1 MB of chunks in 38,986 resources),
aqr-lzx-check -r, measured on 2026-10-30:

| | |
|---|---|
| data | 6,603.3 MB |
| records | 171.0 MB, 6.74% of the chunks: kinds 91.1, lengths 72.7, trees 6.3, offsets 0.8, the rest 0.1 MB |
| speed, one thread | split 9.5 MB/s, rebuilt 20.8 MB/s of data (before the decoding tables were made small) |

Every resource was rebuilt. With the data at `zstd -3 --long` (1,765.5 MB), about 1,936 MB in
all against 2,538 MB of LZX, before the data is recompressed further. Through `aqr` the whole
DVD (3.17 GB) archives to 2,483.1 MB (aqr-wim's README).

CAB and CHM files (aqr-mszip's README, on an M4, 2026-10-31): 456 LZX folders (584.7 MB) of 441
CAB files with records of 7.88%, split at 22 and rebuilt at 37 MB/s of data on one thread; 139
CHM files' sections with records of 4.91%.

## Tests

```sh
cargo rt -p aqr-lzx
```

Chunks and CAB streams of a small encoder of the tests (greedy matches, blocks of random types
and sizes, pretree runs taken or not, e8 at random): read and written back, split and rebuilt;
damaged records. `tests/data` holds its fixtures (`write_fixture` and the others, ignored),
which aqr-decompress runs as WebAssembly.

## Status

The module `lzx-recons` (natively and as a Wasm module of aqr-modules), WIM files (aqr-wim),
WOF's LZX files in disk images (aqr-disk) and CAB and CHM files (aqr-mszip) are done.

Not yet: speed (a large CAB folder is one stream on one thread, which keeps LZX CABs to level
9); LZX DELTA (MS-PATCH's reference data); LZMS and XPRESS (ESD files, WIMs of other
compression).
