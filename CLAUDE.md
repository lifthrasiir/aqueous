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
  benchmark), `edge-tests-todo.md` (the edge cases left untested), `file-formats.md`
  (measurements by format), `*-design.md`, and `glossary.md` (in English: the words of the
  code and the spec not obvious at a glance).
- `crates/` — the core crates
  - `aqr` — the CLI (`aqr -c/-d/-K/-t/-I`); `--ipc` drives it from a frontend (`src/ipc.rs`); `--session` serves a
    compression dialog's questions before it begins (`src/session.rs`); `--serve` serves a shell
    extension the entries and files of archives, leaves evaluated kept (`src/serve.rs`,
    `docs/win-shell-ext-design.md`). `--format sfx` (`src/sfx.rs`,
    Windows x86-64) makes a self-extracting program: the archive with shorthand module blocks for
    the modules that have native blobs, after aqr-sfx-shim (packed by UPX) and the bundle of
    those blobs (and of the interpreter blob of `-Z wasm:engine`, WAH by default, if any module is
    in full: aqr-sfx's `bundle`), and
    what the program does once run where the input's root has `.aqr/sfx/sfx.toml`
    (`docs/sfx-config-design.md`: read back of the archive made, compiled by aqr-sfx's `config`
    into a record of CBOR before the archive, and the stub's icon, `VERSIONINFO`, manifest and
    subsystem written by its `pe`, as a section after UPX's). What it bundles is `target/aqr-sfx/` (or
    `AQR_SFX_PACK`), which `crates/aqr-sfx/pack.py` gathers and packs (`sfx.pack`, the blobs as
    bundles of LZMA2, ~2.9 MB: only aqr reads it; each interpreter blob a bundle of its own, WAH's
    always and Wasmtime's with `pack.py --engines=wah,wasmtime`, +2.2 MB, which aqr picks by
    `-Z wasm:engine`); it is no part of aqr's build but written into
    the linked executable as resources (`aqr-sfx-pack --embed DIR EXE`, aqr-sfx's `resource`, read
    in place as it runs): `cargo aqr` writes it into the binary it keeps (`pack.id` in its
    manifest), aqr's tests/sfx.rs into a copy of theirs. Each blob is used only for the hash it
    was packed for, so `cargo aqr` runs pack.py again where the pack's modules are not those of
    known.rs (where there is a pack), or its `sfx.pack` is not of this form. Without the pack,
    aqr.exe runs no module as WebAssembly. The release workflow builds the stub and the modules'
    blobs (`pack.py --part=modules`), the interpreter blob (`--part=interp`) and the binaries in
    three jobs at once, then packs (`--part=pack`), writes the pack into aqr.exe, smoke-tests
    `-Z wasm` and an SFX, and builds the installer. Every Windows
    binary of a release (aqr, aqr-win, the installer, the stub, the blobs) is of the target of
    Windows 7 on (`cargo +NIGHTLY win7`, `.cargo/config.toml`; pack.py's stub for
    `x86_64-win7-windows-msvc`): what newer systems add is looked up as it runs (the sandbox's
    mitigations, aqr-wasmrt's placeholders), and std's SRW locks were no slower (10gb at -5).
    Development builds (`cargo aqr`, the tests) stay of `x86_64-pc-windows-msvc`.
  - `aqr-ipc` — the frontends' side of `--ipc`: `aqr` run on the paths, what it tells taken.
  - `aqr-win` — Explorer's verbs and a progress window over `aqr --ipc` (Windows).
  - `aqr-win-install` — the installer carrying `target/dist/{aqr,aqr-win}.exe`, packed by its
    `build.rs` with aqr-exe's filters and zstd (Windows).
  - `aqr-l10n` — the frontends' messages (`messages/{en,ko}.toml`, English the source; a table
    of each section, so that the SFX stub carries only its `sfx`), and what the systems read of
    them: Windows' string table, the bundle's `.lproj/*.strings`.
  - `aqr-mac` — Finder's services, `.aqr` files opened and their progress in Finder over
    `aqr --ipc` (macOS); `bundle.sh` makes `target/dist/Aqueous.app` and its zip.
  - `aqr-formula` — entry formulas as recompressors declare them (`Formula`, `Node`, `Apply`,
    `Module` and its plan), with what they need to make them: the spill `store` of large
    buffers, `patch`es, the `probe` of whether recompressing pays (the archive encoder behind
    its `Archiver`). The codec crates depend on it alone; nothing of the archive is in it.
  - `aqr-archive` — the archive structure: the archive encoder (`src/encoder.rs`, `emit.rs`,
    `compress.rs`) and decoder (`open.rs`, `parse.rs`, `program.rs`), the
    evaluation of formulas (`Evaluate`, `Modules`: by the decoder's evaluator of formula
    blocks, `src/eval.rs`), metadata trees, segments, envelopes, mirroring, released modules.
    Of the codecs' libraries only aqr-mkv (`description_bound`) and aqr-tensors (`BLOCK`) take
    it; their tests and tools (`examples/aqr-NAME.rs`) take it as a dev-dependency.
  - `aqr-compress` — picks a recompressor by magic bytes, makes the formula, stores the archive;
    the hints of attributes by path (`attributes`, `hints`: `--attributes`, `.aqr/attributes`,
    `.gitattributes`; docs/hints-design.md) steer it, a hinted file's kind `Kind::Hint`.
  - `aqr-decompress` — runs the formulas' modules natively and extracts archives.
  - `aqr-native` — how each module runs natively (`NATIVE`, by module name: its native decoding
    of whole inputs, threaded where it is, or its part entry point, and when to stream), from the
    codec crates' decoding code alone (no `encode`); aqr-decompress pairs its `Module`s with it,
    aqr-dll builds each module's library with its entry.
  - `aqr-unpack` — archives of other formats (ZIP, 7z, tar and its compressors) read into a
    tree for `aqr -d`, which extracts them as Aqueous archives (`crates/aqr/src/extract.rs`).
  - `aqr-pack` — the tree `aqr -c` gathers written as an archive of another format (ZIP, 7z,
    ALZ, EGG, tar and its compressors, single-file streams: `--format FORMAT`, or `-o NAME.EXT`),
    each format's work spread over the pool; liblzma (XZ Utils, built from source) for xz/7z.
  - `aqr-foreign` — what aqr-unpack and aqr-pack share, both ways: the encryptions (ZIP's,
    WinZip's AES, 7-Zip's AES) and times (FILETIME, MS-DOS's).
  - `aqr-common` — shared parts: varints, rANS, bit I/O, heap accounting, the task pool.
  - `aqr-testkit` — what the tests share (a dev-dependency): inputs damaged deterministically, a
    bound on a case's time, an allocator refusing large requests (`Refusing`).
    `aqr-num` — numbers printed as text.
  - `aqr-version` — the binaries' version for their build scripts: the tag's (`AQR_VERSION`, set
    by the release workflow) or `git describe`'s, as `env!("AQR_VERSION")` and Windows' `VERSIONINFO`.
  - `aqr-walk` — one syntax walked for coding, decoding and a dry parse (`Io`), the range coder and
    the hashed model of aqr-div3 and aqr-ac3 (their coding is specified in its README).
  - `aqr-bincache` — `cargo aqr`, `cargo aqr-win`: the release `aqr` (and `aqr-win`, with the
    kept `aqr` beside it) built and kept by a hash of its sources.
  - `aqr-api`, `aqr-ld` — the Rust interface of modules and the Wasm linker wrapper; aqr-ld also
    (`src/pemerge.rs`) has `blob`, which digests a module library into a blob (just the bytes and
    facts a merge needs: image from the first section, no PE headers/`.reloc`/header gap, dead
    directories zeroed); `blobs`, which digests a set (modules and the shared libraries they link
    against) with the imports between them bound by name to export RVAs; and `merge`, which merges
    blobs (or libraries) into a stub executable as one section with no loader — code copied and
    rebased, bound imports written as addresses (relocated), the others merged into the stub's
    table, a dispatch table added (`demo/{stub,modadd,modxor}`, `pemerge-test.sh`); a library too
    (`merge_bytes`). `src/msvc.rs`: the MSVC linker of aqr-dll's no-TLS
    target, cutting a shared library's exports to a list (`AQR_LD_EXPORTS`, made by `exports`) so
    that `/OPT:REF` drops the rest, its import library still of every export; libraries
    linked with no entry point (`/NOENTRY`: the C runtime's DLL startup and its imports left out).
  - `aqr-wasmrt` — runs the Wasm modules of aqr natively by WAH or Wasmtime (`wasm::load`), in
    the process or in a sandbox worker (`sandbox::Sandbox`, told the `-Z wasm:` options set), with
    no native codecs linked. Shared by aqr-decompress (for modules it does not know natively) and
    the self-extracting interpreter blob. `Sandboxed` (native where known, else a worker) stays in
    aqr-decompress, which has the native modules. An engine is linked only with the feature `wah`
    (`wasm/wah_engine.rs`, WAH from crates.io) or `wasmtime` (`wasm/engine.rs`), or both
    (`wasm/both.rs`: `-Z wasm:engine`, Wasmtime by default as the faster linked; the feature
    `engine` is either); WAH always but on Windows x86-64 — aqr-decompress enabling it there, and
    in the tests' dev-dependencies (`--features wasmtime` for both). Else `wasm/interp.rs` runs the
    modules by the interpreter blob the program gives
    (`wasm::set_interpreter`, aqr: its pack's), placed at run time (`aqr_blob::load`), a worker
    served by the blob's own (`worker_main`). aqr.exe on Windows links no engine (41.9 ->
    30.4 MB). What reads a module's bytes alone, and the bound, is `wasm.rs`'s, for all.
  - `aqr-blob` — blobs: their format (aqr-ld makes them), `load` placing a set in memory at run
    time (Windows x86-64: relocated, imports between them and from system libraries bound,
    exception tables registered), and (feature `runner`) the interpreter blob's client: its
    `Vtable`, `Interpreter` (a `Runner`), failures crossing the C ABI with their kinds.
    Windows binaries link the Visual C++ runtime statically (`.cargo/config.toml`: the release's,
    of the win7 target, and the SFX stub the UCRT too; development builds the UCRT as Windows
    10's DLL), so no redistributable
    is needed; the blobs still import vcruntime140.dll and the UCRT's DLLs (a static copy would
    go uninitialised: no DllMain runs), and `load` and `aqr-ld merge` bind those imports to the
    program's own (`CRT_TABLE`, `CRT_NAMES`: `aqr-ld blob(s)` fail on a function not there), with a
    `_CxxThrowException` of its own: a panic's type information is relative to its blob's
    RVA 0, which it takes from the images registered (`register_image`: `load`, and the stubs
    for the blobs merged into them). A merge moves the blobs' function entries into the stub's
    exception directory, their unwind data rebased (aqr-ld's `src/unwind.rs`): the unwinder
    reads only an image's own for its addresses. `pemerge-test.sh` checks panics both ways.
  - `aqr-sfx` — the self-extracting apply logic (a library): `Sfx` finds the blobs merged into the
    program through the dispatch table's anchor — native module blobs named `HASH/SLOTS`
    (`native_name`: the module's hash and task slots), the interpreter blob `aqr-interp` — and is
    the `Runner`: a shorthand block's hash gives a stand-in Wasm module (no code, the task slots
    and the hash in custom sections), which `load` takes to the native blob (a part module
    streamed through `aqr_dll_apply_part`, as `Info::apply_part` gives it); any other module
    goes to the interpreter. The shared libraries the native blobs link against are
    entries `lib:NAME` (their images registered, nothing else). `Sfx::load` takes the same
    blobs from a bundle (`bundle`: each blob's code, up to the end of its exception table's
    last function, through aqr-exe's x86 disfilter, the streams of all the blobs one after
    another by stream and the rest apart, each a zstd frame at 19; feature `encode` makes them;
    feature `xz` the same of raw LZMA2 frames, for the pack aqr carries alone, not the stub),
    placed by `aqr_blob::load`. `extract` writes a decoded archive's files out (`extract_with`:
    as the settings write over what is there, the made noted for `clean`). Shared by the
    stub and aqr. `pack.py` gathers what aqr embeds (the stub, packed by UPX `-9` where `AQR_UPX` or
    `upx` is found, the blobs of the modules and of the shared libraries) and packs it
    (`aqr-sfx-pack`: `sfx.pack`).
  - `aqr-sfx-shim` — the self-extracting stub (lean: aqr-archive + aqr-sfx, no codecs or engine,
    0.78 MB, 0.35 MB packed by UPX: the profile `sfx`, panic=abort): the small executable that `aqr --format sfx` appends a bundle
    of blobs and an archive to (or that `aqr-ld merge` fills with blobs). It takes the bundle, the
    settings and the archive from right after its image (else it scans itself from the end for
    the archive's header magic, the settings right before it), decodes the archive running the
    modules through the blobs, writes the files out, makes the shortcuts (`src/shortcut.rs`) and
    runs the program the settings tell (`src/run.rs`; its questions, messages and license dialog
    in boxes where it has no console, `src/ui.rs`, under the settings' banner, `src/banner.rs`;
    the folder picked, `src/folder.rs`).
  - `aqr-interp` — the interpreter blob: a module library (`aqr_interp_vtable`, aqr-blob's
    `Vtable`: load, apply, apply_part, and for aqr the loads of aqr-wasmrt, the bound and the
    sandbox's worker) that runs any Wasm module handed to it by WAH (`aqr-wasmrt`; Wasmtime with
    `--no-default-features --features wasmtime`), no codecs linked (2.5 MB; 12.3 MB of Wasmtime).
    Built for the no-TLS target (`NOTLS`) it is mergeable into a stub: the fallback for modules the
    stub has no native blob for.
  - `aqr-dll` — a module's binary built as a native library (`AQR_DLL_MODULE`; `build.py` builds
    them into `target/aqr-dll/`) exporting `aqr_api::dll`, toward self-extracting archives. The
    crates that several modules use are Rust dylibs it links against (`layout.py`, run by
    build.py, writes `shared/NAME/`, `shared.rs` and its Cargo.toml's block: one dylib for each
    set of modules using the same crates, the runtime `aqr-dll-rt`, std and aqr-api, used by all);
    building every module, build.py links them again until each exports only what the others
    import (aqr-ld's `exports`): all modules and libraries 8 MB as blobs, not 25 MB;
    `aqr-dll-shim` records applications from archives and runs the libraries on them (its own, or
    the ones merged into it). `NOTLS=1 build.py` builds for `x86_64-aqr-windows-msvc.json` (native
    thread-locals off, so std uses TlsAlloc, not a TLS directory; vendor `win7`, so that std uses
    no API of Windows 8 on: one set of blobs and one stub for every Windows), which `aqr-ld
    merge` can take;
    `merge-test.sh ARCHIVE...` is the end-to-end test (needs nightly and build-std). The nightly of
    these builds (and of the release's Windows binaries) is pinned by the root file `nightly-toolchain`
    (`AQR_NIGHTLY` overrides it), as the custom target's JSON is unstable.
  - `aqr-modules` — Wasm modules built from the recompressors' code (`src/bin`, `wasm/`,
    `src/known.rs`).
- `codecs/` — the recompressor crates `aqr-*` (flate, png, jpeg, mp3, flac, pdf, zip, sqlite,
  git, minecraft, lz4, lzma, lzo, bvx, bz2, alz, egg, azo, lha, rar, snappy, zstd, gif, prores, exe, psd, clip, raw, tex, obj, fasta, wrt, bundle,
  asar, inno, ole, isc, vorbis, ogg, wwise, ima, msadpcm, mkv, lzx, xpress, lznt1, div3, ac3, tensors, log, b2t, …), each with a `README.md` and a `FORMAT.md` describing what it stores; a
  crate with a `FORMAT.md` belongs here. Disk images are aqr-disk's (partition tables, containers,
  the formula; `docs/disk-image-design.md`), their file systems read by aqr-iso, aqr-fat, aqr-ntfs,
  aqr-ext, aqr-apfs and aqr-hfs (no FORMAT.md: aqr-disk's tells). Game engines' and middleware's packs are
  aqr-bundle's `packs` (aqr-disk's formula, no module), formats named `<engine>-<extension>` or the
  middleware's or tool's (`godot-pck`, `unity-assets`, `unreal-upk`, `wwise-bnk`, `cri-awb`,
  `bethesda-ba2`, …: `packs/mod.rs`'s `packs!`); aqr-compress's `siblings.rs` plans the files of no
  header taken apart by others archived along (a VPK's archives, Unity's `.resS`). `aqr-flate` underlies everything that uses DEFLATE. The workspace
  takes both directories by glob, so a new crate needs no entry in `members`; the versions and
  paths of the members' dependencies are all in the root's `[workspace.dependencies]` (a crate
  that others take gets an entry there), the members saying `NAME.workspace = true` (adding
  `features`, `optional`, or `default-features = true` where the root turns them off).
- `corpus/` and `cache/` under a crate are gitignored local data (test inputs, cached results
  of slow measurements).
- flate-zoo (DEFLATE encoders for aqr-flate's training and tests) comes from crates.io: the root
  `Cargo.toml`'s `[workspace.dependencies]`, with the backends that do not work with MSVC added
  for other targets. `third_party/` — reference checkouts, gitignored, excluded from the
  workspace.
- `scripts/` — helper scripts (Python uses the `.venv` at the root).
- `fuzz/` — cargo-fuzz targets of the archive decoder (`decode`, `salvage`, `envelope`), of the
  encoder's round trip (`roundtrip`) of archives of other formats (`unpack`, and written by aqr-pack: `pack`), of modules not known (`modules`), of self-extracting programs' data (`sfx`) and of the codecs (`natives`, `wasm`,
  `compress`; nightly), a workspace of its own; aqr-archive skips its checksums
  under `cfg(fuzzing)` (`sum_holds`; encrypted chunks' tags as `Unlocked::opens` tells).
  `fuzz/run.sh TARGET` runs one with its seeds and dictionary (its README).

## Development notes

- Test with `cargo rt -p <crate>` (`.cargo/config.toml`: `cargo test --profile release-test`,
  optimised and incremental). Codec tests in debug builds are very slow (a debug
  `cargo test --workspace` once ran over 12 minutes). Timings (`--ignored` tests) still want
  `-r`. Test the crates you changed rather than the whole workspace; for the archive encoder
  and decoder that is aqr-archive, aqr-compress and aqr-decompress. Stream long output with
  `rg --line-buffered` rather than through `sort`, which hides progress. `cargo nextest run
  --cargo-profile release-test …` (no doctests) runs the tests of all binaries at once, about 3
  times as fast over many packages. CI (`.github/workflows/test.yml`: Linux and Windows, macOS
  by schedule and by hand) tests what `scripts/affected-crates.py BASE` selects: the packages
  changed and those depending on them.
  A test reading another package's files names them in its source by a relative path
  (`"../../codecs/aqr-jpeg/tests/data/…"`) or a path from the root (`fixture("codecs/aqr-7z/…")`),
  up to a `{` of a format string at least; else the script misses it
  (`scripts/affected-crates.py --readers PATH` tells who it finds).
- **Build and run the release `aqr` only through `cargo aqr <args>…`** (crates/aqr-bincache),
  never `cargo build -p aqr`, `cargo run -p aqr` or `target/release/aqr` directly: it builds
  as needed and keeps each binary by a hash of its sources, so a rebuild for a source tree
  already built is skipped and earlier binaries stay at hand. It also runs
  `crates/aqr-modules/build.py` (and pack.py) first where what the modules are made of changed
  (`src/modules.rs`; `AQR_BINCACHE_MODULES=0` skips it).
  `cargo aqr +` lists the kept binaries; `cargo aqr +<id> <args>…` runs an earlier one, e.g. as
  the "before" of a comparison. `cargo aqr-win <args>…` does the same for aqr-win, the kept
  `aqr` beside it: `cargo aqr-win --register` puts Explorer's verbs on the development build
  (that binary pinned in the cache until `--unregister`).
- Format with `cargo fmt` (the root's `rustfmt.toml`: width 120).
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
  specialized natively (4–8% each otherwise; in Wasm one table of any shape, `AnyTable`),
  while Adaptive uses `dyn Search` (`Corr::SPECIALIZED = false`).
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

- Codec crates: feature `encode` (default) brings in `formula`, aqr-formula, zstd and rayon
  (never aqr-archive: their tests and tools take it as a dev-dependency);
  `module.rs` is the decoding code without aqr-formula. Natively they stay plain libraries.
- aqr-modules: one `[[bin]]` per module (never one module doing several things). `build.py`
  builds them through aqr-ld and writes `wasm/*.wasm`, `*.block` (the module blocks' data),
  `src/known.rs` (name, version, SHA-512/256) and the registry's table in `spec/index.bs`.
  Every host builds the same bytes: aqr-ld, as rustc's wrapper there, makes the crates' `-C
  metadata` (of their paths, with the host's separators, by Cargo) of no path (`src/metadata.rs`).
- **After changing any code a module links, commit the modules rebuilt**: `cargo aqr` runs
  `crates/aqr-modules/build.py` where that code changed and names the modules it changed
  (run build.py by hand otherwise); a test in aqr-decompress catches stale ones. If you don't
  rebuild, say so in the commit message.
- aqr-decompress: a known hash runs natively when the application's declared limits hold its
  plan (`needed_by`: the same function the encoder declares with), anything else as WebAssembly.
- Whole modules take their inputs at once; part modules (`entry! … windows(…)`, spec "Module
  execution") stream them through windows of fixed pages, so their memory does not grow with
  the data. Big streams' modules are part modules (lzma, bz2, lz4, snappy, git, exe, png, flac,
  pcm, mp3, fasta, bundle, bcj, mszip, lzx, flate-set, flate-inflate, prores, div3, ac3, mca); natively lzma, lzx, bz2, png,
  mp3, flac, pcm and mca still decode whole on threads (`Run::Threaded`), not held to the plan.
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
  the releases (`crates/aqr-archive/src/released.rs`, the hashes that `known.rs` no longer has),
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
- aqr-archive's decoder (`defined()` in `src/parse.rs`) does not accept import blocks 58–5F,
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
- aqr-zstd: a `.zst` file's frames are not rebuilt (it would gain little); it is kept as it is
  and the files of its batch that its content holds (the whole, a tar archive's members) are
  taken from its frames, decompressed by Patch instructions of an empty source and split
  (`Formula::split_patch`, `crates/aqr-compress/src/zst.rs`). `compress` emulates libzstd's
  compressor at level 3 (the double-fast strategy, contents under 256 KiB), written from its
  behaviour with libzstd as the black box, not its code: the module `zstd-frames` (a part module)
  makes frames in other data again of their contents, their pieces anywhere in the file, for
  SQLite databases of zstd blobs (aqr-compress's `sqlite-zstd`, aqr-sqlite's `blobs`; Distant
  Horizons' LODs). Left (its README's Status).
- aqr-git is the pack recompressor `git-pack-recons`, at aqr-flate's profile as aqr-compress
  picks it; a pack with its `.idx`/`.rev` is a set (`git-pack-index-recons`, only the ids
  stored). Left: deriving the ids (resolving every delta), `multi-pack-index` from the indexes,
  the archive's 16 MiB solid-block bound, pack blobs in `together`.
- aqr-tensors (files of tensors: GGUF, `torch.save`'s ZIP format, safetensors; the formats `gguf`,
  `pth`, `safetensors` of aqr-compress, one recompressor): the tensor data in segments of 16 MiB,
  each an application of the part module `tensors-recons`: floats' exponents and quantised
  blocks' nibbles (and Q6_K's crumbs, the high bits of Q8_0 and Q5_*) as symbols of a static
  order-0 rANS of 16 symbols, 32 states (SIMD: AVX2 natively, SIMD128 in Wasm, no gather), the
  rest as it is. Weights converted from a lower precision (NVFP4, MXFP4, FP8, NF4, GPTQ, INT8 to
  BF16/F16/F32) are found by a sample of the tensor and coded as a grid's indices times scales of
  tiles fitted exactly (`src/grid.rs`); the mantissa's top bits are symbols only where the sample
  says they pay; integers and packed bytes as lanes. Every dtype of the containers is taken. A
  `.pth` is read by its central directory and the persistent ids of its pickle
  (a small machine of the opcodes, nothing else of the pickle). Speed is the point (the files
  are tens of GB): aim at 500 MB/s through `aqr` (492 MB/s on tenz's 11 models, 2026-11-14). What
  bounds it now is the archive's: the coded streams probed by zstd whole (`How::Light`), and each
  segment's side stream planned by itself (its README's Status). Samples are in
  `F:\Software\bai-models`, `F:\Software\lmstudio-models` and `F:\Data\sam_vit_h_4b8939.pth`
  (F32), used in place (never copied); safetensors from Hugging Face (and checkpoints
  dequantised from them) and the models of tenz's README (github.com/nsiddiqui248/tenz, a GGUF
  compressor of context models: about 4 points smaller than aqr-tensors on quantised GGUF) in
  `F:\Data\aqr-corpus\tensors`. Left (its README's Status).
- aqr-log (lines of text: logs, tables of numbers; `log-recons`): each line a template (timestamps
  found in it, by aqr-num's `time` patterns, are slots of the mark 0x04 with their pattern; the
  rest cut into tokens at spaces, or by what the first MiB codes smaller at `;`, TAB and `,` too,
  `,` not where the numbers have a decimal comma; a token that is a whole number of aqr-num a
  number slot, a token of a form rare in the segment a word, any other kept with its digit runs as
  slots; an IPv4 address in a token is the slot 0x05, a 32-bit integer) and the fields in columns by
  (template, slot), numbers as they are or as changes from the column's previous value or the
  line before's same slot, taken at the scale of the number (a time with a fraction is two
  columns), divided by the column's unit (their gcd) where that pays, an address also as an index
  into the 64 most recent addresses of all the address columns; a CR before the LF is a byte of
  the template, not of the last token. It is a whole module, 64 MiB segments, its plan
  exact from the params' counts (templates, columns, numbers of a template). aqr-compress's
  `table` (lib.rs) chooses it for text by coding a sample (the text up to 2 MiB, else 8 windows of
  256 KiB spread from start to end): where aqr-log's streams (zstd -3) are no larger than the
  text's (and aqr-xsv's, where that takes it too: the smaller of the two); prose and code come out
  at 120% and are left to aqr-wrt. Left (its README and `docs/log-design.md` section 4): IPv6,
  units and addresses in aqr-xsv, Drain-style clusters.
  aqr-xsv codes date and time columns (module 2, patterns as aqr-log's), and both take zones
  after a time as literal bytes of the pattern and digit groups (`1.187,25`, aqr-num mode bit 7).
- aqr-minecraft (Java Edition's region files, `.mca`; `mca-recons`): each chunk's zlib stream made
  again of its NBT by aqr-flate's zlib emulation (Minecraft writes level 6: every chunk of the
  18 GB sample world), the header and the stale bytes Minecraft leaves in freed sectors kept. A
  part module, a chunk at a time. Version 2 codes the NBT's holes apart (`src/nbt.rs`,
  `src/model.rs`): the sections' block states (each index as one of its neighbours, a row as the
  one below) and light (top down, of the light above, west and north and the block's hash) by a
  binary model kept over a file's chunks; the rest is the chunk's skeleton (42% smaller than v1
  on the Overworld's regions, compressing 32% and testing 64% slower). Samples:
  `F:\Data\aqr-corpus\minecraft-world-sample` (its Distant Horizons databases: aqr-zstd's
  `zstd-frames`, and `dh-lods`: the parents (coarser LODs) made of the children that come before
  them in the file, `src/dh.rs`, `src/lods.rs`). Left (its README's Status): the LODs of children
  after their parents, a model of DH's columns, LZ4 chunks, speed.
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
