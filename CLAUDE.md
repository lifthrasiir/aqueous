# CLAUDE.md

Aqueous: an archive format and archiver, `aqr`, built around lossless recompression. See
[README.md](README.md) for the overview.

## Layout

- `spec/index.bs` — the format specification (Bikeshed). `make -C spec` builds `index.html`;
  figures are `figures/*.pik` with their SVGs committed. Pushes to `main` deploy it to GitHub
  Pages (`.github/workflows/spec.yml`).
- `docs/` — design documents, mostly in Korean: `10gb-todo.md` (what is left for the 10 GB
  benchmark), `file-formats.md` (measurements by format), `*-design.md`.
- `crates/` — the core crates
  - `aqr` — the CLI (`aqr -c/-t/-d`).
  - `aqr-formula` — entry formulas (stack-machine programs) and the archive structure
    (`src/aqr.rs`: the archive encoder and decoder).
  - `aqr-compress` — picks a recompressor by magic bytes, makes the formula, stores the archive.
  - `aqr-decompress` — runs the formulas' modules natively and extracts archives.
  - `aqr-common` — shared parts: varints, rANS, bit I/O, heap accounting, the task pool.
    `aqr-num` — numbers printed as text.
  - `aqr-api`, `aqr-ld` — the Rust interface of modules and the Wasm linker wrapper.
  - `aqr-modules` — Wasm modules built from the recompressors' code (`src/bin`, `wasm/`,
    `src/known.rs`).
- `codecs/` — the recompressor crates `aqr-*` (flate, png, jpeg, mp3, flac, pdf, zip, sqlite,
  git, lz4, lzma, lzo, bvx, bz2, snappy, gif, exe, psd, clip, raw, tex, obj, fasta, wrt, bundle,
  asar, …), each with a `README.md` and a `FORMAT.md` describing what it stores; a crate with a
  `FORMAT.md` belongs here. `aqr-flate` underlies everything that uses DEFLATE. The workspace
  takes both directories by glob, so a new crate needs no entry in the root `Cargo.toml`.
- `corpus/` and `cache/` under a crate are gitignored local data (test inputs, cached results
  of slow measurements).
- `flate-zoo/` — a submodule (a workspace of its own). `third_party/` — reference checkouts,
  gitignored. Both are excluded from the workspace.
- `scripts/` — helper scripts (Python uses the `.venv` at the root).

## Development notes

- Test with `cargo rt -p <crate>` (`.cargo/config.toml`: `cargo test --profile release-test`,
  optimised and incremental). Codec tests in debug builds are very slow (a debug
  `cargo test --workspace` once ran over 12 minutes). Timings (`--ignored` tests) still want
  `-r`. Test the crates you changed rather than the whole workspace; for the archive encoder
  and decoder that is aqr-formula, aqr-compress and aqr-decompress. Stream long output with
  `rg --line-buffered` rather than through `sort`, which hides progress.
- **Build and run the release `aqr` only through `cargo aqr <args>…`** (crates/aqr-bincache),
  never `cargo build -p aqr`, `cargo run -p aqr` or `target/release/aqr` directly: it builds
  as needed and keeps each binary by a hash of its sources, so a rebuild for a source tree
  already built is skipped and earlier binaries stay at hand.
  `cargo aqr +` lists the kept binaries; `cargo aqr +<id> <args>…` runs an earlier one, e.g. as
  the "before" of a comparison.
- Format with the `rustfmt.toml`s (width 120).
- Cache the results of measurements taking over a minute in `cache/`.
- Measure peak memory (`/usr/bin/time -l`) on runs over big inputs.
- `aqr --stats` (compressing, decompressing or testing) samples the threads as it runs and
  tells on standard error, after each archive, a line of the whole (seconds, workers' busy
  share, heap peak and what held it, resident peak) and what stands out: stretches with the
  pool barely used and what ran then, files that took long by themselves, workers waiting,
  kinds of files slow for their bytes or hardly compressed. `--stats=FILE.json` writes all
  that was sampled too (per content: kind, bytes, archived bytes, modules; the samples by
  thread and label), `--stats=FILE.html` a page showing it (threads over time, heap by
  allocator, time by label, files by kind); both may be given. The JSON's layout is in
  `crates/aqr/src/stats.rs`. The sampling costs little, so run big benchmarks (10gb) with
  `--stats --stats=FILE.json`, and read it before reaching for a profiler.
- Benchmark zstd at level 3 or below, never 19 (far too slow); cached results at 19 may still
  be cited.
- Performance in numbers: measure speed and size before and after, and record them in the
  commit message.
- Monomorphize (generics, const generics) only where it is needed and a measurement shows it
  is hot; elsewhere use `dyn`. Generic copies bloat the Wasm modules that ship in archives.
  Compare "instructions retired" (`/usr/bin/time -l`) of interleaved A/B binaries rather than
  wall-clock. For example, aqr-flate's table shapes and the token loop of Player/Recorder stay
  specialized (4–8% each otherwise), while Adaptive uses `dyn Search`
  (`Corr::SPECIALIZED = false`).
- On big streams prefer structural wins (sharing, probe-guided selection) to slower coding;
  slow modes such as aqr-flate's profile 10 belong to a separate high level, never to big
  streams by default.
- Don't look at GPL code: for LZO, only public specifications (the kernel's
  `Documentation/staging/lzo.rst`, libarchive's BSD lzop reader) are read, and liblzo2 and
  lzop are used only as black-box oracles for fixtures. Never read LZO, miniLZO or the kernel's
  compressor sources, nor the installed lzo headers.
- Private data (browser profiles and the like) is measured by the user; don't copy it.
- Design documents: don't repeat what `spec/index.bs` already says; point to the spec section
  or drop the text. Where they disagree, the spec is the newer source; fix the document.

## Modules

Archives carry real Wasm modules built from the recompressors' code; native decoding is an
optimization picked by hash.

- Codec crates: feature `encode` (default) brings in `formula`, aqr-formula, zstd and rayon;
  `module.rs` is the decoding code without aqr-formula. Natively they stay plain libraries.
- aqr-modules: one `[[bin]]` per module (never one module doing several things). `build.sh`
  builds them through aqr-ld and writes `wasm/*.wasm`, `*.block` (the module blocks' data),
  `src/known.rs` (name, version, SHA-512/256) and the registry's table in `spec/index.bs`.
- **After changing any code a module links, rerun `crates/aqr-modules/build.sh`** and commit
  the results; a test in aqr-decompress catches stale ones. If you don't rebuild, say so in
  the commit message.
- aqr-decompress: a known hash runs natively, anything else in wasmtime.
- No variable arity: use Split and separate modules (png-against, leveldb-recons, …).
- Model data (aqr-flate's) is a resource, not embedded in the module.
- Threads in Wasm: fork-join tasks (`aqr_spawn`/`aqr_join`/`aqr_take`; `aqr_common::pool::
  par_map_apart` with `wire::Wire`) and channels (flac/pcm pipelines); the spec's "Module
  execution" has the rules, `docs/xform-design.md` the design. The spawn record must stay in a
  never-inlined frame (`spawn_record`), or the task's stack clobbers the caller's locals. A module
  spawning tasks declares its task slots with `whole!(slots N, …)` (the custom section
  `aqr_task_slots`, which aqr-ld checks), its code given N as its threads. Other
  `par_map`/`scope` uses run serially in Wasm. Port a pipeline only when its speedup is worth
  the module's growth; report both (png, psd and mp3 were measured and not ported).
- Memory: each Module carries an estimate of its working memory (`Module::new(..).with_memory`
  in the codec's `formula.rs`; `docs/arena-design.md`), from which aqr-compress declares each
  application's `apply_memory_limit` (`crates/aqr-compress/src/memory.rs`). Changing what a
  module allocates means changing its estimate; aqr-decompress's ignored test `estimates_hold`
  checks them against the samples of `scripts/module-memory.sh`.
- Size: aqr-ld trims modules itself (prefer automating in aqr-ld over toolchain flags);
  `AQR_LD_NAMES=1` keeps the name section for size debugging. Module error types must
  implement `Display` (`Debug` of `&str` pulls in core's Unicode tables).
- Module bytes are a per-archive cost that dominates small archives. Planned: built-in
  dictionaries in the spec (u8-sized module blocks as hash references, dictionary blocks as
  references to a built-in dictionary; two 64 KiB dictionaries, module and text, about 60 KB
  in all). A text dictionary needs natural language as well as code; a trained dictionary's
  entropy tables hurt CJK, so the compressor must be able to use it as raw content too.

## Project notes

- aqr-formula's decoder (`defined()` in `src/aqr.rs`) does not accept import blocks 58–5F,
  though the spec has them. Implement them together with import resolution and overlays when
  extraction work starts, not by just adding them to `defined()`.
- Near-duplicate files (variant sets, from 差分 PNG sets): reference coding in aqr-png,
  multi-output formulas (`Node::Output`, `Node::Patch`), png-set and generic per-class
  patches (`Formula::merge_classes`) are done (`docs/xform-design.md` §4.6). Left: a byte bound
  on the dependency closure of top-level entries for random access; aqr-flate rebuilding of the
  ZIPs' deflated entries (not worth it unless profile 10 gets much faster).
- aqr-lzma: the goal is deduplicating contents, not the best recompression; a record of about
  14% at a reasonable speed is fine. What is left is in `docs/10gb-todo.md` item 6.
- aqr-lzo: a record of the parse rather than emulation, as LZO has many encoders. Left (its
  README's Status): raw LZO1X in other containers, LZO-RLE, LZO1X-1's hash table start.
- aqr-bvx (Apple's LZFSE and LZVN, `bvx2`/`bvxn` blocks; asset catalogs): a record of the parse,
  as aqr-lzo, with the LZFSE compressor's table of candidates (14-bit hash, 4 ways) emulated as
  a context. Apple's lzfse code (BSD) is not read; libcompression is the black box. Left (its
  README's Status): decmpfs, DMG ULFO, an image model for the data.
- aqr-gif: the LZW codes coded given the pixels against the longest match (every encoder seen
  writes it), the pixels by a context-mixing model of palette indices (lpaq's style; integers
  only, so Wasm and native agree). Left (its README's Status): frames against the image shown,
  TIFF's and PDF's LZW.
- aqr-bz2 reimplements libbzip2's compressor (1.0.3 on), written from memory of its behaviour;
  libbzip2's code is not read (BSD-style). Python's `bz2` and a shared libbz2
  (`BZ2_hbMakeCodeLengths`) are the black boxes. Equal rotations of periodic blocks order
  otherwise, so their `origPtr` goes in the params. Left (its README's Status): lbzip2 and
  others, bzip2 in ZIP/7z/DMG, tasks in Wasm.
- aqr-snappy and aqr-lz4 reimplement the encoders: Chrome's LevelDB Snappy is the
  multiplicative-hash build (Homebrew's arm64 builds use the CRC hash and do not match);
  Firefox's mozlz4 is liblz4's `LZ4_compress_default`.
- aqr-git is the pack recompressor `git-pack-recons`, at aqr-flate's profile as aqr-compress
  picks it; a pack with its `.idx`/`.rev` is a set (`git-pack-index-recons`, only the ids
  stored). Left: deriving the ids (resolving every delta), `multi-pack-index` from the indexes,
  the archive's 16 MiB solid-block bound, pack blobs in `together`.

## Working copy and commits

Other sessions often work in the same working copy at the same time.

- Stage explicit paths only; never `git commit -a` or `git add .`/`-A`.
- Never stage the `flate-zoo` submodule pointer, nor its hunk in `Cargo.lock`. For
  `Cargo.lock`, stage only your own hunks.

**Commit on your own after each substantial step, one commit per coherent step. Never push.**

**Write messages in the usual git format: a short summary on the first line, one blank line,
then the details.** The history so far has settled on cramming a paragraph of several hundred
characters into the subject line. That convention ends here; don't imitate the history.

- Subject: `crate: what was done`, in English, about 72 characters at most, no trailing period.
- Body: wrapped at about 72 columns. Why the change, how, measurements (before -> after, and
  on what), whether modules were rebuilt, what is left. Use lists where they help.

```
aqr-clip: make tiles' zlib streams with aqr-flate's zlib::compress

The tiles were deflated by driving the emulator block by block, with an
Adler-32 of its own. Use zlib::compress (deflate_fast run on its own) and
aqr-common's Adler-32 (simd-adler32 natively) instead.

On an M4, speed -t 10 -b 64 -u on the 13 files: deflating 30.0 -> 18.6
thread seconds (24.0 with compress alone), every stream byte for byte.

Modules not rebuilt.
```
