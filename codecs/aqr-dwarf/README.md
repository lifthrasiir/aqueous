# aqr-dwarf

DWARF debugging information rebuilt bit for bit from streams the archive compresses better
(`FORMAT.md`):

- `.debug_info` (`dwarf-info`): its units' DIEs parsed by the abbreviations of
  `.debug_abbrev`, each value coded by what it is, in a stream of its kind: references to
  DIEs as indices of DIEs (through a cache of the 64 most recent, else as the delta from the
  attribute's previous), a sibling derived from where its subtree ends; strings of
  `.debug_str` as indices of its strings, the next one not used yet predicted (else a cache,
  else the delta from the attribute's previous); addresses, lines, columns and offsets as
  deltas; the lengths of `DW_AT_high_pc`, files, expressions and other constants apart. Units
  that do not parse so (64-bit DWARF, LEB128 longer than they need) are kept as they are.
- Apple's accelerator tables (`dwarf-apple`; `__apple_names`, `__apple_types`,
  `__apple_namespac`, `__apple_objc` of a dSYM's `__DWARF` segment): their entries in the order
  of their strings, their hashes, buckets and offsets derived from `.debug_str`, the DIE
  offsets of each name as deltas.

aqr-compress takes the sections of Mach-O images (`__DWARF`, as of a dSYM) and of ELF images
(but compressed ones); `.debug_abbrev` and `.debug_str` go to streams of their own, which the
modules take as inputs.

Written from the DWARF 5 standard and what is known of the tables' layout; no code of LLVM,
gimli or other DWARF tools was read.

```sh
cargo run --release -p aqr-dwarf --example aqr-dwarf-check -- DIR...   # dumped sections rebuilt, streams under zstd
```

`DIR` holds sections dumped as `llvm-objcopy --dump-section __DWARF,__debug_info=DIR/debug_info`
(or `.debug_info=DIR/debug_info` of ELF): `debug_info`, `debug_abbrev`, `debug_str`,
`apple_names`, ...

## How it does

Measured on 2026-11-02 on an M4 (`corpus/`, not in git):

- **rustc**: the dSYM of a release build of `aqr` (rustc 1.99.1, DWARF 4; 214.3 MB:
  `__debug_str` 75.2 MB, `__debug_info` 50.1 MB, `__debug_ranges` 30.3 MB, `__apple_names` 23.5
  MB, `__debug_line` 17.2 MB);
- **clang**: the dSYM of 7-Zip's `7zr` (the LZMA SDK's sources) built by Apple clang 21 with
  `-O2 -g` (DWARF 5, 5.3 MB);
- **gcc**: the same built by GCC 13 on Ubuntu 24.04 for ARM64 (DWARF 5, ELF, 9.8 MB).

`.debug_info` alone, through `aqr-dwarf-check` (zstd -3 of each stream):

| | as it is | zstd -3 | xz -9 | the streams, zstd -3 | |
|---|---|---|---|---|---|
| rustc | 50.05 MB | 14.57 MB | 11.04 MB | 7.26 MB | −50.2% |
| clang | 2.41 MB | 1.01 MB | 0.74 MB | 0.52 MB | −48.4% |
| gcc | 4.85 MB | 2.23 MB | 1.62 MB | 0.75 MB | −66.6% |

Of rustc's, the strings take 1.65 MB, the references 1.64 MB, the lines of inlined calls
0.96 MB, the codes 0.84 MB. The module decodes it at 180 MB/s (encoding, with the check, at
65 MB/s).

Apple's tables of rustc's dSYM:

| table | as it is | streams | zstd -3 | of the streams |
|---|---|---|---|---|
| `__apple_names` | 23.48 MB | 8.84 MB | 17.96 MB | 3.79 MB |
| `__apple_types` | 0.72 MB | 0.45 MB | 0.44 MB | 0.19 MB |
| `__apple_namespac` | 0.91 MB | 0.54 MB | 0.79 MB | 0.46 MB |

Of `__apple_names`, 3.75 M DIE offsets for 0.40 M names (rustc inlines much, and each
inlined instance is a DIE of the name): 3.19 MB of the 3.79 MB. The module decodes it in 0.10
s (encoding, with the check, in 0.16 s).

Through `aqr` (level mid), each file alone:

| | before | Apple's tables | and `.debug_info` | xz -9 |
|---|---|---|---|---|
| rustc | 41.77 MB | 30.12 MB | 22.85 MB (−45.3%) | 31.71 MB |
| clang | 1.95 MB | | 1.47 MB (−24.8%) | 1.54 MB |
| gcc | 3.78 MB | | 2.35 MB (−37.9%) | 2.97 MB |

Compressing rustc's takes 47.1 G instructions (58.8 G before, 40.6 G with the tables alone),
peak RSS 0.99 GB (1.56 GB before); clang's 7.4 G (3.2 G before: the file was kept whole, now
the rest of the image is scanned as well), gcc's 8.6 G (5.5 G). Testing rustc's takes 8.0 G
instructions (3.1 G before), peak RSS 299 MB (200 MB); clang's 1.04 G (0.87 G), gcc's 1.31 G
(0.87 G). The modules' Wasm blocks are 9.9 KB (`dwarf-info`) and 4.7 KB (`dwarf-apple`), 12.0 and 7.1 KB
before they were laid out as stacks (`e468f3f0`).

## Status

Left:

- The line programs, the range and location lists, `.debug_str` itself, `.debug_str_offsets`,
  `.debug_names` (DWARF 5's index, hashes as Apple's), `.debug_types`.
- ELF's compressed sections (`SHF_COMPRESSED`, zlib or zstd; Ubuntu's debug files), Mach-O
  object files' relocations, `.dwo` and `.dwp`.
- The DIE offsets of `__apple_namespac`, which follow from `.debug_info` (each namespace's
  DIEs in every unit).
