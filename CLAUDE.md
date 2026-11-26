# CLAUDE.md

Aqueous: an archive format and archiver, `aqr`, built around lossless recompression. See
[README.md](README.md) for the overview. Archives carry real Wasm modules built from the
recompressors' code; native decoding is an optimization picked by hash.

## Where to read more

- `spec/index.bs` — the format specification (Bikeshed; `make -C spec`, figures `figures/*.pik`
  with their SVGs committed). Normative: where a document disagrees, the spec is newer.
- `docs/crates.md` — what each crate holds, the self-extracting programs, Windows builds and the
  release, notes by codec. Read the section of the crate you change.
- `docs/modules.md` — the rules for code that a Wasm module links (building, threads, memory
  plans, released modules). Read it before changing such code.
- `docs/README.md` — the design documents (mostly Korean) and what each covers;
  `docs/glossary.md` for the words of the code and spec.
- Each crate's `README.md` (status, what is left) and each codec's `FORMAT.md` (what it stores).

## Layout

- `crates/` — the core: `aqr` (the CLI), the archive (`aqr-formula`, `aqr-archive`,
  `aqr-compress`, `aqr-decompress`, `aqr-native`), other formats (`aqr-unpack`, `aqr-pack`,
  `aqr-foreign`), modules and their runtimes (`aqr-api`, `aqr-ld`, `aqr-modules`, `aqr-wasmrt`,
  `aqr-blob`, `aqr-interp`, `aqr-dll`), self-extracting programs (`aqr-sfx`, `aqr-sfx-shim`),
  frontends (`aqr-ipc`, `aqr-win`, `aqr-win-shell`, `aqr-win-install`, `aqr-mac`, `aqr-l10n`) and
  shared parts (`aqr-common`, `aqr-testkit`, `aqr-num`, `aqr-walk`, `aqr-version`,
  `aqr-bincache`). One line each and more in `docs/crates.md`.
- `codecs/` — the recompressor crates `aqr-*`, each with a `README.md` and a `FORMAT.md`; a crate
  with a `FORMAT.md` belongs here. `aqr-flate` underlies everything that uses DEFLATE.
- `man/` — the manual pages, mdoc(7): `aqr.1` (every option of `--help`, a test of crates/aqr
  checks), `aqrsfx.5` (`.aqr/sfx/`), `aqrattributes.5` (`.aqr/attributes`). Update them with
  what they describe. The site's `man/` is made of them in CI (`scripts/man2html.py OUT`, mandoc).
- `site/` — the project site; pushes to `main` deploy it and the spec (under `/spec/`) to GitHub
  Pages (`.github/workflows/spec.yml`).
- `scripts/` — helper scripts (Python uses the `.venv` at the root).
- `fuzz/` — cargo-fuzz targets (a workspace of its own; `fuzz/run.sh TARGET`, its README).
  aqr-archive skips its checksums under `cfg(fuzzing)`.
- `third_party/` — reference checkouts, gitignored, excluded from the workspace. `corpus/` and
  `cache/` under a crate are gitignored local data.
- The workspace takes `crates/*` and `codecs/*` by glob, so a new crate needs no entry in
  `members`. The versions and paths of dependencies are all in the root's
  `[workspace.dependencies]` (a crate others take gets an entry there); members say
  `NAME.workspace = true`, adding `features`, `optional`, or `default-features = true`.

## Building and testing

- **Build and run the release `aqr` only through `cargo aqr <args>…`** (crates/aqr-bincache),
  never `cargo build -p aqr`, `cargo run -p aqr` or `target/release/aqr`: it keeps each binary by
  a hash of its sources, and rebuilds the modules (and the SFX pack) where what they are made of
  changed (`AQR_BINCACHE_MODULES=0` skips it). `cargo aqr +` lists the kept binaries; `cargo aqr
  +<id> <args>…` runs an earlier one (the "before" of a comparison). `cargo aqr-win` does the same
  for aqr-win.
- Test with `cargo rt -p <crate>` (`cargo test --profile release-test`: optimised, incremental);
  debug codec tests are very slow. Test the crates you changed, not the workspace; for the archive
  encoder and decoder that is aqr-archive, aqr-compress and aqr-decompress. `cargo nextest run
  --cargo-profile release-test …` is about 3 times as fast over many packages. Timings
  (`--ignored` tests) want `-r`.
- CI tests what `scripts/affected-crates.py BASE` selects. A test reading another package's files
  names them in its source by a relative path (`"../../codecs/aqr-jpeg/tests/data/…"`) or a path
  from the root (`fixture("codecs/aqr-7z/…")`), or the script misses it.
- Stream long output with `rg --line-buffered` rather than through `sort`.
- Format with `cargo fmt` (width 120).
- Windows release binaries are of a Windows 7 target on a pinned nightly; development builds are
  `x86_64-pc-windows-msvc` (`docs/crates.md`, "Windows builds").

## Modules, in short

Details in `docs/modules.md`.

- **After changing any code a module links, commit the modules rebuilt** (`cargo aqr` runs
  `crates/aqr-modules/build.py` and names the modules changed); a test in aqr-decompress catches
  stale ones. If you don't rebuild, say so in the commit message.
- Each module carries the exact plan of its working memory; changing what it allocates means
  changing its plan (`*_plans_hold` tests check it to the page).
- One `[[bin]]` per module, no variable arity (Split and separate modules), model data as
  resources. Module error types implement `Display`, not just `Debug`.
- Module bytes are a per-archive cost: prefer `dyn` to generics unless a measurement shows a hot
  spot; port a pipeline to Wasm tasks only when its speedup is worth the module's growth.

## Measuring

- Performance in numbers: measure speed and size before and after, and record them in the commit
  message. Compare "instructions retired" of interleaved A/B binaries rather than wall-clock.
- Cache the results of measurements taking over a minute in `cache/`.
- Measure peak memory (`/usr/bin/time -l`) on runs over big inputs.
- `aqr --stats` tells, after each archive, the time, the pool's use, heap and resident peaks and
  what stands out; `--stats=FILE.json` / `FILE.html` write all that was sampled, again every 10 s
  (layout in `crates/aqr/src/stats.rs`). Run big benchmarks (10gb) with `--stats
  --stats=FILE.json` and read it before reaching for a profiler.
- Benchmark zstd at level 3 or below, never 19 (cached results at 19 may still be cited).
- On big streams prefer structural wins (sharing, probe-guided selection) to slower coding; slow
  modes such as aqr-flate's profile 10 belong to a separate high level, never to big streams by
  default (except where an encoder is predicted exactly and profile 10 is faster for it:
  `crates/aqr-compress/src/dmg.rs`).

## Sources and data

- Several codecs are written clean room, from specifications, black-box oracles or notes — never
  from the implementations' code:
  - LZO: only public specifications (the kernel's `Documentation/staging/lzo.rst`, libarchive's
    BSD lzop reader); liblzo2 and lzop as black boxes. Never read LZO, miniLZO or the kernel's
    compressor sources, nor the installed lzo headers.
  - RAR: the code only from `docs/rar5-decoding.md` and `docs/rar3-decoding.md`. Don't read
    libarchive's RAR readers (have a note written instead), and never unrar's source.
  - bzip2 (libbzip2), LZFSE (Apple's lzfse), zstd's compressor, LHA, ProRes decoders, AZO: black
    boxes only (`docs/crates.md`, "Notes by codec").
- No GPL code is looked at.
- Private data (browser profiles and the like) is measured by the user; don't copy it.
- Design documents: don't repeat what `spec/index.bs` already says; point to the spec section or
  drop the text. Update them (and READMEs) in the same commit as the change.

## Working copy and commits

Other sessions often work in the same working copy at the same time.

- Stage explicit paths only; never `git commit -a` or `git add .`/`-A`.
- For `Cargo.lock`, stage only your own hunks.

**Commit on your own after each substantial step, one commit per coherent step. Never push.**

**Write messages in the usual git format: a short summary on the first line, one blank line,
then the details.** The history has some subjects of several hundred characters; don't imitate
them.

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
