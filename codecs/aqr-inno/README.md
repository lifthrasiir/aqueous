# aqr-inno

Inno Setup installers (4.0 and later: the offset table of version 1 that the loader carries
from 5.1.5, the earlier tables at the installer's end before): the stream of LZMA that holds the files of a solid installer, found by that table and split by
aqr-lzma into its data and a record of its parse. aqr-compress then stores the data, scanned in
turn like any file (the installer's executables, DLLs and resources are each recompressed by
their own crate), and `lzma-recons` rebuilds the stream bit for bit. The formula is one
application of aqr-lzma's module over a slice of the installer, so this crate has no module of
its own; [FORMAT.md](FORMAT.md) tells what is taken and how it works. Of Inno Setup 4.0's zlib chunks, and its
setup program (a zlib stream too), this crate finds the streams; aqr-compress recompresses each in
turn as a zlib stream (aqr-flate: zlib's level 9, what Inno Setup 4.0 wrote).

```sh
cargo run --profile release-test -p aqr-inno --example aqr-inno-check -- FILE   # table, streams split, rebuilt, timed
```

```rust
let t = aqr_inno::table(&exe); // the loader's offsets, None where there is no table that holds
let pieces = aqr_inno::split(&exe, threads); // each LZMA stream: its range, data and record
```

## What is taken

Solid and per-file LZMA chunks that end with an end marker (what the Inno Setup compiler
writes), found by the table of the loader, in a PE image; zlib chunks and the setup program of
4.0. Not taken, kept as they are: installers of bzip2 (`compression=bzip`) and encrypted ones
(6.4's `encryption`).

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

WinUHA 2.0's four installers (Inno Setup 4.0.0a to 5.0.4, 4.51 MB; two of zlib chunks, two of a
solid LZMA stream): their files are executables packed by PECompact or UPX, whose streams split
come to more than they are, so only 2.0a's setup program is taken: 4,340,482 -> 4,233,874 B
(2026-11-15, 16 threads, 0.28 s). The rest is turned down: the streams split come out smaller
only with the others' data (a version's files alike in the next: 4.34 -> 3.89 MB as files
apart), which no probe of one stream sees, and each installer's data is archived in blocks of
its own.

## Left

- **The setup header and the setup program of 4.1 on.** The header (`Inno Setup Setup Data`,
  blocks of 4 KiB each with a CRC-32, LZMA) and the compressed setup program (the same in every
  installer of a version) are kept as they are.
- **Data in separate `setup-1.bin` files.** `DiskSpanning`: the offset table's data offset is
  then 0.
- **Earlier tables of other versions.** `rDlPtS02`, `rDlPtS06`: no sample seen.
- **Files cut apart by the header's list.** The call instruction filter
  `CallInstructionOptimization` Inno applies to executables, and per-file kinds: the data is
  scanned as a whole instead.
