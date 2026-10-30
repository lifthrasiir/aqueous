# aqr-bcj

The filters of xz and 7z as modules applying their encoders: the branch converters (BCJ: x86,
PowerPC, IA-64, ARM, ARM Thumb, SPARC, ARM64, RISC-V), Delta, and 7z's BCJ2. A stream that went
through a filter before LZMA or LZMA2 is rebuilt from the data before the filter, the files'
own bytes, so that they are stored once where the same files are archived along with it
(aqr-lzma's `.xz` blocks, aqr-7z's folders). `FORMAT.md` specifies the modules.

The filters are ported from the LZMA SDK (public domain). BCJ2's encoder is not reproduced: its
module reads the bits of conversion from the archive's own range coder stream, kept as it is,
as the decoder does, so any encoder's streams are rebuilt.

```sh
cargo run --release -p aqr-bcj --example aqr-bcj-filter -- ARCH|delta ENC(0/1) PC|DIST IN OUT
```

```rust
use aqr_bcj::formula::{self, Filter};
let f = Filter::from_xz(id, &props).unwrap();          // or Filter::from_7z(coder id, props)
let data = formula::unfilter(&[f], &filtered).unwrap(); // None where encoding does not give it back
let node = formula::apply(&[f], data_node, len);        // the module applied to the data
```

## Results

Measured on 2026-10-30 on an Apple M4 (10 cores), through `aqr` (all threads, level mid; the
commit before as the baseline).

- The 10gb benchmark's `mingw480` (411.0 MB: a tree of 8,344 files and a 7z archive of them,
  whose second folder, 13.58 MB, is BCJ2 over LZMA2 and LZMA) archives into 68.79 -> 55.36 MB;
  the tree alone into 47.36 MB, so the archive costs 8.0 MB beside it, of which 6.51 MB are
  the LZMA and LZMA2 records. The whole archive is rebuilt (71 bytes kept), its 374.9 MB of
  data being the files'.
- That run takes 27.8 -> 42.3 s and 3.34 -> 4.96 GB at peak; testing it 10.9 -> 16.9 s and
  2.17 -> 2.27 GB. Each folder is one LZMA or LZMA2 stream (250 and 124 MB of data), split and
  rebuilt on one thread; the times look like the two are rebuilt one after the other (not
  looked into).
- The modules are 9.0 to 9.9 KB (`bcj2` 12 KB), of module blocks of 3.6 to 5.1 KB.

## Tests

```sh
cargo rt -p aqr-bcj   # round trips, the modules, BCJ2 against a reference encoder in the test
```

The filters were compared with the LZMA SDK's C code (built apart) on random bytes biased to
what they look for and on executables, both ways, from several start addresses: equal. The
`.xz` fixtures of aqr-lzma (XZ Utils' filters) and the 7z fixtures of aqr-7z (7-Zip's, BCJ2
too) test them as black boxes.

## Status

Done: the modules, natively and in Wasm, and their use by aqr-lzma (`.xz` blocks of filters
before LZMA2) and aqr-7z (folders of filters over LZMA, LZMA2 or Copy, and BCJ2 over them).
Left: 7z's Swap2/Swap4 filters and chains of BCJ2 under other filters, both rare.
