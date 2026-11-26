# aqr-exe

Machine code recompression, of the executable sections of PE images, Mach-O images (thin or in a
universal file) and ELF images, and of the metadata and IL of .NET images, each rebuilt bit for bit.
`FORMAT.md` specifies the modules `exe-recons/x86` (with the opcode tables in full), `x86-remap`,
`arm64`, `arm64-remap`, `arm64-delta`, `cil`, `pe-reloc`, `macho-symtab` and `macho-codesign`, and
how they work.

```sh
cargo run --release -p aqr-exe --example aqr-exe-bench -- [-l LEVEL] [--no-zstd] [NAME|PATH ...]  # filter round trip, speed, zstd before and after
cargo run --release -p aqr-exe --example aqr-exe-bench-arm64 -- [-l LEVEL] MACHO...              # the same for ARM64 Mach-O images, against xz's BCJ
cargo run --release -p aqr-exe --example aqr-exe-bench-remap -- [-x] A B                          # B's ARM64 (x86-64) code remapped to A's, against A's
cargo run --release -p aqr-exe --example aqr-exe-bench-delta -- ...                              # ARM64 code against another version, by differences
cargo run --release -p aqr-exe --example aqr-exe-bench-codesign -- ...                           # Mach-O code signatures' page hashes
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

use aqr_exe::format::elf;
if let Some(image) = elf::image(&file) {                // EM_386, EM_X86_64, EM_AARCH64, …
    for &(va, off, size) in &image.code { /* as Mach-O's sections */ }
}
```

## What is taken

- **x86 and x86-64** (PE, Mach-O, ELF): the code in 14 streams by instruction field (a disfilter),
  its relative targets made absolute; for PE, the base relocations predicted from the code and
  the data (`pe-reloc`), and the export, import and runtime function tables by aqr-infer-recs'
  `recs-fields`.
- **ARM64** (Mach-O, ELF): the instructions as big-endian words, `BL` and `ADRP` targets made
  absolute, the offsets of local branches apart.
- **.NET** (CIL, of IL alone or ReadyToRun): the metadata's tables by column, the heaps apart, the IL
  split by operand (26 streams in all); the native code of a ReadyToRun image of x86-64 through the
  x86 disfilter.
- **Mach-O**: the 64-bit symbol and string tables as `ld64` writes them (16 KiB or more together),
  the page hashes of code signatures derived, and the `__const` tables of pointers apart.
- **ELF**: 32- or 64-bit, little-endian (executables, shared objects, relocatable objects); the code
  of its allocated sections of instructions, or of the executable segments of an image without
  section headers. Its tables (relocations, symbols, `.eh_frame`, `.gnu.hash`) are left to the scan
  of the rest. DWARF is aqr-dwarf's.
- **Code against another version** (Mach-O and ELF images archived together, of more than half a
  solid block of code each): ARM64 and x86 code remapped to the other's targets (`arm64::remap`,
  `x86::remap`; after Courgette), and ARM64 code of an image's largest section coded as differences
  against its tree's root (`arm64::delta`).
- Not taken: other machines' code (32-bit ARM, PowerPC) and big-endian ELF, kept as they are.

## Results

Measured on 2026-10-24 on an Apple M4 (one thread), `aqr-show` at level `mid` (round-trip check
included), the files being copies of those of a Windows 11 machine (`corpus/`, not in git;
`manifest.tsv`); later versions of x86 are measured below.

- **sample**: `.exe` and `.dll` files of 256 KiB or more under `C:\Program Files`,
  `C:\Program Files (x86)` and `C:\Windows\System32`, taken by their paths' hashes up to about
  400 MB.
- **large**: the 11 binaries of `examples/bench.rs` (MinGW GCC, clang-cl, MSVC, rustc, Go; x86-64
  and two x86-32).

| set | files | MB | ratio | zstd -3 | compress | decompress |
|---|---|---|---|---|---|---|
| sample | 259 | 391.3 | **0.326** | 0.381 | 28.4 MB/s | 414 MB/s |
| sample, .NET coded | 82 | 90.7 | 0.320 | 0.380 | 28.4 MB/s | 465 MB/s |
| large | 11 | 1528.6 | **0.306** | 0.383 | 58.2 MB/s | 348 MB/s |

Memory is about 26 MB + 7.9x the input (the cache of freed large blocks on). Of .NET assemblies,
those of IL alone come to 0.267 from 0.306 (84 files, 66.1 MB), the ReadyToRun ones to 0.374 from
0.432 (24 files, 38.4 MB). The filter alone is 292 MB/s, the unfilter 336 MB/s; zstd -9 of the
code sections of `large` (1108.6 MB) goes 406.1 -> 324.7 MB (-20.0%).

Later changes of x86 (Ryzen 7 9700X, `aqr -c` at the default level; the same files):

- Version 2 (fields the opcodes tell within `OP`, 8-bit displacements by their base register,
  padding records): sample -1.7%, large -2.4%.
- Version 5 (jumps as numbers of steps): the 11 binaries of `large` 421.5 -> 414.2 MB (-1.74%);
  AcroRd32.dll 7.68 -> 7.62 MB (-0.75%).
- Version 6 with `pe-reloc` 2 (base relocations predicted from the code): AcroRd32.dll -1.30%,
  `large` -0.46% (its x86-32 files -2.46% and -0.84%); `pe-reloc` 3 (data's pointers too): AcroRd32.dll
  a further -23 KB, chrome.dll -84 KB, libcef32 -89 KB.

### ARM64 (Apple M4, 2026-10-23)

The code sections of the ARM64 images of 12 Mach-O files, 298.4 MB (Electron Framework, Rust and Go
programs, ANGLE, SwiftShader, FFmpeg, SQLite, libLLVM): zstd -9 114.6 MB; xz's ARM64 BCJ then
zstd -9 100.7 MB (-12.1%); this filter then zstd -9 94.5 MB (-17.6%; FFmpeg -7.4% to MSAL
-32.8%), at 1604 MB/s filtering and 1289 MB/s back. VS Code's 34 universal binaries (493 MB) come
to 163.2 MB against 196.0 MB as they are (-16.7%).

### ELF (Apple M4, 2026-11-01)

22 files, 498.7 MB, of Ubuntu 24.04 for ARM64 and x86-64 (LLVM 18, libclang-cpp, rustc's
`librustc_driver`, Python, Perl, git, glibc, libstdc++, ripgrep, Go, Node.js), each alone through
`aqr -c` (`mid`): 146.90 -> 123.16 MB (**-16.2%**); `librustc_driver` -20.6%, `libclang-cpp` -21.9%,
`libLLVM` -16.9%, `node` -11.1%. Compressing takes 1.9x the instructions (peak 990 MB for
`libLLVM`). Two versions of `librustc_driver` together: 72.42 -> 58.16 MB (-19.7%).

### Mach-O symbol tables, signatures and tables of pointers (Apple M4, 2026-10-30 to 11-03, `aqr -c` at `mid`)

| | before | after | |
|---|---|---|---|
| symbol tables: Windows App's main binary, arm64 | 11.42 MB | 10.73 MB | -6.0% |
| symbol tables: Deskflow.app | 19.81 MB | 18.62 MB | -6.0% |
| code signatures: the seven applications of `docs/macos-app-todo.md` | 1080.60 MB | 1053.10 MB | -2.5% |
| tables of pointers (against the root's, and apart): the same seven | 1053.10 MB | 1039.19 MB | -1.3% |

A code signature's page hashes (8.67 MB of signatures at zstd -19) are 0.053 MB with them derived,
recomputed at 2.5 GB/s. Tables of pointers: Discord -4.6%, Google Chrome -1.3%.

### Code against another version (Apple M4, 2026-10-30 to 11-03, `aqr -c` at `mid`)

| | before | after | |
|---|---|---|---|
| Chrome Framework's three arm64 images (755.1 MB), remapped | 141.91 MB | 119.64 MB | -15.7% |
| Chrome Framework's three x86-64 images (823.7 MB), remapped | 136.82 MB | 122.78 MB | -10.3% |
| Chrome Framework .47 and .49, arm64, by differences | 92.04 MB | 88.71 MB | -3.6% |
| Chrome Framework .49 and .76, arm64, by differences | 103.22 MB | 96.04 MB | -7.0% |
| Electron Framework 43 and 44, arm64, by differences | 111.19 MB | 97.75 MB | -12.1% |
| Google Chrome.app (2.28 GB, three versions of its framework), by differences | 367.19 MB | 352.75 MB | -3.9% |

Of B's code stream against A's (zstd -7 `--patch-from` against the remap): Chrome Framework .47 ->
.49 arm64 33.73 -> 8.18 MB, x86-64 9.20 -> 1.25 MB; Electron 42 -> 43 arm64 36.72 -> 29.48 MB. By
differences, Chrome's first pair is 0.87 MB against bsdiff's 1.01 MB and Zucchini's 0.93 MB.
Remapped ARM64 code encodes at about 900 MB/s and decodes at 700-800 MB/s (x86: 170 and 270 MB/s);
the differences decode at 0.9 to 3.1 GB/s. Compressing Chrome.app takes 1.0x the instructions with
remaps and differences together; testing a three-version tree takes 1.1-1.2x.

## Tests

```sh
cargo rt -p aqr-exe
```

- `tests/part.rs`: the part modules' streaming against whole decoding. `tests/alloc.rs`: memory
  bounds of the modules.
- `tests/edge.rs`: PE, ELF and Mach-O images damaged or odd (headers, `SHN_XINDEX`, truncated
  sections). `tests/edge_codecs.rs`: the same for the codecs' streams.
- aqr-decompress's `tests/wasm.rs` checks the plans of `exe-recons/x86`, `arm64`, `macho-codesign`
  and `cil` to the page.

## Left

- **ELF tables.** Only little-endian images are taken; relocations, symbols, `.eh_frame` and its
  header's table, `.gnu.hash` are left in the rest. Compressed sections (`SHF_COMPRESSED`) go to
  the scan, which finds their zlib streams.
- **Other machines.** 32-bit ARM, PowerPC and other code is kept as it is. ARM64 data in code
  sections (literal pools) is taken as instructions.
- **Code against another version, PE.** Only Mach-O and ELF images are remapped, and only of files
  in one batch (two large files of no name or size alike, as Electron Frameworks of two apps, are
  not) and of more than half a solid block of code; shorter streams go one after another, rarely by
  their parents' (Blender's `_ssl` against `_hashlib`, both linking OpenSSL, 11 KB more).
- **Differences of x86 code and of non-ARM64.** `arm64-delta` takes only ARM64 code of an image's
  largest section against its tree's root (not its parent); the low 12 bits that follow an `ADRP`
  are not remapped (the delta predicts them). What is left of B's share is mostly not code (Chrome's
  first pair: 4.1 MB of the rest of the file at zstd -9, 1.8 MB of A's stream in pieces).
- **Mach-O code signatures.** Only the code slots are derived; the special slots (requirements,
  entitlements, `Info.plist`, `CodeResources`) and `_CodeSignature/CodeResources` are kept.
- **Mach-O symbol tables.** Only as `ld64` writes them (730 of the 775 of 16 KiB or more under
  `/Applications`, `/usr/bin` and the like of a Mac on 2026-10-30): not lld's string tables, whose
  strings share their tails (Chromium, Electron, Rust binaries), nor strings in another order
  (WebRTC's) or holding other data (a .NET single-file bundle), nor 32-bit images'.
- **.NET.** Only the section holding the metadata, so IL in other sections is kept (one file of
  the sample); managed resources are kept (most of what is left of satellite assemblies, which
  gain nothing); only ReadyToRun images of x86-64 have their native code filtered; NGen images
  (`*.ni.dll`) and mixed-mode assemblies are not taken.
- **PE sections over 48 MiB.** `pe-reloc` takes no data sections of more than 48 MiB in all
  (rustc_driver's 76 MB `.rdata` is left).
