# aqr-exe

Machine code recompression, of the executable sections of PE images and Mach-O images (thin or
in a universal file), each rebuilt bit for bit:

- x86 and x86-64: a disfilter splits the code into 46 streams by instruction field, with
  relative targets made absolute.
- ARM64: the instructions stay in one stream of big-endian words, with the targets of `BL`
  and `ADRP` made absolute, and the offsets of local branches taken out of them to streams
  of their own.

```sh
cargo run --release -p aqr-exe --example bench -- [-l LEVEL] [--no-zstd] [NAME|PATH ...]  # filter round trip, speed, zstd before and after
cargo run --release -p aqr-exe --example bench-arm64 -- [-l LEVEL] MACHO...              # the same for ARM64 Mach-O images, against xz's BCJ
build-wasm.bat                                                                       # the decoder as a Wasm module
```

```rust
use aqr_exe::arch::x86;
use aqr_exe::format::pe::locate_code;
let code = locate_code(&mut file)?;                  // the executable sections, PE32 or PE32+
let st = x86::encode(section, va, code.is64).unwrap();
let back = x86::decode(&(&st).into(), code.is64);    // Some(section)

use aqr_exe::arch::arm64;
use aqr_exe::format::macho;
for image in macho::images(&file).unwrap_or_default() {  // each architecture of a universal file
    for &(va, off, size) in &image.sections {         // the sections with instructions
        if image.cputype == macho::CPU_TYPE_ARM64 {
            let st = arm64::encode(&file[off..off + size], va);
            let back = arm64::decode(std::array::from_fn(|i| &st.streams()[i][..]), va, size);
        }
    }
}
```

`FORMAT.md` specifies the modules `exe-recons/x86`, with the opcode tables in full, and
`exe-recons/arm64`.

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

### ARM64

Measured on 2026-10-23 on an Apple M4 with `bench-arm64` (zstd -9, each stream on its own;
filter speeds on one thread): the code sections of the ARM64 images of 12 Mach-O files,
298.4 MB (VS Code's Electron Framework, `code-tunnel` and `rg` in Rust, `vsce-sign` in Go,
ANGLE, SwiftShader, FFmpeg, SQLite, MSAL; Homebrew's libLLVM and libclang-cpp).

| | MB | |
|---|---|---|
| zstd -9 | 114.6 | |
| xz's ARM64 BCJ (`BL`, `ADRP`), then zstd -9 | 100.7 | −12.1% |
| this filter, then zstd -9 | 94.5 | **−17.6%** (−7.4% for FFmpeg to −32.8% for MSAL) |
| filter, unfilter | | 1604 and 1289 MB/s |

Through `aqr-show` (level `mid`, 10 threads), VS Code's 34 universal binaries (493 MB) come
to 163.2 MB, against 196.0 MB as they are (−16.7%) and 179.1 MB with their x86-64 halves
alone filtered; VS Code's ZIP (267 MB) to 222.2 MB.

## Limitations

- PE and Mach-O only: no ELF.
- x86, x86-64 and ARM64 only: images of other machines (32-bit ARM, PowerPC) are kept as
  they are. ARM64 data in code sections (literal pools) is taken as instructions.
- Mach-O code signatures are kept as they are, though their page hashes follow from the
  image.
- .NET IL is not modelled.
