# CLAUDE.md

Aqueous: an archive format and archiver, `aqr`, built around lossless recompression. See
[README.md](README.md) for the overview.

## Layout

- `spec/index.bs` — the format specification (Bikeshed). `make -C spec` builds `index.html`;
  figures are `figures/*.pik` with their SVGs committed. Pushes to `main` deploy it to GitHub
  Pages (`.github/workflows/spec.yml`) under `/spec/`.
- `site/` — the project site, a single page deployed by the same workflow at the Pages root
  (with `droplet.svg`); its downloads are filled in from the latest GitHub release by script.
- `docs/` — design documents, mostly in Korean: `10gb-todo.md` (what is left for the 10 GB
  benchmark), `file-formats.md` (measurements by format), `*-design.md`.
- `crates/` — the core crates
  - `aqr` — the CLI (`aqr -c/-d/-K/-t/-I`); `--ipc` drives it from a frontend (`src/ipc.rs`); `--session` serves a
    compression dialog's questions before it begins (`src/session.rs`).
  - `aqr-ipc` — the frontends' side of `--ipc`: `aqr` run on the paths, what it tells taken.
  - `aqr-win` — Explorer's verbs and a progress window over `aqr --ipc` (Windows).
  - `aqr-win-install` — the installer carrying `target/dist/{aqr,aqr-win}.exe`, packed by its
    `build.rs` with aqr-exe's filters and zstd (Windows).
  - `aqr-l10n` — the frontends' messages (`messages/{en,ko}.toml`, English the source), and
    what the systems read of them: Windows' string table, the bundle's `.lproj/*.strings`.
  - `aqr-mac` — Finder's services, `.aqr` files opened and their progress in Finder over
    `aqr --ipc` (macOS); `bundle.sh` makes `target/dist/Aqueous.app` and its zip.
  - `aqr-formula` — entry formulas (stack-machine programs) and the archive structure
    (`src/aqr.rs`: the archive encoder and decoder).
  - `aqr-compress` — picks a recompressor by magic bytes, makes the formula, stores the archive.
  - `aqr-decompress` — runs the formulas' modules natively and extracts archives.
  - `aqr-unpack` — archives of other formats (ZIP, 7z, tar and its compressors) read into a
    tree for `aqr -d`, which extracts them as Aqueous archives (`crates/aqr/src/extract.rs`).
  - `aqr-pack` — the tree `aqr -c` gathers written as an archive of another format (ZIP, 7z,
    ALZ, EGG, tar and its compressors, single-file streams: `--format FORMAT`, or `-o NAME.EXT`),
    each format's work spread over the pool; liblzma (XZ Utils, built from source) for xz/7z.
  - `aqr-foreign` — what aqr-unpack and aqr-pack share, both ways: the encryptions (ZIP's,
    WinZip's AES, 7-Zip's AES) and times (FILETIME, MS-DOS's).
  - `aqr-common` — shared parts: varints, rANS, bit I/O, heap accounting, the task pool.
    `aqr-num` — numbers printed as text.
  - `aqr-version` — the binaries' version for their build scripts: the tag's (`AQR_VERSION`, set
    by the release workflow) or `git describe`'s, as `env!("AQR_VERSION")` and Windows' `VERSIONINFO`.
  - `aqr-walk` — one syntax walked for coding, decoding and a dry parse (`Io`), the range coder and
    the hashed model of aqr-div3 and aqr-ac3 (their coding is specified in its README).
  - `aqr-bincache` — `cargo aqr`: the release `aqr` built and kept by a hash of its sources.
  - `aqr-api`, `aqr-ld` — the Rust interface of modules and the Wasm linker wrapper.
  - `aqr-modules` — Wasm modules built from the recompressors' code (`src/bin`, `wasm/`,
    `src/known.rs`).
- `codecs/` — the recompressor crates `aqr-*` (flate, png, jpeg, mp3, flac, pdf, zip, sqlite,
  git, lz4, lzma, lzo, bvx, bz2, alz, egg, azo, lha, rar, snappy, zstd, gif, prores, exe, psd, clip, raw, tex, obj, fasta, wrt, bundle,
  asar, inno, ole, vorbis, ogg, mkv, lzx, xpress, lznt1, div3, ac3, …), each with a `README.md` and a `FORMAT.md` describing what it stores; a
  crate with a `FORMAT.md` belongs here. Disk images are aqr-disk's (partition tables, containers,
  the formula; `docs/disk-image-design.md`), their file systems read by aqr-iso, aqr-fat, aqr-ntfs,
  aqr-ext, aqr-apfs and aqr-hfs (no FORMAT.md: aqr-disk's tells). `aqr-flate` underlies everything that uses DEFLATE. The workspace
  takes both directories by glob, so a new crate needs no entry in the root `Cargo.toml`.
- `corpus/` and `cache/` under a crate are gitignored local data (test inputs, cached results
  of slow measurements).
- flate-zoo (DEFLATE encoders for aqr-flate's training and tests) comes from crates.io: the root
  `Cargo.toml`'s `[workspace.dependencies]`, with the backends that do not work with MSVC added
  for other targets. `third_party/` — reference checkouts, gitignored, excluded from the
  workspace.
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
  allocator, time by label, files by kind); both may be given, and are written again every
  10 seconds as it runs and as it is stopped (`state`: live, interrupted, finished; the archive
  not done a `partial` run), so a run killed midway still leaves them. The JSON's layout is in
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
  streams by default. The exception: where an encoder is predicted exactly (zlib's emulation
  missing no token of a sample, its parameters then hinted) and profile 10 is faster for it,
  profile 10 is used at the default level too (compressed DMGs: `crates/aqr-compress/src/dmg.rs`).
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
- aqr-decompress: a known hash runs natively when the application's declared limits hold its
  plan (`needed_by`: the same function the encoder declares with), anything else in wasmtime.
- Whole modules take their inputs at once; part modules (`entry! … windows(…)`, spec "Module
  execution") stream them through windows of fixed pages, so their memory does not grow with
  the data. Big streams' modules are part modules (lzma, bz2, lz4, snappy, git, exe, png, flac,
  pcm, mp3, fasta, bundle, bcj, mszip, flate-set, flate-inflate, prores, div3, ac3); natively lzma, bz2, png,
  mp3, flac and pcm still decode whole on threads (`Run::Threaded`), not held to the plan.
- No variable arity: use Split and separate modules (png-against, leveldb-recons, …).
- Model data (aqr-flate's) is a resource, not embedded in the module.
- Threads in Wasm: fork-join tasks (`aqr_spawn(class, count, records, arg, n)`, the only spawn;
  `aqr_join`/`aqr_take`; `aqr_common::pool::par_map_apart` with `wire::Wire`) and channels
  (flac/pcm pipelines, prores's stripes); the spec's "Module execution" has the rules, `docs/xform-design.md` §5.5
  the design. aqr-api's `task::spawn` is class 0 with one region record for all of the working
  memory; `spawn_class(class, ..)` and `spawn_gang(class, ..)` name the class. The spawn record
  must stay in a never-inlined frame (`spawn_record`), or the task's stack clobbers the caller's
  locals. A module spawning tasks declares its classes in the custom section `aqr_task_slots`
  (one byte each: slots 1..=64 in the low 7 bits, a gang class with the top bit; aqr-ld checks
  it): `whole!(slots N, …)` for one class of N slots, its code given N as its threads, or
  `whole!(gang N, …)` for one gang class of a slot, whose gang is N tasks. A gang's tasks start
  together and its limit covers their memories and the messages queued in its channels; the
  channels made (`aqr_chan`) belong to the instance's next spawn, which must be of a gang class
  (the emulation checks the same, by `spawn_gang`), and only that gang's tasks and the spawner
  take their ends. The decoder picks each class's concurrency from 1 to its slots
  (`choose_concurrency`: lowered to fit `-M` when one is given). The earlier two-parameter
  `aqr_spawn` and `Apply` of opcode 11 are for the modules of releases only. Other
  `par_map`/`scope` uses run serially in Wasm. Port a pipeline only when its speedup is worth
  the module's growth; report both (png, psd and mp3 were measured and not ported).
- Memory: each Module carries the exact plan of its working memory (`Module::new(..).with_plan`,
  or `with_task_plan` for a module spawning tasks; the codec's `formula.rs`, `aqr_api::stack`,
  `docs/arena-design.md`): the limit of the application's instance, and of a task of each class.
  aqr-compress declares them (`crates/aqr-compress/src/memory.rs`) in the Apply instruction
  (always opcode 16, one task limit for each class of the module). An application over
  `--application-bound` (256 MiB compressing, 512 MiB decompressing) is not used / fails; a
  recompression runs under a heap allowance (`aqr_common::allowance`), each format's `heap` in
  aqr-compress (`src/formats.rs`) turning inputs away up front. A module allocates on the stack
  in the order its plan follows; changing what it allocates means changing its plan, which the
  `*_plans_hold` tests in aqr-decompress's `tests/wasm.rs` check to the page, instance and each class
  (`plans_hold_on_samples` in `tests/samples.rs`, ignored, does on `scripts/module-memory.sh`'s
  samples). Tasks spawn only classes above their own.
- Released modules: the earlier Apply (opcode 11, reserved) is accepted only for the modules of
  the releases (`crates/aqr-formula/src/released.rs`, the hashes that `known.rs` no longer has),
  and its memory limit is not enforced. At each release add the tag to the revisions of
  `scripts/released-modules.sh`, run it and commit the result.
- Size: aqr-ld trims modules itself (prefer automating in aqr-ld over toolchain flags);
  `AQR_LD_NAMES=1` keeps the name section for size debugging. Module error types must
  implement `Display` (`Debug` of `&str` pulls in core's Unicode tables).
- Module bytes are a per-archive cost that dominates small archives. Planned: built-in
  dictionaries in the spec (u8-sized module blocks as hash references, dictionary blocks as
  references to a built-in dictionary; two 64 KiB dictionaries, module and text, about 60 KB
  in all). A text dictionary needs natural language as well as code; a trained dictionary's
  entropy tables hurt CJK, so the compressor must be able to use it as raw content too.

## Project notes

- Archives of several files are committed as they are written and appended to by `-a` (spec,
  "Appending to an archive"; `docs/append-design.md`): a commit writes the segments and a new
  footer after the old one, then makes the old one a spacer block by its first 2 bytes; a stop
  (first Ctrl-C) ends the archive as it is, telling what it left out (`segment-status`). Not
  with `-P` (nonces) nor `-A` (Mirroring owns the footer), which end as they are but do not commit.
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
  README's Status): decmpfs, an image model for the data.
- aqr-gif: the LZW codes coded given the pixels against the longest match (every encoder seen
  writes it), the pixels by a context-mixing model of palette indices (lpaq's style; integers
  only, so Wasm and native agree). Left (its README's Status): frames against the image shown,
  TIFF's and PDF's LZW.
- aqr-bz2 reimplements libbzip2's compressor (1.0.3 on), written from memory of its behaviour;
  libbzip2's code is not read (BSD-style). Python's `bz2` and a shared libbz2
  (`BZ2_hbMakeCodeLengths`) are the black boxes. Equal rotations of periodic blocks order
  otherwise, so their `origPtr` goes in the params. Left (its README's Status): lbzip2 and
  others, bzip2 in ZIP/7z, tasks in Wasm.
- aqr-azo (AZO, ALZip's compressor in EGG archives): a record of the parse, as aqr-lzo, the
  stream coded again by the spec's models (OctopusET's EGG spec, Appendix A; ALZip's output the
  black box). The x86 filter is not known (the spec's does not give ALZip's CRC-32): filtered
  streams are rebuilt with their data as the blocks decode. Left (its README's Status).
- aqr-lha (LHA's `-lh4-` to `-lh7-`): a record of the parse, as aqr-lzo, with Okumura's LZSS
  tree as a second match finder; every compressor seen writes the headers of ar002's Huffman
  coder (predicted from the tokens) and cuts blocks by its token buffer. No LHA code is read;
  lhasa's test archives and Aminet are the black boxes (Debian's `lha`, LHa for UNIX, would be
  an oracle in the Linux VM). Left (its README's Status): `-lh1-`, archives in other data.
- aqr-rar (RAR5, and RAR 3.x/4.x's RAR 2.9 algorithm: LZ, PPMd, the standard filters): a record
  of the parse, as aqr-lzo. Clean room: `docs/rar5-decoding.md` and `docs/rar3-decoding.md` were
  written by agents that read libarchive (BSD) and wrote no code; the code is written from those
  notes only. Don't read libarchive's RAR readers when changing aqr-rar's code (have a note
  written instead), and never unrar's source (its licence forbids RAR compressors). The PPMd
  model is ported from lzma-sdk (public domain). rar 7.23 and 6.24 (`third_party/rar-dl`,
  6.24 for `-ma4`) are the black boxes. Left (its README's Status): volumes, SFX checks, older
  WinRARs' tables, RAR5's parse (most of its record), speed.
- aqr-prores (Apple ProRes in QuickTime or MXF): the frames found by their `icpf` (no container
  parsed), parsed to coefficients and written again (every coefficient set has one coding); a
  context model with the previous frame as context, in chunks of 64 MiB. It is a part module
  (`prores-recons` 3): the coded chunk is pieces for each frame (side info; one for each
  stripe of rows, a stripe for each 4 MiB of frames, at most 16, and for 16 KiB of a frame), a
  gang of one task for each stripe lives through the chunk, the instance sending each its
  pieces and taking the slices to write the frame, so its plan follows the frames, not the
  chunk. Natively the same code runs through the emulation of aqr-api (`Run::Part(entry, None)`).
  Written from RDD 36's description; no decoder's code read. Left (its
  README's Status): decoding speed (decisions per byte), alpha's run-lengths, Apple-encoded
  samples (AVFoundation, iPhone).
- aqr-snappy and aqr-lz4 reimplement the encoders: Chrome's LevelDB Snappy is the
  multiplicative-hash build (Homebrew's arm64 builds use the CRC hash and do not match);
  Firefox's mozlz4 is liblz4's `LZ4_compress_default`.
- aqr-zstd: Zstandard frames are not rebuilt (it would gain little); a `.zst` file is kept as it
  is and the files of its batch that its content holds (the whole, a tar archive's members) are
  taken from its frames, decompressed by Patch instructions of an empty source and split
  (`Formula::split_patch`, `crates/aqr-compress/src/zst.rs`). No module. Left (its README's Status).
- aqr-git is the pack recompressor `git-pack-recons`, at aqr-flate's profile as aqr-compress
  picks it; a pack with its `.idx`/`.rev` is a set (`git-pack-index-recons`, only the ids
  stored). Left: deriving the ids (resolving every delta), `multi-pack-index` from the indexes,
  the archive's 16 MiB solid-block bound, pack blobs in `together`.
- aqr-jpeg's lossless JPEG (SOF3: the pixels of CR2, DNG and the like) is the module
  `jpeg-lossless-recons`, apart from `jpeg-recons`: the streams are found by their SOI in a
  TIFF file (`lformula::starts`), their Huffman data taken out and the samples coded by
  `lmodel` (a blend of predictors, a Golomb code by adaptive bits; integers only). A big stream
  is cut in bands of rows, each its own chunk (about 1M samples), to run at once; the small ones
  (a DNG's tiles) share one model. It is a part module (`lpart`): the file is put as it comes,
  the rows of a frame decoded and their Huffman data written a row of MCUs at a time
  (`lmodel::Frame`, `lossless::Emitter`; a band's tasks make the raw bits of its codes, the
  parent shifts them in: no restart intervals in a banded frame), so its plan (`lpart::plan`, to the page,
  `lossless_jpeg_plan_holds*`) follows the width of the frames and the size of a chunk, not the
  file (2-4 MiB for a 62 MB CR2). A header holds its lists in place (`Small`): the module's
  allocator takes only 64 blocks freed out of order. Natively it is applied whole on threads
  (`lmodule::decode`). Samples (CC0): raw.pixls.us, in `F:\Data\aqr-corpus\aqr-jpeg-lossless`.
  Left: the speed of one thread, mixing of models, and other containers (the groups of tiles are
  tasks as the bands are, 8 at a time, a round of up to 16 jobs with its script).

## Working copy and commits

Other sessions often work in the same working copy at the same time.

- Stage explicit paths only; never `git commit -a` or `git add .`/`-A`.
- For `Cargo.lock`, stage only your own hunks.

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
