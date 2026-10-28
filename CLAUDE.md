# CLAUDE.md

Aqueous: an archive format and archiver, `aqr`, built around lossless recompression. See
[README.md](README.md) for the overview.

## Layout

- `spec/index.bs` — the format specification (Bikeshed). `make -C spec` builds `index.html`;
  figures are `figures/*.pik` with their SVGs committed. Pushes to `main` deploy it to GitHub
  Pages (`.github/workflows/spec.yml`).
- `docs/` — design documents, mostly in Korean: `10gb-todo.md` (what is left for the 10 GB
  benchmark), `file-formats.md` (measurements by format), `*-design.md`. Don't repeat in a
  design document what the spec already says; point to the spec. Where they disagree, the
  spec wins.
- Core crates
  - `aqr` — the CLI (`aqr -c/-t/-d`).
  - `aqr-formula` — entry formulas (stack-machine programs) and the archive structure.
  - `aqr-compress` — picks a recompressor by magic bytes, makes the formula, stores the archive.
  - `aqr-decompress` — runs the formulas' modules natively and extracts archives.
  - `aqr-common` — shared parts: varints, rANS, bit I/O, heap accounting. `aqr-num` — numbers
    printed as text.
  - `aqr-api`, `aqr-ld` — the Rust interface of modules and the Wasm linker wrapper.
  - `aqr-modules` — Wasm modules built from the recompressors' code (`src/bin`, `wasm/`,
    `src/known.rs`).
- Recompressor crates `aqr-*` (flate, png, jpeg, mp3, flac, pdf, zip, sqlite, git, lz4, lzma,
  lzo, snappy, exe, psd, clip, raw, tex, obj, fasta, wrt, bundle, …), each with a `README.md`
  and a `FORMAT.md` describing what it stores. `aqr-flate` underlies everything that uses
  DEFLATE.
- `corpus/` and `cache/` under a crate are gitignored local data (test inputs, cached results
  of slow measurements).
- `flate-zoo/` — a submodule (a workspace of its own). `third_party/` — reference checkouts,
  gitignored. Both are excluded from the workspace.
- `scripts/` — helper scripts (Python uses the `.venv` at the root).

## Development notes

- Test with `cargo rt -p <crate>` (`.cargo/config.toml`: `cargo test --profile release-test`,
  optimised without the fat LTO of `-r`). Codec tests in debug builds are very slow. Timings
  (`--ignored` tests) still want `-r`. Test the crates you changed rather than the whole
  workspace.
- Format with the `rustfmt.toml`s (width 120).
- After changing code that goes into modules (a recompressor's reconstruction path and the
  like), rebuild them with `aqr-modules/build.sh` so that the hashes in `known.rs` and the
  table in `spec/index.bs` match. If you don't rebuild, say so in the commit message.
- Performance in numbers: measure speed and size before and after, and record them in the
  commit message. Prefer `dyn` over generics off the hot paths; monomorphize only where a
  measurement shows the gain.
- Cache the results of measurements taking over a minute in `cache/`. Compare against zstd at
  level 3 or below (never 19).
- Don't look at GPL code (LZO, for one): implement clean-room from specifications and
  black-box testing only.
- Never stage a change of the `flate-zoo` submodule pointer.
- The disk is small: `cargo clean` `target/` now and then after big runs.

## Commits

**Commit on your own after each substantial step. Never push.**

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
