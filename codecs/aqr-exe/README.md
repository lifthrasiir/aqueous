# aqr-exe

Machine code recompression, of the executable sections of PE images, Mach-O images (thin or
in a universal file) and ELF images, and of the metadata and IL of .NET images, each rebuilt
bit for bit:

- x86 and x86-64: a disfilter splits the code into 13 streams by instruction field, with
  relative targets made absolute and the padding between functions coded.
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
cargo run --release -p aqr-exe --example aqr-exe-bench-remap -- [-x] A B                          # B's ARM64 (x86-64) code remapped to A's, against A's
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

Of a PE image, the tables of known layouts outside the code are taken too: the base
relocations by a module of their own (the entries' RVAs as deltas, the blocks' headers derived),
and the export, import and runtime function tables by aqr-infer-recs' `recs-fields`, as deltas.

Of a 64-bit Mach-O image, the symbol table and its string table are taken by a module of
their own (16 KiB at least together): the entries by field, their values predicted from the
debug map's stabs and from each other, their names as references to the strings, and the
strings each once, front coded, in the order `ld64` writes them.

Of an ELF image (32- or 64-bit, little-endian; executables, shared objects and relocatable
objects), the code is that of its allocated sections of instructions, those that follow one
another but for padding taken as one range (`.init`, `.plt`, `.text`, `.fini`), and of an
image without section headers that of its executable segments. Its tables are left in the
rest (below).

```rust
use aqr_exe::format::elf;
if let Some(image) = elf::image(&file) {                // EM_386, EM_X86_64, EM_AARCH64, …
    for &(va, off, size) in &image.code { /* as Mach-O's sections */ }
}
```

ARM64 and x86 code can be coded against another version of it (`arm64::remap`,
`x86::remap`, after Courgette; `matching` matches the functions of either). Of ARM64: the
functions of the two are matched (by the hashes of their words with the `BL` and `ADRP` fields
cleared, then by the targets matched so far and their neighbours' shifts), and the targets and
pages of the matched vote for a map of the image's targets to the other's. The `BL` and `ADRP`
the map takes are written with the other's targets, so that code that moved matches the
other's, with a flag for each and the shifts of the values so written; the module needs nothing
of the other image. Of x86, the same of the targets of `CALL rel32` and of other jumps of 32-bit
offsets and of RIP-relative addresses, by byte; the decoder notes where the flagged ones are
as it decodes and adds their shifts once the map is read. aqr-compress remaps the images of the
Mach-O and ELF files archived together alike, of more than half a solid block of code each.

`FORMAT.md` specifies the modules `exe-recons/x86`, with the opcode tables in full,
`exe-recons/x86-remap`, `exe-recons/arm64`, `exe-recons/arm64-remap`, `exe-recons/cil`,
`exe-recons/pe-reloc` and `exe-recons/macho-symtab`.

## Results

Measured on 2026-10-24 as in the [file formats overview](../../docs/file-formats.md#how-it-was-measured)
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

The table is of `exe-recons/x86@1`, 46 streams. Its 13 streams of version 2 (`REX` and
other fields that the opcodes tell, or rare ones, within `OP`; 8-bit displacements by their
base register; padding records coded) were measured on 2026-10-28 on a Ryzen 7 9700X through
`aqr-show -t 16` against version 1 at once, the decoder modules in the archives: sample
0.3215 → 0.3160 (−1.7%; its 150 files of x86 code −2.1%, those under 1 MB −2.5%), large
0.2901 → 0.2832 (−2.4%), 3 files larger by 0.3–1.3% (x86-32). Version 3 takes the opcode
classes as its resource (1280 bytes), so that opcodes can be added without another version:
582 B more for an archive of one file, 248 B of them the resource block.

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

### Mach-O symbol tables

Measured on 2026-10-30 on an Apple M4 through `aqr -c` (level `mid`), before and after
`exe-recons/macho-symtab`:

| | before | after | |
|---|---|---|---|
| Windows App's main binary, arm64 image (56.0 MB; symbols 7.10 MB, strings 14.91 MB) | 11.42 MB | 10.73 MB | −6.0% |
| Secretive's main binary, arm64 image (2.37 MB) | 427.5 KB | 412.4 KB | −3.5% |
| Secretive.app | 3.23 MB | 2.94 MB | −9.0% |
| Deskflow.app | 19.81 MB | 18.62 MB | −6.0% |
| Windows App.app | 109.33 MB | 107.20 MB | −1.9% |
| Blender.app | 242.04 MB | 237.31 MB | −2.0% |

Instructions retired: Windows App 115.9 → 117.5 G (+1.3%); Secretive, Deskflow and Blender
fewer (29.9 → 20.9, 73.8 → 71.0, 675.3 → 649.9 G), what was scanned and inferred in the rest
taken by the module. The module block is 5.8 KB.

### ELF

Measured on 2026-11-01 on an Apple M4 through `aqr -c` (level `mid`, 10 threads), each file
archived alone, before (the whole file taken by aqr-infer-recs, as data of no format) and
after: 22 files, 498.7 MB, of Ubuntu 24.04 for ARM64 (LLVM 18, libclang-cpp, rustc 1.96.1's
`librustc_driver`, Python, Perl, git, vim, bash, gold, glibc, libstdc++, OpenSSL, zlib, `ls`)
and for x86-64 (glibc, libstdc++, libasan; ripgrep 14.1.1 of musl, Go 1.23.3's `go` and
`compile`, Node.js 22.11.0).

| | before | after | |
|---|---|---|---|
| all | 146.90 MB | 123.16 MB | **−16.2%** |
| `libLLVM.so.18.1`, ARM64 (119.2 MB) | 31.13 MB | 25.87 MB | −16.9% |
| `librustc_driver`, ARM64 (111.4 MB) | 35.14 MB | 27.90 MB | −20.6% |
| `node`, x86-64 (117.4 MB) | 35.40 MB | 31.47 MB | −11.1% |
| `libclang-cpp.so.18.1`, ARM64 (65.3 MB) | 15.56 MB | 12.15 MB | −21.9% |
| `go`, `compile`, x86-64 (33.2 MB) | 11.26 MB | 9.86 MB | −12.5% |
| `rg`, x86-64 (6.6 MB) | 2.06 MB | 1.76 MB | −14.2% |
| glibc, ARM64 and x86-64 (3.8 MB) | 1.70 MB | 1.49 MB | −12.6% |

`libz.so.1` (133 KB) archives as before and `ls` (199 KB) 2.1% smaller, `python3.12` 5.3%;
the other 19 files 10.6–21.9%. Instructions retired: 169.5 → 325.3 G (+92%,
the code's streams coded densely, as of PE), peak RSS 735 → 990 MB (`libLLVM.so.18.1`, 8.3×
its size). Two versions of `librustc_driver` (1.96.1 and 1.99.1, 215.6 MB) together: 72.42
→ 58.16 MB (−19.7%); their code is too little alike (0.06) to be remapped.

Taking `.eh_frame_hdr`'s table and the relocations (`.rela.dyn`) by `recs-fields` was tried
and left out: the relocations (as deltas, or the offsets alone as deltas) archive larger than
in the rest, and the table's effect is that of cutting the rest elsewhere (+2.2% to −2.8% a
file, not by the table's size).

### Code against another version

Measured on 2026-10-30 on an Apple M4. With `aqr-exe-bench-remap`, B's `CODE` stream
compressed against A's (`zstd -7 --long=28 --patch-from`), as it is and remapped (the flags and
maps with zstd -19 added):

| A → B | alone | against A | remapped | |
|---|---|---|---|---|
| Chrome Framework 156.0.8078.47 → .49, arm64 | 64.13 MB | 33.73 MB | 8.18 MB | −75.8% |
| Chrome Framework .49 → .76 | 64.15 MB | 45.88 MB | 20.35 MB | −55.6% |
| Electron Framework 42 (Discord) → 43 (VS Code) | 44.01 MB | 36.72 MB | 29.48 MB | −19.7% |
| aqr built 10-27 (`880665b`) → 10-28 (`9e610a7`) | 3.57 MB | 2.05 MB | 1.38 MB | −32.7% |

The map takes 1–2 s for Chrome's pair; remapped code encodes at about 900 MB/s and decodes at
700–800 MB/s (the plain filter: 1604 and 1289). Through `aqr -c` (level `mid`, 10 threads),
before (the remap left out) and after:

| | before | after | |
|---|---|---|---|
| Chrome Framework's three arm64 images (755.1 MB) | 141.91 MB | 119.64 MB | −15.7% |
| Google Chrome.app (2.28 GB, the three versions of its framework) | 410.70 MB | 388.10 MB | −5.5% |
| aqr built 10-27, 10-28 and 10-29 (62.0 MB) | 13.47 MB | 12.95 MB | −3.8% |
| the same, pairs | 9.52, 10.17, 9.94 MB | 9.16, 9.76, 9.47 MB | −3.7%, −4.0%, −4.7% |

Instructions retired: the three images 323.5 → 358.0 G (+10.7%), Chrome.app 1327.1 → 1328.6 G,
aqr's builds 31.1 → 32.8 G; peak RSS 3.22 → 3.54 GB for the three images. Secretive, Deskflow,
Windows App, Discord, Blender and Pages have no images alike of that much code, and archive as
before. The module block is 6.0 KB.

Of x86-64, measured the same way on 2026-10-30 (`aqr-exe-bench-remap -x`: each of B's 13
streams against A's):

| A → B | alone | against A | remapped | |
|---|---|---|---|---|
| Chrome Framework 156.0.8078.47 → .49, x86-64 | 78.19 MB | 9.20 MB | 1.25 MB | −86.5% |
| Chrome Framework .49 → .76 | 78.36 MB | 21.98 MB | 15.46 MB | −29.7% |
| Electron Framework 42 → 43 | 53.85 MB | 32.20 MB | 31.53 MB | −2.1% |

The gains are in `JUMP`, `ADDR` and `CALL`; of Electron, whose code changed more than it moved,
the other streams (`OP` 15.7 MB) stay as they were. Remapped code encodes at about 170 MB/s
and decodes at 270 MB/s (the disfilter alone: 292 and 336). Through `aqr -c`, before (the x86
images left out of the plan) and after:

| | before | after | |
|---|---|---|---|
| Chrome Framework's three x86-64 images (823.7 MB) | 136.82 MB | 122.78 MB | −10.3% |
| Google Chrome.app | 388.01 MB | 373.37 MB | −3.8% |

Instructions retired: the three images 446.6 → 524.4 G (+17.4%), Chrome.app 1320.5 → 1421.2 G
(+7.6%); peak RSS 3.58 → 3.69 GB and 5.71 → 6.29 GB, the x86 images parsed once for the plan
and held through it. Discord and Windows App archive as before. The module block is 8.9 KB.
`exe-recons/x86`'s bytes changed (its decoder now takes a hook, of no effect there), so that
archives of an older one run it as WebAssembly.

### Code against another version, by differences

Measured on 2026-11-03 on an Apple M4. B's remapped `CODE` stream of the arm64 images, against
A's (`aqr-exe-bench-delta`; the streams zstd -19), as LZ does it (`zstd -7 --long=31
--patch-from`), bsdiff 4.3, Zucchini (Firefox's, raw, its patch through `xz -9`) and
`exe-recons/arm64-delta` (`arm64::delta`, B in pieces of 4 MiB each against one of 16 MiB of
A, as aqr-compress codes them):

| A → B | LZ | bsdiff | Zucchini | arm64-delta, whole | in pieces |
|---|---|---|---|---|---|
| Chrome Framework 156.0.8078.47 → .49 | 8.10 MB | 1.01 MB | 0.93 MB | 0.87 MB | 0.86 MB |
| Chrome Framework .49 → .76 | 19.99 MB | 4.97 MB | 4.41 MB | 4.88 MB | 5.07 MB |
| Electron Framework 43 (VS Code) → 44 (Discord) | 28.96 MB | 16.01 MB | 12.79 MB | 14.14 MB | 16.01 MB |

Most of what LZ leaves is where the words differ (1.4% of those copied), not what they are:
the differences of the words alike are 0, and the others go sparse. A word of A that became
another in B mostly becomes it again (data moved within a page changes the low bits of every
`ADD` and `LDR` after an `ADRP` to it alike): predicted so, the differences not 0 drop from
726,440 to 262,549 of Chrome's first pair. Pieces of B against three of 4 MiB of A make
Electron 14.96 MB, against two of 8 MiB 14.97 MB, against one of 8 MiB 23.50 MB (the
prototype's, `aqr-exe-bench-delta -p`); but A's stream in pieces is larger (at zstd -9, by
6.1%, 2.9% and 1.9% in pieces of 4, 8 and 16 MiB), and through `aqr -c` one of 16 MiB archives
smallest but for Electron (below). The differences decode at 0.9 to 3.1 GB/s; the streams are
made at 80 to 210 MB/s a thread, picking A's pieces 0.2 to 0.4 s.

Through `aqr -c` (level `mid`, 10 threads), the arm64 images of each pair, and Chrome's
application (its framework's three versions), before (commit `511681e`) and after:

| | before | after | |
|---|---|---|---|
| Chrome Framework .47 and .49, arm64 (503.4 MB) | 92.04 MB | 88.71 MB | −3.6% |
| Chrome Framework .49 and .76, arm64 | 103.22 MB | 96.04 MB | −7.0% |
| Electron Framework 43 and 44, arm64 (386.1 MB) | 111.19 MB | 97.75 MB | −12.1% |
| Google Chrome.app (2.28 GB) | 367.19 MB | 352.75 MB | −3.9% |

Against two pieces of 8 MiB of A, the pairs were 90.06, 97.29 and 97.35 MB; against three of
4 MiB the first was 91.56 MB. Of Chrome.app, instructions retired 1439.4 → 1191.8 G (less of
the code compressed), 27.9 → 27.7 s, peak RSS 6.22 GB as before; `aqr -t` 4.85–5.33 →
4.71–4.79 s, 86.6 → 90.3 G instructions, peak RSS 3.24 → 2.80 GB. The other six applications
of `docs/macos-app-todo.md` archive as before (to a few bytes). The module block is 2.9 KB.

What is left of B's share is mostly not code: of Chrome's first pair (10.58 MB through
`aqr -c` before the pieces of 16 MiB), the rest of B's file (`__TEXT,__const`, strings,
`__LINKEDIT`) 4.1 MB at zstd -9, A's code stream in pieces 1.8 MB more than whole; Zucchini's
patch of the whole file is 3.43 MB (`docs/macos-app-todo.md`, appendix A).

### Mach-O code signatures

Measured on 2026-11-03 on an Apple M4. The page hashes of a code signature's CodeDirectories
follow from the image (`exe-recons/macho-codesign`). `aqr-exe-bench-codesign` on seven files
(Chrome Framework 156.0.8078.47, thin and universal; Secretive, Pages, Calculator, `/bin/ls`,
Electron Framework 44): every signature had them so, 8.67 MB of signatures at zstd -19 are
0.053 MB with them zeroed; the hashes are computed again at 2.5 GB/s (sha2 with ARMv8's
instructions; 590 MB/s without).

Through `aqr -c` (level `mid`, 10 threads), the applications of `docs/macos-app-todo.md`,
before (commit `40205d3`) and after:

| | before | after | |
|---|---|---|---|
| Secretive.app | 2.86 MB | 2.81 MB | −1.7% |
| Deskflow.app | 18.63 MB | 18.14 MB | −2.6% |
| Windows App.app | 107.02 MB | 104.64 MB | −2.2% |
| Discord.app | 170.57 MB | 168.44 MB | −1.3% |
| Pages.app | 202.23 MB | 198.53 MB | −1.8% |
| Blender.app | 233.13 MB | 227.85 MB | −2.3% |
| Google Chrome.app | 346.15 MB | 332.68 MB | −3.9% |
| all | 1080.60 MB | 1053.10 MB | −2.5% |

Instructions retired 3252.8 → 3285.6 G (+1.0%). The file is put together whole before its
pages are hashed: `aqr -t` of Pages.app 46.6 → 52.0 G instructions (+11.5%), peak RSS
0.58–0.63 → 0.75–0.84 GB (the time within its noise, 0.74–1.34 s either way); of Google
Chrome.app 90.0 → 93.5 G (+3.8%), 2.79 → 2.63 GB. The module block is 10.1 KB.

Taking the signatures out made the rest of Deskflow's two programs, put in one stream, 8.3 MB
rather than 9.5 MB, below the 8 MiB from which aqr-formula then matched from afar, and the
application 2.1% larger: the threshold is 1 MiB since (`aqr::LONG`).

### Tables of pointers

Measured on 2026-11-03 on an Apple M4, through `aqr -c` (level `mid`, 10 threads). A Mach-O
image's `__DATA_CONST,__const`, `__DATA,__const` and `__AUTH_CONST,__const` (pointers and
such, of 4 or 8 bytes a value) archive smaller on their own than amid the rest of the file: of
Electron Framework 43 and 44 (arm64), zeroing them made the two 95.18 → 90.69 MB, though they
are 1.02 and 1.03 MB at zstd -19 apart. Of the images of a tree they are coded against the
root's too (`exe-recons/arm64-delta`), as the code is:

| | before (commit `bab67ef`) | against the root | and apart in every image |
|---|---|---|---|
| Chrome Framework .47 + .49, arm64 | 83.38 MB | 82.04 MB | |
| Chrome Framework .49 + .76, arm64 | 91.60 MB | 90.56 MB | |
| Electron Framework 43 + 44, arm64 | 95.18 MB | 90.37 MB | |
| the seven applications | 1053.10 MB | 1048.17 MB | 1039.19 MB (−1.3%) |

Of the applications: Discord 168.44 → 160.67 MB (−4.6%), Google Chrome 332.68 → 328.24 MB
(−1.3%), Windows App −0.9%, Secretive −0.8%, Pages −0.5%; Deskflow and Blender +0.1% (also
with tables of 16 KiB or more taken alone). Instructions retired 3285.5 → 3280.4 G.

Against the root's, by `exe-recons/arm64-delta`, Chrome's `__DATA_CONST,__const` of .49 is
0.44 MB (LZ against .47's 0.92 MB, bsdiff 0.25 MB): bsdiff copies at any byte, where words
inserted 2 bytes apart are new to whole words.

## Limitations

- ELF: only little-endian images. Their tables (relocations, symbols, `.eh_frame` and its
  header's table, which follows from it, `.gnu.hash`) are left in the rest. DWARF (`.debug_*`
  of ELF and Mach-O images, as of dSYMs) belongs to aqr-dwarf, as PDB files would to a
  recompressor of their own for PE: so far it takes `.debug_info` and Apple's accelerator
  tables (aqr-dwarf's README). Compressed sections (`SHF_COMPRESSED`) are left to the scan of
  the rest, which finds their zlib streams.
- x86, x86-64 and ARM64 only: images of other machines (32-bit ARM, PowerPC) are kept as
  they are. ARM64 data in code sections (literal pools) is taken as instructions.
- Code against another version: only of Mach-O and ELF images, not PE; of files in one batch
  (two large files of no name or size alike, as Electron Frameworks of two apps, are not); and
  only images of more than half a solid block of code, whose streams are cut
  into pieces put with those alike: shorter streams go one after another as their files do,
  rarely by their parents', where remapping them costs more (Blender's `_ssl` against
  `_hashlib`, both linking OpenSSL, 11 KB more). The low 12 bits of what follows an `ADRP`
  (`ADD`, `LDR`) are not remapped (`exe-recons/arm64-delta` predicts them). By differences
  (`exe-recons/arm64-delta`) only ARM64 code, of an image's largest section, against its
  tree's root (not its parent); x86 code (whose streams are of bytes, by field) is not.
- Mach-O code signatures: only their code slots are derived; the special slots (of the
  requirements, the entitlements, `Info.plist` and `CodeResources`), and `_CodeSignature/
  CodeResources` itself (the hashes of a bundle's files), are kept as they are.
- Mach-O symbol tables are taken only as `ld64` writes them (730 of the 775 of 16 KiB or more
  in the images under `/Applications`, `/System/Applications`, `/usr/bin` and `/usr/lib` of a
  Mac on 2026-10-30): not lld's string tables, whose strings share their tails (Chromium,
  Electron and Rust binaries), nor one whose strings are in another order (WebRTC's) or that
  holds other data (a .NET single-file bundle). Nor are 32-bit images' tables.
- .NET: only the section holding the metadata is taken, so IL in other sections is kept as it
  is (one file of the sample). Managed resources are kept as they are (they are most of what
  is left: satellite assemblies, `*.resources.dll`, gain nothing). Of ReadyToRun images, only
  those of x86-64 have their native code filtered; NGen images (`*.ni.dll`) and mixed-mode
  assemblies are not taken.
