# aqr-dex

Android DEX files (`classes.dex` in APKs, AARs and JARs for Android) rebuilt bit for bit from
streams the archive compresses better (`FORMAT.md`): the strings front-coded, with their
lengths and offsets derived; the tables of type, prototype, field and method ids and of class
definitions by column, in deltas, with prototypes' shorties derived from their types; the
classes' data with the offsets of their code as ranks among the code items near the last
used, and of the data itself as ranks; the checksum and the SHA-1 signature derived; every
other section as it is, each kind in a stream of its own. Whatever is not laid out as the
module rebuilds it is kept as it is.

Written from the DEX format's public description (source.android.com, "Dalvik Executable
format"); no DEX tool's code was read.

```sh
cargo run --release -p aqr-dex --example aqr-dex-check -- FILE_OR_DIR...   # rebuilt, streams under zstd
cargo run --release -p aqr-dex --example aqr-dex-bench -- [-n RUNS] FILE... # the module's speed
```

## How it does

Measured on 2026-11-02 on an M4, on the 21 DEX files of 7 APKs from F-Droid (132.0 MB:
Organic Maps, K-9 Mail, AntennaPod, F-Droid, Fennec, NewPipe, VLC; `corpus/fdroid`, not in
git).

- `aqr-dex-check` (zstd -9, each stream class as one): the files 49.23 MB, the streams
  38.38 MB (−22.0%). xz -9e takes the files one by one to 37.32 MB.
- Through `aqr` (level mid) on the files alone: 46.23 -> 38.09 MB (−17.6%).
- The 7 APKs through `aqr` (level mid): 202.53 -> 193.53 MB (−4.4%). Their DEX files are
  now always inflated and their DEFLATE bodies rebuilt, which aqr-zip's probe had kept as
  they were in some (VLC's): testing the 7 takes 17% more instructions (226 -> 265 G for two
  rounds), nearly all of it aqr-flate's; the module itself decodes at 620 MB/s on a thread.
- Splitting takes about 3 ms a MB on a thread.

| streams | under zstd -9 | the sections as they are |
|---|---|---|
| strings | 4.57 MB | 9.02 MB |
| id tables | 3.30 MB | 5.49 MB |
| class definitions and data | 1.92 MB | 4.58 MB |
| code items | 24.98 MB | 24.98 MB |

## Status

Left:

- The code items: 65% of what is left. The instructions split by opcode, registers and
  operand, the indices of methods, fields, strings and types each by a cache of the recent
  ones (as aqr-exe does IL in .NET images); debug info by its state machine.
- The orders of the classes' code items within a class (the source's order, not derivable)
  cost 0.32 MB: ranks within the class's own items came to 0.21 MB in a sketch.
- Annotations, encoded arrays and type lists as they are.
- DEX 041's containers of several files; DEX files inside OAT, VDEX or other data.
- resources.arsc and binary XML (APKs' other formats, of string pools as DEX has).
