# aqr-dex

Android DEX files (`classes.dex` in APKs, AARs and JARs for Android) rebuilt bit for bit from
streams the archive compresses better: strings front-coded, the id tables and class definitions
by column, the code items' instructions split by field with their indices through caches of the
recent ones, checksum and signature derived; whatever is not laid out as the module rebuilds it
is kept as it is. `FORMAT.md` specifies the module `dex-recons`, and how it works. Takes DEX up
to version 040 (not 041's containers of several files).

Written from the DEX format's public description (source.android.com, "Dalvik Executable
format"); no DEX tool's code was read.

```sh
cargo run --release -p aqr-dex --example aqr-dex-check -- FILE_OR_DIR...   # rebuilt, streams under zstd
cargo run --release -p aqr-dex --example aqr-dex-bench -- [-n RUNS] FILE... # the module's speed
```

## Results

On the 21 DEX files of 7 APKs from F-Droid (132.0 MB: Organic Maps, K-9 Mail, AntennaPod, F-Droid,
Fennec, NewPipe, VLC; `corpus/fdroid`, not in git), on an M4, 2026-11-02.

| | before aqr-dex | with it |
|---|---|---|
| the files, `aqr` at mid | 46.23 MB | 30.87 MB (-33.2%) |
| the 7 APKs, `aqr` at mid | 202.53 MB | 188.77 MB (-6.8%) |
| the files, xz -9e one by one (as one tar) | 37.32 MB (33.21 MB) | |

Splitting runs at about 130 MB/s and decoding at about 290 MB/s on a thread; testing the files
through `aqr` 827 MB/s. The module's Wasm block is 21.0 KB. In the APKs DEX files are now always
inflated and their DEFLATE bodies rebuilt (aqr-zip's probe had kept some as they were), so testing
the 7 takes 22% more instructions, compressing 6% more.

## Tests

```sh
cargo rt -p aqr-dex
```

- `tests/dex.rs`: round trips of synthetic files (`tests/common/synth.rs`) and the kept modes,
  files not taken, corrupt and damaged streams, lying lengths and item counts.

## Left

- **Opcodes and registers.** 3.6 MB each of the 31 MB of streams: a context model would code them by
  the opcodes before; `registers_size` and `ins_size` derived from the methods' prototypes.
- **Order of a class's code items.** The source's order, not derivable, costs 0.32 MB; ranks within
  the class's own items came to 0.21 MB in a sketch.
- **Debug info, annotations, encoded arrays, type lists, tries and handlers.** Debug info (1.13 MB)
  could follow its state machine; the rest is as it is (handlers 0.23 MB).
- **DEX 041 and DEX in other data.** Containers of several files; DEX inside OAT, VDEX.
- **resources.arsc and binary XML.** The APKs' other formats, with string pools as DEX has.
