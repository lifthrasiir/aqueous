# aqr-dex

Android DEX files (`classes.dex` in APKs, AARs and JARs for Android) rebuilt bit for bit from
streams the archive compresses better (`FORMAT.md`): the strings front-coded, with their
lengths and offsets derived; the tables of type, prototype, field and method ids and of class
definitions by column, in deltas, with prototypes' shorties derived from their types; the
classes' data with the offsets of their code as ranks among the code items near the last
used, and of the data itself as ranks; the code items' instructions split by field (opcodes,
registers, literals, branches, payloads), their indices of strings, types, fields and methods
through caches of the recent ones of each kind, the counts of arguments of invocations
predicted from the methods' prototypes, the debug info items by first use and recency; the
checksum and the SHA-1 signature derived; every other section as it is. The 58 streams go to
14 inputs of the module. Whatever is not laid out as the module rebuilds it is kept as it is.

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

- `aqr-dex-check` (zstd -9, each stream's bytes of all files as one): the files 49.23 MB,
  the streams 31.05 MB (−36.9%), the module's 14 inputs 31.32 MB (−36.4%). xz -9e takes the
  files one by one to 37.32 MB, as one tar to 33.21 MB.
- Through `aqr` (level mid) on the files alone: 46.23 MB before aqr-dex, 30.87 MB now
  (−33.2%), tested at 827 MB/s.
- The 7 APKs through `aqr` (level mid): 202.53 MB before aqr-dex, 188.77 MB now (−6.8%).
  Their DEX files are now always inflated and their DEFLATE bodies rebuilt, which aqr-zip's
  probe had kept as they were in some (VLC's): testing the 7 takes 22% more instructions
  (two rounds: 226 -> 276 G), most of it aqr-flate's; compressing 6% more (1494 -> 1583 G).
- The module splits at about 130 MB/s and decodes at about 290 MB/s on a thread (VLC's
  six files; 620 MB/s before the instructions were split).
  Its Wasm block is 25.2 KB.

| streams | under zstd -9 | the sections as they are |
|---|---|---|
| strings | 4.57 MB | 9.02 MB |
| id tables | 3.30 MB | 5.49 MB |
| class definitions and data | 1.92 MB | 4.58 MB |
| code items | 17.65 MB | 24.98 MB |
| debug info, annotations, type lists | 3.61 MB | 3.61 MB |

## Status

Left:

- The instructions' opcodes (3.6 MB) and registers (3.6 MB), the most of what is left: a
  context model would code them by the opcodes before (zstd does not see that an opcode's
  registers follow from it); `registers_size` and `ins_size` from the methods' prototypes.
- The order of the classes' code items within a class (the source's order, not derivable)
  costs 0.32 MB: ranks within the class's own items came to 0.21 MB in a sketch.
- Debug info (1.13 MB) by its state machine; annotations, encoded arrays and type lists as
  they are; tries and handlers as they are (0.23 MB).
- DEX 041's containers of several files; DEX files inside OAT, VDEX or other data.
- resources.arsc and binary XML (APKs' other formats, of string pools as DEX has).
