# aqr-dwarf

DWARF debugging information rebuilt bit for bit from streams the archive compresses better
(`FORMAT.md`). So far: Apple's accelerator tables (`__apple_names`, `__apple_types`,
`__apple_namespac`, `__apple_objc` of a dSYM's `__DWARF` segment) from their entries in the
order of their strings, their hashes, buckets and offsets derived from `.debug_str`, the DIE
offsets of each name as deltas (`dwarf-apple`). aqr-compress takes the tables of Mach-O
images, `.debug_str` in a stream of its own that the tables take as input.

Written from the DWARF 5 standard and what is known of the tables' layout; no code of LLVM,
gimli or other DWARF tools was read.

```sh
cargo run --release -p aqr-dwarf --example aqr-dwarf-check -- DIR...   # tables of dumped sections, rebuilt, streams under zstd
```

`DIR` holds sections dumped as `llvm-objcopy --dump-section __DWARF,__apple_names=DIR/apple_names`
(and `debug_str`).

## How it does

Measured on 2026-11-02 on an M4, on the dSYM of a release build of `aqr` (rustc 1.99.1, DWARF
4; 214.3 MB, of which `__apple_names` 23.5 MB and `__debug_str` 75.2 MB; `corpus/`, not in
git).

| table | as it is | streams | zstd -3 | of the streams |
|---|---|---|---|---|
| `__apple_names` | 23.48 MB | 8.84 MB | 17.96 MB | 3.79 MB |
| `__apple_types` | 0.72 MB | 0.45 MB | 0.44 MB | 0.19 MB |
| `__apple_namespac` | 0.91 MB | 0.54 MB | 0.79 MB | 0.46 MB |

Of `__apple_names`, 3.75 M DIE offsets for 0.40 M names (rustc inlines much, and each
inlined instance is a DIE of the name): 3.19 MB of the 3.79 MB. The module decodes it in 0.10
s (encoding, with the check, in 0.16 s).

Through `aqr` (level mid), the dSYM: 41.77 -> 30.12 MB (−27.9%; xz -9 31.71 MB, zstd -3 48.72
MB). Compressing takes 40.6 G instructions (58.8 G before: less is left to the archive), peak
RSS 1.33 GB (1.56 GB before); testing 3.48 G instructions (3.12 G before), peak RSS 279 MB
(200 MB before).
The module's Wasm block is 7.1 KB.

## Status

Left:

- `.debug_info` by the abbreviations of each unit, the line programs, the range and location
  lists, `.debug_str_offsets`, `.debug_names` (DWARF 5's index, hashes as Apple's).
- ELF images (`.debug_*`, their compressed sections), Mach-O object files, `.dwo` and `.dwp`.
- The DIE offsets of `__apple_namespac`, which follow from `.debug_info` (each namespace's
  DIEs in every unit).
