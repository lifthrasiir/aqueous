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
let sp = recons::split(&stream, usize::MAX, threads)?;  // data (before the x86 filter) and record
assert!(recons::join(&sp.data, &sp.record, threads)? == stream);
```

`FORMAT.md` specifies the streams as far as the module needs them, the record and the module,
after OctopusET's
[EGG Archive Format Specification](https://gist.github.com/OctopusET/0ad7da5da238b9153960d0bc4dfe2a50)
(CC0), Appendix A. No AZO decoder's code was read; ALZip's output (the test archives of
EggDotNet and alzgg) was the black box.

## How

AZO is LZ77 under a binary range coder with adaptive models (the spec's Appendix A): literals by
the byte before, matches as one of two recent distances, a distance and a length, or a
remembered match again (a history of 128). `azo` decodes a block into its data and tokens and
encodes tokens again; the spec's decoder was right in every model, and two things it leaves out
were found from ALZip's streams: the models start afresh at each block (of 512 KiB), and how the
coder ends (one more bit put off, then the zero bytes at the end dropped).

The record (`recons`, as aqr-lzo's) codes the tokens given the data. ALZip's matches are the
nearest ones of their lengths, with the whole common prefix, so a match is told by which of the
longest matches along hash chains of 2 to 16 bytes it is, mostly the longest; whether a match
starts is coded against the matches at the position and the next (ALZip matches lazily, or by
cost).

`azo-recons` is a whole module with an exact memory plan (`plan.rs`, `aqr_api::stack`): the models,
the match finder and the contexts of the coded block, written onto the output as the tokens come.
It spawns no tasks.

## Status

The x86 filter (bit 0 of a stream's flags) is E8/E9 with a converted address, the five bytes
skipped whether converted or not (FORMAT.md §2.6; found from the CRC-32 of the four `.sys`
streams of alzgg's archives, `docs/azo-x86-filter-search.md`). The module still rebuilds the
streams from the data the blocks decode to, the filter's input, so the files are not the files
themselves; ALZip picks AZO by itself for `.sys` and `.com` files only (alzgg's notes), so it is
rare.

Left: the module giving the filtered data's inverse (the files themselves); ALZip's choice between a match and a literal,
most of the record; speed (5–6 MB/s of data a thread to split and to join, the chains walked
twice where a match may start; a stream's blocks go on up to `threads` threads natively, one
after another in Wasm).

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
