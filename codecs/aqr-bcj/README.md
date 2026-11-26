# aqr-bcj

The filters of xz and 7z as modules applying their encoders: the branch converters (BCJ: x86,
PowerPC, IA-64, ARM, ARM Thumb, SPARC, ARM64, RISC-V), Delta, and 7z's BCJ2, Swap2 and Swap4. A stream that went
through a filter before LZMA or LZMA2 is rebuilt from the data before the filter, the files'
own bytes, so that they are stored once where the same files are archived along with it
(aqr-lzma's `.xz` blocks, aqr-7z's folders). `FORMAT.md` specifies the modules, and how they work.

The filters are ported from the LZMA SDK (public domain). BCJ2's encoder is not reproduced: any encoder's
streams are rebuilt (`FORMAT.md`).

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
- That run takes 42.3 s and 4.96 GB at peak (27.8 s and 3.34 GB before); testing it 16.9 s and
  2.27 GB (10.9 s and 2.17 GB). Each folder is one LZMA or LZMA2 stream, rebuilt on one thread.
- The modules are 2.4 to 3.2 KB (`bcj2` 4.4 KB, `bcj-delta`, `bcj-swap2` and `bcj-swap4` 1.9 KB),
  part modules whose memory does not depend on the length of the data.

## Tests

```sh
cargo rt -p aqr-bcj
```

- `tests/filters.rs`: round trips and the modules, BCJ2 against a reference encoder in the test; the
  filters equal the LZMA SDK's C code (built apart) on random and executable bytes, both ways,
  from several start addresses.
- `tests/edge.rs`: damaged and edge inputs. `tests/alloc.rs`: memory bounds.
- The `.xz` fixtures of aqr-lzma and the 7z fixtures of aqr-7z (BCJ2 too) test them as black boxes.

## Left

- **BCJ2 chains.** BCJ2 under other filters is not rebuilt; rare.
