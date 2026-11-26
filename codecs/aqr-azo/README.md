# aqr-azo

AZO streams (ESTsoft's own compressor in ALZip's EGG archives, method 3) split into the data
their blocks decode to and a record of the parse ALZip's compressor chose, and rebuilt bit for
bit from them by the module `azo-recons`. aqr-egg takes the AZO blocks of EGG archives through
it, so that the files they hold are stored as data.

```sh
cargo run --release -p aqr-azo --example aqr-azo-check -- [-v] FILE_OR_DIR...   # EGG files: decoded, encoded again, split and joined
```

```rust
use aqr_azo::recons;
let sp = recons::split(&stream, usize::MAX, threads)?;  // data (x86 filter undone) and record
assert!(recons::join(&sp.data, &sp.record, threads)? == stream);
```

`FORMAT.md` specifies the streams as far as the module needs them, the record and the module, and how
they work, after OctopusET's
[EGG Archive Format Specification](https://gist.github.com/OctopusET/0ad7da5da238b9153960d0bc4dfe2a50)
(CC0), Appendix A. No AZO decoder's code was read; ALZip's output (the test archives of
EggDotNet and alzgg) was the black box.

## What is taken

Streams of ALZip's EGG archives as aqr-egg finds them. The x86 filter (bit 0 of a stream's
flags) is undone: the data of a stream is the file itself (a block that the filter does not give
back is not taken).

## Results

Measured on 2026-11-01 on an M4, `aqr-azo-check` on the 17 AZO streams of the EGG test archives
at hand (3.12 MB of streams, 11.4 MB of data; aqr-egg's README): every stream split and joined
again; records 32,215 B, 1.03% of the streams (2.1% of the coded blocks; 5.2% for a PE file,
7.1% for 7 MB of digits, 0.6% for nearly random data). Split 2.1 s, join 1.8 s on one thread.

## Tests

- `tests/recons.rs`: the AZO streams of aqr-egg's fixtures split and joined; random parses of
  every kind of token (matches neither the nearest nor whole, remembered ones) encoded, decoded,
  split and joined, of stored and coded blocks and the x86 flag, their records damaged failing
  or giving a stream of the data; streams refused (version, sizes, checks, blocks of other
  sizes, the limit).
- aqr-decompress `tests/wasm.rs` (`azo_same_as_wasm`): natively and as Wasm alike, damaged
  records alike; aqr-egg `tests/formula.rs`: AZO blocks rebuilt in EGG archives.

## Left

- **ALZip's choice between a match and a literal.** Most of the record (1.03% of the streams).
- **Speed.** 5-6 MB/s of data a thread to split and to join; the chains are walked twice where a
  match may start.
