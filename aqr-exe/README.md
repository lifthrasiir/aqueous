# aqr-exe

x86 and x86-64 code recompression: a disfilter splits each executable section of a PE image
into 46 streams by instruction field, with relative targets made absolute, and the section is
rebuilt bit for bit from them.

```sh
cargo run --release -p aqr-exe --example bench -- [-l LEVEL] [--no-zstd] [NAME|PATH ...]  # filter round trip, speed, zstd before and after
build-wasm.bat                                                                       # the decoder as a Wasm module
```

```rust
use aqr_exe::arch::x86;
use aqr_exe::format::pe::locate_code;
let code = locate_code(&mut file)?;                  // the executable sections, PE32 or PE32+
let st = x86::encode(section, va, code.is64).unwrap();
let back = x86::decode(&(&st).into(), code.is64);    // Some(section)
```

`FORMAT.md` specifies the module `exe-recons/x86`, with the opcode tables in full.

## Results

Measured on 2026-10-23 at `aa7a327` on an **AMD Ryzen 7 9700X under Windows 11**, one thread,
not on the Apple M4 of the [top-level README](../README.md#how-it-was-measured): the speeds here
are not comparable to those of the other crates. Otherwise as there, through `aqr-show`: each
file archived at level `mid` with its round-trip check, then extracted. Ratios are tentative,
since the decoder modules are not in the archives yet. Memory is the peak working set of that
process (input, archive and output included) over the input size.

- **sample**: `.exe` and `.dll` files of 256 KiB or more under `C:\Program Files`,
  `C:\Program Files (x86)` and `C:\Windows\System32` (13,459 files, 38.9 GB), taken in the order
  of their paths' hashes, without duplicates, up to about 400 MB.
- **large**: the 11 binaries of `examples/bench.rs` (MinGW GCC, clang-cl, MSVC, rustc, Go;
  x86-64 and two x86-32).

| set | files | MB | ratio | zstd -3 | compress | decompress | memory / input |
|---|---|---|---|---|---|---|---|
| sample | 261 | 400.5 | **0.339** | 0.383 | **16.5 MB/s** | **336 MB/s** | 5.3× (median of 6 files of 8 MB or more); 14 MB + 4.7× |
| sample, coded | 140 | 287.9 | 0.333 | 0.390 | 16.3 MB/s | 271 MB/s | |
| sample, not coded | 121 | 112.6 | 0.357 | 0.363 | 17.0 MB/s | 862 MB/s | |
| large | 11 | 1528.6 | **0.306** | 0.383 | **36.4 MB/s** | **268 MB/s** | 4.7× (4.3–5.2×) |

All files round-trip. The files not coded were kept raw, their formulas archiving no smaller:
108 .NET assemblies (IL; the one mixed-mode assembly was coded), and 13 native files: resource-only
DLLs, three DirectX SDK samples, an ARM64 and an ARM DLL among them.

The filter alone (`examples/bench.rs` on the large set, sections only, zstd -9 before and after):

| | |
|---|---|
| code sections | 1108.6 MB |
| filter | 238 MB/s |
| unfilter | 296 MB/s |
| zstd -9 | 406.1 → 324.7 MB (−20.0%; −14.7% for ffmpeg to −25.4% for tensorflow) |

So the rest of compression (archive coding at `mid` and the check), not the filter, sets its
speed.

## Limitations

- PE only: no ELF or Mach-O.
- x86 and x86-64 only: no ARM64 (or ARM) code. The machine type is not checked, so such images
  are filtered as x86 code, for nothing.
- .NET IL is not modelled.
