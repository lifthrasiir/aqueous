# aqr-inno

Inno Setup installers (5.1.5 and later, whose loader carries the offset table of version 1):
the stream of LZMA that holds the files of a solid installer, found by that table and split by
aqr-lzma into its data and a record of its parse. aqr-compress then stores the data, scanned in
turn like any file (the installer's executables, DLLs and resources are each recompressed by
their own crate), and `lzma-recons` rebuilds the stream bit for bit. The formula is one
application of aqr-lzma's module over a slice of the installer, so this crate has no module of
its own; [FORMAT.md](FORMAT.md) tells what is taken.

```sh
cargo run --profile release-test -p aqr-inno --example aqr-inno-check -- FILE   # table, streams split, rebuilt, timed
```

```rust
let t = aqr_inno::table(&exe); // the loader's offsets, None where there is no table that holds
let pieces = aqr_inno::split(&exe, threads); // each LZMA stream: its range, data and record
```

aqr-compress takes the streams of a PE image from level 4 (as `.lzma` files and 7z archives:
rebuilding LZMA is slow), and counts the installer's data (up to the total the table gives, a
certificate after it left to the PE extent) in the image's extent.

## Results

`ALZip1237.exe` (ALZip 12.37, Inno Setup 6.4.2, 45.27 MB, signed; 107.71 MB of files in a single
solid LZMA stream of 43.17 MB, dictionary 8 MiB), 16 threads, measured on 2026-11-11:

| | size | time |
|---|---|---|
| before (the stream kept as it is) | 44.79 MB | 0.45 s |
| the stream split: data + record | 35.84 MB | 19.0 s |
| decompressing | | 6.8 s |

The record is 2.90 MB (6.7% of the stream); the rest is the data, recompressed as
`aqr -c` does 107.71 MB of the same files (31.4 MB).

## Status

Done: solid and per-file LZMA chunks that end with an end marker (what the Inno Setup compiler
writes), found by the table of the loader, in a PE image.

Left:

- The setup header (`Inno Setup Setup Data`, blocks of 4 KiB each with a CRC-32, LZMA) and the
  compressed setup program (the same in every installer of a version): both kept as they are.
- Installers of zlib and bzip2 (Inno Setup before 5.3's default, `compression=zip|bzip`), and
  encrypted ones (6.4's `encryption`): no chunk is taken.
- Installers before 5.1.5 (another table), and data in separate `setup-1.bin` files
  (`DiskSpanning`, the offset table's data offset is then 0).
- Files cut apart by the header's list (for the call instruction filter `CallInstructionOptimization`
  Inno applies to executables, and per-file kinds): the data is scanned as a whole instead.
