# aqr-wim

WIM files (Windows install and recovery images) taken apart into their resources' data: the
LZX chunks of each resource rebuilt bit for bit by aqr-lzx's module `lzx-recons` from their data
and record, resources of no compression as they are, the rest (header, chunk tables, lookup
table, XML data) kept. The archive then stores the data, which compresses far better together
than in chunks compressed 32 KiB at a time, and which aqr-compress recompresses in turn. The
formula is applications of `lzx-recons` and slices, one after another, so this crate has no
module of its own; [FORMAT.md](FORMAT.md) tells what it holds.

```sh
cargo run --release -p aqr-wim --example aqr-wim-check -- FILE [OFFSET LEN]   # taken apart, evaluated natively, timed
```

```rust
use aqr_wim::formula;
// None where it is no WIM file of LZX or of no compression, or no resource is taken apart
let f = formula::encode(&wim, threads).unwrap();
// `f` rebuilds `wim` with aqr-lzx's `formula::MODULE`
```

Decompression stays bounded whatever the file: a resource is streams of at most 1024 chunks
(32 MiB of data), and an application takes streams of at most 32 MiB of data, so its memory is
about that, its record and its output.

## Results

The Windows Server 2008 R2 DVD (measured on 2026-10-30, 16 threads):

| file | | |
|---|---|---|
| install.wim, 2,809.4 MB (40,124 resources, 38,986 of LZX) | raw 6,857.7 MB, rest 16.8 MB, nothing kept | formula in 32.1 s, evaluated natively in 52.7 s |
| boot.wim, 168.6 MB | | formula in 2.1 s, evaluated in 2.4 s |

Every file was given back. Through `aqr` (via aqr-iso, which gives install.wim as a file of
its own), the DVD (3.17 GB) archived to 2,483.1 MB, compressing in 170 s at 22.9 GB of peak
commit and extracting in 70 s at 1.72 GB. aqr-lzx's README has what the records cost.

## Tests

```sh
cargo rt -p aqr-wim
```

WIM files made of aqr-lzx's fixture chunks (a resource once and twice, one of no compression,
the lookup table and XML data), taken apart and given back; a chunk table off by one (kept);
XPRESS (not taken); files cut short or changed.

## Status

Done: WIM files of LZX and of no compression, in aqr-compress (at the start of data).

Not yet: XPRESS and LZMS (and ESD files, solid LZMS); the lookup table's SHA-1 and the chunk
tables derived rather than kept; the resources cut into the image's files by its metadata;
spanned WIMs (`.swm`).
