# aqr-exe

Machine code recompression, of the executable sections of PE images and Mach-O images (thin or
in a universal file), and of the metadata and IL of .NET images, each rebuilt bit for bit:

- x86 and x86-64: a disfilter splits the code into 46 streams by instruction field, with
  relative targets made absolute.
- ARM64: the instructions stay in one stream of big-endian words, with the targets of `BL`
  and `ADRP` made absolute, and the offsets of local branches taken out of them to streams
  of their own.
- .NET (CIL), the section holding the metadata of an image of IL alone or ReadyToRun: the
  tables by column, sorted columns as deltas; the heaps apart; the IL split by operand into
  26 streams in all, its tokens through a move-to-front cache of each table. The native code
  of a ReadyToRun image of x86-64 (as its runtime functions tell) goes through the x86
  disfilter within it.

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

use aqr_exe::arch::cil;
if let Some(cli) = code.cli {                         // .NET: the section holding the metadata
    let (rva, off, size) = cli.section;
    let st = cil::encode(&file[off as usize..][..size], rva, cli.metadata).unwrap();
    let back = cil::decode(std::array::from_fn(|i| &st.streams()[i][..]), rva, cli.metadata, size);
}

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

`FORMAT.md` specifies the modules `exe-recons/x86`, with the opcode tables in full,
`exe-recons/arm64` and `exe-recons/cil`.

## Results

Measured on 2026-10-24 as in the [file formats overview](../docs/file-formats.md#how-it-was-measured)
(Apple M4, one thread), through `aqr-show`: each file archived at level `mid` with its
round-trip check, then extracted. Ratios are tentative, since the decoder modules were not in
the archives then. Memory is the peak RSS of that process (input, archive and output included)
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
| sample | 259 | 391.3 | **0.326** | 0.381 | **28.4 MB/s** | **414 MB/s** | 26 MB + 7.9× (two files over twice that, below) |
| sample, coded | 220 | 366.3 | 0.328 | 0.387 | 28.3 MB/s | 392 MB/s | |
| sample, .NET coded | 82 | 90.7 | 0.320 | 0.380 | 28.4 MB/s | 465 MB/s | |
| sample, not coded | 39 | 25.1 | 0.292 | 0.298 | 30.2 MB/s | 2013 MB/s | |
| large | 11 | 1528.6 | **0.306** | 0.383 | **58.2 MB/s** | **348 MB/s** | 0.76–1.01 of the sample's fit |

All files round-trip. The files not coded (no `exe-recons`) were kept raw, their formulas
archiving no smaller: 27 .NET assemblies (24 satellite assemblies of resources alone;
`System.Text.Encoding.CodePages.dll`, mostly tables of code
pages; `Microsoft.SqlServer.Types.dll`, whose IL is in another section than its metadata; a
Silverlight NGen image), and 12 native files: resource-only DLLs, three DirectX SDK samples
and an ARM DLL among them. Of the .NET assemblies, those of IL alone come to 0.267 from 0.306
(84 files, 66.1 MB; zstd -3 0.316), and the ReadyToRun ones to 0.374 from 0.432 (24 files,
38.4 MB; zstd -3 0.435): 0.396 with their native code alone through the x86 disfilter, the
rest through `exe-recons/cil`. The rest of an image coded, and what `exe-recons/cil` keeps as
it is, are scanned in turn: 10 PNG files among the resources of 9 files, and an installer
within `oalinst.exe`, are coded (−74 KB in all), at the cost of about 9% of the speed of
compression, most of it spent on zlib and PNG data found that does not pay. The ARM64 DLL
of the sample is coded by `exe-recons/arm64` (0.346).

The archives are those of the first measurement, on an AMD Ryzen 7 9700X under Windows 11 at
`aa7a327`, byte for byte, but for the ARM64 DLL and the .NET assemblies (kept raw then) and
the changed file. That
run gave 16.5 / 336 MB/s on the sample and 36.4 / 268 MB/s on the large set; its peak working
set was smaller, 14 MB + 4.7× on the sample. Most of the difference is the allocator: with
the cache off (`MallocLargeCache=0`), the sample takes 17 MB + 5.3× here (the large set
0.86–1.01 of that), about what the heap holds at its peak, the round-trip check (the input,
the archive, the decoded streams and sections, and the output). With the cache on, two files
kept raw take over twice the fit, from what was tried on them: the Silverlight NGen image
`System.ni.dll`, 106 MB for 0.7 MB, and a printer driver's `HPFIGLHN.DLL`, 136 MB for 5.3 MB;
none does with it off.

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
- .NET: only the section holding the metadata is taken, so IL in other sections is kept as it
  is (one file of the sample). Managed resources are kept as they are (they are most of what
  is left: satellite assemblies, `*.resources.dll`, gain nothing). Of ReadyToRun images, only
  those of x86-64 have their native code filtered; NGen images (`*.ni.dll`) and mixed-mode
  assemblies are not taken.
