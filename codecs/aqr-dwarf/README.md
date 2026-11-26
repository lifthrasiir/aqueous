# aqr-dwarf

DWARF debugging information rebuilt bit for bit from streams the archive compresses better:
`.debug_info` (`dwarf-info`: each DIE's values coded by what they are, references as indices,
siblings derived) and Apple's accelerator tables (`dwarf-apple`: `__apple_names`, `__apple_types`,
`__apple_namespac`, `__apple_objc`, their hashes, buckets and offsets derived). `FORMAT.md`
specifies the modules, and how they work.

aqr-compress takes the sections of Mach-O images (`__DWARF`, as of a dSYM) and of ELF images (not
compressed ones); `.debug_abbrev` and `.debug_str` go to streams of their own, which the modules
take as inputs. Units that do not parse (64-bit DWARF, LEB128 longer than they need) are kept as
they are.

Written from the DWARF 5 standard and what is known of the tables' layout; no code of LLVM,
gimli or other DWARF tools was read.

```sh
cargo run --release -p aqr-dwarf --example aqr-dwarf-check -- DIR...   # dumped sections rebuilt, streams under zstd
```

`DIR` holds sections dumped as `llvm-objcopy --dump-section __DWARF,__debug_info=DIR/debug_info`
(or `.debug_info=DIR/debug_info` of ELF): `debug_info`, `debug_abbrev`, `debug_str`,
`apple_names`, ...

## Results

On an M4, 2026-11-02 (`corpus/`, not in git): the dSYM of a release build of `aqr` (rustc 1.99.1,
DWARF 4, 214.3 MB), of 7-Zip's `7zr` by Apple clang 21 (`-O2 -g`, DWARF 5, 5.3 MB), and of the
same built by GCC 13 on Ubuntu 24.04 for ARM64 (ELF, DWARF 5, 9.8 MB).

Through `aqr` (level mid), each file alone:

| | before | with aqr-dwarf | xz -9 |
|---|---|---|---|
| rustc | 41.77 MB | 22.85 MB (-45.3%) | 31.71 MB |
| clang | 1.95 MB | 1.47 MB (-24.8%) | 1.54 MB |
| gcc | 3.78 MB | 2.35 MB (-37.9%) | 2.97 MB |

Of rustc's, Apple's tables alone take 41.77 to 30.12 MB. `.debug_info` alone, its streams under
zstd -3 against the section under zstd -3: rustc 7.26 against 14.57 MB, clang 0.52 against
1.01 MB, gcc 0.75 against 2.23 MB; `__apple_names` 3.79 against 17.96 MB.

The `.debug_info` module decodes at 180 MB/s (encoding, with the check, 65 MB/s), the Apple tables
of rustc's dSYM (23.5 MB) in 0.10 s. Compressing rustc's dSYM takes 47.1 G instructions (58.8 G
before) and 0.99 GB at peak (1.56 GB); testing 8.0 G (3.1 G) and 299 MB (200 MB). The modules'
blocks are 9.9 KB (`dwarf-info`) and 4.7 KB (`dwarf-apple`).

## Tests

```sh
cargo rt -p aqr-dwarf
```

- `tests/info.rs`: `dwarf-info` round trips and the units kept as they are.
- `tests/apple.rs`: `dwarf-apple` round trips and the tables kept as they are.
- `tests/alloc.rs`: memory bounds.

## Left

- **Other sections.** Line programs, range and location lists, `.debug_str` itself,
  `.debug_str_offsets`, `.debug_names` (DWARF 5's index, hashes as Apple's), `.debug_types`; the
  line programs and ranges are the biggest (rustc's `__debug_line` 17.2 MB, `__debug_ranges`
  30.3 MB).
- **Compressed and object inputs.** ELF's compressed sections (`SHF_COMPRESSED`, zlib or zstd;
  Ubuntu's debug files), Mach-O object files' relocations, `.dwo` and `.dwp`.
- **`__apple_namespac` DIE offsets.** They follow from `.debug_info` (each namespace's DIEs in
  every unit); now 0.46 MB of 0.79 MB under zstd -3.
