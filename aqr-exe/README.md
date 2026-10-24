# aqr-exe

Machine code recompression, of the executable sections of PE images and Mach-O images (thin or
in a universal file), each rebuilt bit for bit; of .NET ReadyToRun images (x86-64), of their
native code alone, as their runtime functions tell, the metadata and IL around it left out:

- x86 and x86-64: a disfilter splits the code into 46 streams by instruction field, with
  relative targets made absolute.
- ARM64: the instructions stay in one stream of big-endian words, with the targets of `BL`
  and `ADRP` made absolute, and the offsets of local branches taken out of them to streams
  of their own.

```sh
cargo run --release -p aqr-exe --example aqr-exe-bench -- [-l LEVEL] [--no-zstd] [NAME|PATH ...]  # filter round trip, speed, zstd before and after
cargo run --release -p aqr-exe --example aqr-exe-bench-arm64 -- [-l LEVEL] MACHO...              # the same for ARM64 Mach-O images, against xz's BCJ
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

Measured on 2026-10-24 as in the [top-level README](../README.md#how-it-was-measured)
(Apple M4, one thread), through `aqr-show`: each file archived at level `mid` with its
round-trip check, then extracted. Ratios are tentative, since the decoder modules are not in
the archives yet. Memory is the peak RSS of that process (input, archive and output included)
over the input size, with macOS's cache of freed large blocks on, as for every crate. The files are copies, kept in `corpus/` (`manifest.tsv`, with their
hashes and original paths), of those on a Windows 11 machine.

- **sample**: `.exe` and `.dll` files of 256 KiB or more under `C:\Program Files`,
  `C:\Program Files (x86)` and `C:\Windows\System32` (13,459 files, 38.9 GB), taken in the order
  of their paths' hashes, without duplicates, up to about 400 MB. Two of the 261 files taken
  were gone from that machine by the time of the copy (`corpus/missing.tsv`), and one had
  changed.
- **large**: the 11 binaries of `examples/bench.rs` (MinGW GCC, clang-cl, MSVC, rustc, Go;
  x86-64 and two x86-32), found under `corpus/large/` where their Windows paths do not exist.

| set | files | MB | ratio | zstd -3 | compress | decompress | memory / input |
|---|---|---|---|---|---|---|---|
| sample | 259 | 391.3 | **0.335** | 0.381 | **29.3 MB/s** | **465 MB/s** | 9.3× (median of 6 files of 8 MB or more); 27 MB + 7.2× |
| sample, coded | 160 | 313.3 | 0.339 | 0.395 | 31.0 MB/s | 396 MB/s | |
| sample, not coded | 99 | 78.0 | 0.319 | 0.327 | 24.2 MB/s | 1547 MB/s | |
| large | 11 | 1528.6 | **0.306** | 0.383 | **57.7 MB/s** | **347 MB/s** | 6.8× (5.7–9.0×) |

All files round-trip. The files not coded (no `exe-recons`) were kept raw, their formulas
archiving no smaller: 87 .NET assemblies (84 of IL alone, two of them with PNG resources coded;
two ReadyToRun images of x86-32 and ARM64; a Silverlight NGen image), and 12 native files:
resource-only DLLs, three DirectX SDK samples and an ARM DLL among them. The 22 ReadyToRun
images of x86-64 are coded, their native code alone (13.0 MB of 37.7 MB): 0.432 → 0.396, though
one of them loses its PNG resources (the rest of a PE image is not scanned once coded). The
ARM64 DLL of the sample is coded by `exe-recons/arm64` (0.346).

The archives are those of the first measurement, on an AMD Ryzen 7 9700X under Windows 11 at
`aa7a327`, byte for byte, but for the ARM64 DLL and the ReadyToRun images (kept raw then)
and the changed file. That
run gave 16.5 / 336 MB/s on the sample and 36.4 / 268 MB/s on the large set; its peak working
set was smaller, 5.3× (14 MB + 4.7×) and 4.7× (4.3–5.2×). Most of the difference is the
allocator: with the cache off (`MallocLargeCache=0`), the files of 8 MB or more take 6.3×
(sample) and 4.6× (large, 4.5–5.7×) here, about what the heap holds at its peak, the
round-trip check (the input, the archive, the decoded streams and sections, and the output).

The filter alone (`examples/bench.rs` on the large set, sections only, zstd -9 before and after):

| | |
|---|---|
| code sections | 1108.6 MB |
| filter | 292 MB/s |
| unfilter | 336 MB/s |
| zstd -9 | 406.1 → 324.7 MB (−20.0%; −14.7% for ffmpeg to −25.4% for tensorflow) |

So the rest of compression (archive coding at `mid` and the check), not the filter, sets its
speed.

### ARM64

Measured on 2026-10-23 on an Apple M4 with `aqr-exe-bench-arm64` (zstd -9, each stream on its own;
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
- .NET IL and metadata are not modelled. Of ReadyToRun images, only those of x86-64 have
  their native code filtered; NGen images (`*.ni.dll`) are kept as they are.
