# Crates: what each holds

The details of the workspace's crates that CLAUDE.md leaves out: read the section of the crate
you are changing. Crates with a README.md (most codecs, aqr-unpack, aqr-pack, aqr-win, …) say
more there.

## The CLI and its frontends

- `aqr` — the CLI (`aqr -c/-d/-K/-t/-I`). `--ipc` drives it from a frontend (`src/ipc.rs`);
  `--session` serves a compression dialog's questions before it begins (`src/session.rs`);
  `--serve` serves a shell extension the entries and files of archives, leaves evaluated kept
  (`src/serve.rs`, `docs/win-shell-ext-design.md`). `--format sfx` (`src/sfx.rs`, Windows x86-64)
  makes a self-extracting program: see "Self-extracting programs" below.
- `aqr-ipc` — the frontends' side of `--ipc`: `aqr` run on the paths, what it tells taken.
- `aqr-win` — Explorer's verbs and a progress window over `aqr --ipc` (Windows).
- `aqr-win-shell` — `aqr-shell.dll` (cargo's `aqr_shell.dll`, renamed as it is copied), `.aqr`
  files browsed in Explorer as folders (a shell namespace extension, thin: it asks `aqr-win
  --serve`, a COM local server, which runs `aqr --serve`; `docs/win-shell-ext-design.md`);
  `cargo aqr-win` keeps it beside aqr-win, `--register` registers it, the installer carries it.
- `aqr-win-install` — the installer carrying `target/dist/{aqr,aqr-win}.exe` and `aqr-shell.dll`,
  packed by its `build.rs` with aqr-exe's filters and zstd (Windows).
- `aqr-l10n` — the frontends' messages (`messages/{en,ko}.toml`, English the source; a table of
  each section, so that the SFX stub carries only its `sfx`), and what the systems read of them:
  Windows' string table, the bundle's `.lproj/*.strings`.
- `aqr-mac` — Finder's services, `.aqr` files opened and their progress in Finder over `aqr --ipc`
  (macOS); `bundle.sh` makes `target/dist/Aqueous.app` and its zip.
- `aqr-bincache` — `cargo aqr`, `cargo aqr-win`: the release `aqr` (and `aqr-win`, with the kept
  `aqr` beside it) built and kept by a hash of its sources.
- `aqr-version` — the binaries' version for their build scripts: the tag's (`AQR_VERSION`, set by
  the release workflow) or `git describe`'s, as `env!("AQR_VERSION")` and Windows' `VERSIONINFO`.

## The archive

- `aqr-formula` — entry formulas as recompressors declare them (`Formula`, `Node`, `Apply`,
  `Module` and its plan), with what they need to make them: the spill `store` of large buffers,
  `patch`es, the `probe` of whether recompressing pays (the archive encoder behind its
  `Archiver`). The codec crates depend on it alone; nothing of the archive is in it.
- `aqr-archive` — the archive structure: the archive encoder (`src/encoder.rs`, `emit.rs`,
  `compress.rs`) and decoder (`open.rs`, `parse.rs`, `program.rs`), the evaluation of formulas
  (`Evaluate`, `Modules`: by the decoder's evaluator of formula blocks, `src/eval.rs`), metadata
  trees, segments, envelopes, mirroring, released modules. Of the codecs' libraries only aqr-mkv
  (`description_bound`) and aqr-tensors (`BLOCK`) take it; their tests and tools
  (`examples/aqr-NAME.rs`) take it as a dev-dependency.
- `aqr-compress` — picks a recompressor by magic bytes, makes the formula, stores the archive; the
  hints of attributes by path (`attributes`, `hints`: `--attributes`, `.aqr/attributes`,
  `.gitattributes`; `docs/hints-design.md`) steer it, a hinted file's kind `Kind::Hint`.
- `aqr-decompress` — runs the formulas' modules natively and extracts archives.
- `aqr-native` — how each module runs natively (`NATIVE`, by module name: its native decoding of
  whole inputs, threaded where it is, or its part entry point, and when to stream), from the codec
  crates' decoding code alone (no `encode`); aqr-decompress pairs its `Module`s with it, aqr-dll
  builds each module's library with its entry.
- `aqr-unpack` — archives of other formats (ZIP, 7z, tar and its compressors) read into a tree for
  `aqr -d`, which extracts them as Aqueous archives (`crates/aqr/src/extract.rs`), and for `aqr
  --serve` (Explorer's folders of them: `sizes`, a file read by `unpack_selected`).
- `aqr-pack` — the tree `aqr -c` gathers written as an archive of another format (ZIP, 7z, ALZ,
  EGG, tar and its compressors, single-file streams: `--format FORMAT`, or `-o NAME.EXT`), each
  format's work spread over the pool; liblzma (XZ Utils, built from source) for xz/7z.
- `aqr-foreign` — what aqr-unpack and aqr-pack share, both ways: the encryptions (ZIP's, WinZip's
  AES, 7-Zip's AES) and times (FILETIME, MS-DOS's).

## Shared

- `aqr-common` — varints, rANS, bit I/O, heap accounting, the task pool.
- `aqr-testkit` — what the tests share (a dev-dependency): inputs damaged deterministically, a
  bound on a case's time, an allocator refusing large requests (`Refusing`).
- `aqr-num` — numbers printed as text.
- `aqr-walk` — one syntax walked for coding, decoding and a dry parse (`Io`), the range coder and
  the hashed model of aqr-div3 and aqr-ac3 (their coding is specified in its README).

## Modules: Wasm, native libraries, blobs

- `aqr-api`, `aqr-ld` — the Rust interface of modules and the Wasm linker wrapper. aqr-ld also
  (`src/pemerge.rs`) has `blob`, which digests a module library into a blob (just the bytes and
  facts a merge needs: image from the first section, no PE headers/`.reloc`/header gap, dead
  directories zeroed); `blobs`, which digests a set (modules and the shared libraries they link
  against) with the imports between them bound by name to export RVAs; and `merge`, which merges
  blobs (or libraries) into a stub executable as one section with no loader — code copied and
  rebased, bound imports written as addresses (relocated), the others merged into the stub's
  table, a dispatch table added (`demo/{stub,modadd,modxor}`, `pemerge-test.sh`); a library too
  (`merge_bytes`). `src/msvc.rs`: the MSVC linker of aqr-dll's no-TLS target, cutting a shared
  library's exports to a list (`AQR_LD_EXPORTS`, made by `exports`) so that `/OPT:REF` drops the
  rest, its import library still of every export; libraries linked with no entry point
  (`/NOENTRY`: the C runtime's DLL startup and its imports left out).
- `aqr-modules` — Wasm modules built from the recompressors' code (`src/bin`, `wasm/`,
  `src/known.rs`); see `docs/modules.md`.
- `aqr-wasmrt` — runs the Wasm modules of aqr natively by WAH or Wasmtime (`wasm::load`), in the
  process or in a sandbox worker (`sandbox::Sandbox`, told the `-Z wasm:` options set), with no
  native codecs linked. Shared by aqr-decompress (for modules it does not know natively) and the
  self-extracting interpreter blob. `Sandboxed` (native where known, else a worker) stays in
  aqr-decompress, which has the native modules. An engine is linked only with the feature `wah`
  (`wasm/wah_engine.rs`, WAH from crates.io) or `wasmtime` (`wasm/engine.rs`), or both
  (`wasm/both.rs`: `-Z wasm:engine`, Wasmtime by default as the faster linked; the feature
  `engine` is either); WAH always but on Windows x86-64 — aqr-decompress enabling it there, and in
  the tests' dev-dependencies (`--features wasmtime` for both). Else `wasm/interp.rs` runs the
  modules by the interpreter blob the program gives (`wasm::set_interpreter`, aqr: its pack's),
  placed at run time (`aqr_blob::load`), a worker served by the blob's own (`worker_main`).
  aqr.exe on Windows links no engine (41.9 -> 30.4 MB). What reads a module's bytes alone, and the
  bound, is `wasm.rs`'s, for all.
- `aqr-blob` — blobs: their format (aqr-ld makes them), `load` placing a set in memory at run time
  (Windows x86-64: relocated, imports between them and from system libraries bound, exception
  tables registered), and (feature `runner`) the interpreter blob's client: its `Vtable`,
  `Interpreter` (a `Runner`), failures crossing the C ABI with their kinds. The blobs import
  vcruntime140.dll and the UCRT's DLLs (a static copy would go uninitialised: no DllMain runs),
  and `load` and `aqr-ld merge` bind those imports to the program's own (`CRT_TABLE`,
  `CRT_NAMES`: `aqr-ld blob(s)` fail on a function not there), with a `_CxxThrowException` of its
  own: a panic's type information is relative to its blob's RVA 0, which it takes from the images
  registered (`register_image`: `load`, and the stubs for the blobs merged into them). A merge
  moves the blobs' function entries into the stub's exception directory, their unwind data
  rebased (aqr-ld's `src/unwind.rs`): the unwinder reads only an image's own for its addresses.
  `pemerge-test.sh` checks panics both ways.
- `aqr-interp` — the interpreter blob: a module library (`aqr_interp_vtable`, aqr-blob's `Vtable`:
  load, apply, apply_part, and for aqr the loads of aqr-wasmrt, the bound and the sandbox's
  worker) that runs any Wasm module handed to it by WAH (`aqr-wasmrt`; Wasmtime with
  `--no-default-features --features wasmtime`), no codecs linked (2.5 MB; 12.3 MB of Wasmtime).
  Built for the no-TLS target (`NOTLS`) it is mergeable into a stub: the fallback for modules the
  stub has no native blob for.
- `aqr-dll` — a module's binary built as a native library (`AQR_DLL_MODULE`; `build.py` builds
  them into `target/aqr-dll/`) exporting `aqr_api::dll`, toward self-extracting archives. The
  crates that several modules use are Rust dylibs it links against (`layout.py`, run by build.py,
  writes `shared/NAME/`, `shared.rs` and its Cargo.toml's block: one dylib for each set of modules
  using the same crates, the runtime `aqr-dll-rt`, std and aqr-api, used by all); building every
  module, build.py links them again until each exports only what the others import (aqr-ld's
  `exports`): all modules and libraries 8 MB as blobs, not 25 MB; `aqr-dll-shim` records
  applications from archives and runs the libraries on them (its own, or the ones merged into
  it). `NOTLS=1 build.py` builds for `x86_64-aqr-windows-msvc.json` (native thread-locals off, so
  std uses TlsAlloc, not a TLS directory; vendor `win7`, so that std uses no API of Windows 8 on:
  one set of blobs and one stub for every Windows), which `aqr-ld merge` can take;
  `merge-test.sh ARCHIVE...` is the end-to-end test (needs nightly and build-std). The nightly of
  these builds (and of the release's Windows binaries) is pinned by the root file
  `nightly-toolchain` (`AQR_NIGHTLY` overrides it), as the custom target's JSON is unstable.

## Self-extracting programs

`aqr --format sfx` writes the archive with shorthand module blocks for the modules that have
native blobs, after aqr-sfx-shim (packed by UPX) and the bundle of those blobs (and of the
interpreter blob of `-Z wasm:engine`, WAH by default, if any module is in full: aqr-sfx's
`bundle`), and what the program does once run where the input's root has `.aqr/sfx/sfx.toml`
(`docs/sfx-config-design.md`: read back of the archive made, compiled by aqr-sfx's `config` into a
record of CBOR before the archive, and the stub's icon, `VERSIONINFO`, manifest and subsystem
written by its `pe`, as a section after UPX's).

What it bundles is `target/aqr-sfx/` (or `AQR_SFX_PACK`), which `crates/aqr-sfx/pack.py` gathers
and packs (`sfx.pack`, the blobs as bundles of LZMA2, ~2.9 MB: only aqr reads it; each interpreter
blob a bundle of its own, WAH's always and Wasmtime's with `pack.py --engines=wah,wasmtime`, +2.2
MB, which aqr picks by `-Z wasm:engine`). It is no part of aqr's build but written into the linked
executable as resources (`aqr-sfx-pack --embed DIR EXE`, aqr-sfx's `resource`, read in place as it
runs): `cargo aqr` writes it into the binary it keeps (`pack.id` in its manifest), aqr's
tests/sfx.rs into a copy of theirs. Each blob is used only for the hash it was packed for, so
`cargo aqr` runs pack.py again where the pack's modules are not those of known.rs (where there is
a pack), or its `sfx.pack` is not of this form. Without the pack, aqr.exe runs no module as
WebAssembly.

- `aqr-sfx` — the self-extracting apply logic (a library): `Sfx` finds the blobs merged into the
  program through the dispatch table's anchor — native module blobs named `HASH/SLOTS`
  (`native_name`: the module's hash and task slots), the interpreter blob `aqr-interp` — and is
  the `Runner`: a shorthand block's hash gives a stand-in Wasm module (no code, the task slots and
  the hash in custom sections), which `load` takes to the native blob (a part module streamed
  through `aqr_dll_apply_part`, as `Info::apply_part` gives it); any other module goes to the
  interpreter. The shared libraries the native blobs link against are entries `lib:NAME` (their
  images registered, nothing else). `Sfx::load` takes the same blobs from a bundle (`bundle`:
  each blob's code, up to the end of its exception table's last function, through aqr-exe's x86
  disfilter, the streams of all the blobs one after another by stream and the rest apart, each a
  zstd frame at 19; feature `encode` makes them; feature `xz` the same of raw LZMA2 frames, for
  the pack aqr carries alone, not the stub), placed by `aqr_blob::load`. `extract` writes a
  decoded archive's files out (`extract_with`: as the settings write over what is there, the made
  noted for `clean`). Shared by the stub and aqr. `pack.py` gathers what aqr embeds (the stub,
  packed by UPX `-9` where `AQR_UPX` or `upx` is found, the blobs of the modules and of the shared
  libraries) and packs it (`aqr-sfx-pack`: `sfx.pack`).
- `aqr-sfx-shim` — the self-extracting stub (lean: aqr-archive + aqr-sfx, no codecs or engine,
  0.78 MB, 0.35 MB packed by UPX: the profile `sfx`, panic=abort): the small executable that `aqr
  --format sfx` appends a bundle of blobs and an archive to (or that `aqr-ld merge` fills with
  blobs). It takes the bundle, the settings and the archive from right after its image (else it
  scans itself from the end for the archive's header magic, the settings right before it),
  decodes the archive running the modules through the blobs, writes the files out, makes the
  shortcuts (`src/shortcut.rs`) and runs the program the settings tell (`src/run.rs`; its
  questions, messages and license dialog in boxes where it has no console, `src/ui.rs`, under the
  settings' banner, `src/banner.rs`; the folder picked, `src/folder.rs`).

## Windows builds and the release

The release workflow builds the stub and the modules' blobs (`pack.py --part=modules`), the
interpreter blob (`--part=interp`) and the binaries in three jobs at once, then packs
(`--part=pack`), writes the pack into aqr.exe, smoke-tests `-Z wasm` and an SFX, and builds the
installer.

Every Windows binary of a release (aqr, aqr-win, the installer, the stub, the blobs) is of the
target of Windows 7 on (`cargo +NIGHTLY win7`, `.cargo/config.toml`; pack.py's stub for
`x86_64-win7-windows-msvc`): what newer systems add is looked up as it runs (the sandbox's
mitigations, aqr-wasmrt's placeholders), and std's SRW locks were no slower (10gb at -5).
Development builds (`cargo aqr`, the tests) stay of `x86_64-pc-windows-msvc`.

Windows binaries link the Visual C++ runtime statically (`.cargo/config.toml`: the release's, of
the win7 target, and the SFX stub the UCRT too; development builds the UCRT as Windows 10's DLL),
so no redistributable is needed; the blobs bind theirs to the program's (aqr-blob above).

## Codecs

`codecs/` holds the recompressor crates `aqr-*`, each with a `README.md` (status, what is left)
and a `FORMAT.md` (what it stores); a crate with a `FORMAT.md` belongs there.

- Disk images are aqr-disk's (partition tables, containers, the formula;
  `docs/disk-image-design.md`), their file systems read by aqr-iso, aqr-fat, aqr-ntfs, aqr-ext,
  aqr-apfs and aqr-hfs (no FORMAT.md: aqr-disk's tells).
- Game engines' and middleware's packs are aqr-bundle's `packs` (aqr-disk's formula, no module),
  formats named `<engine>-<extension>` or the middleware's or tool's (`godot-pck`,
  `unity-assets`, `unreal-upk`, `wwise-bnk`, `cri-awb`, `bethesda-ba2`, …: `packs/mod.rs`'s
  `packs!`); aqr-compress's `siblings.rs` plans the files of no header taken apart by others
  archived along (a VPK's archives, Unity's `.resS`).
- `aqr-flate` underlies everything that uses DEFLATE. flate-zoo (DEFLATE encoders for its training
  and tests) comes from crates.io: the root `Cargo.toml`'s `[workspace.dependencies]`, with the
  backends that do not work with MSVC added for other targets.

### Notes by codec

What the READMEs may not say up front: how each was made and what is left in brief.

- aqr-lzma: the goal is deduplicating contents, not the best recompression; a record of about 14%
  at a reasonable speed is fine. What is left is in `docs/10gb-todo.md` item 6.
- aqr-lzo: a record of the parse rather than emulation, as LZO has many encoders.
- aqr-bvx (Apple's LZFSE and LZVN, `bvx2`/`bvxn` blocks; asset catalogs): a record of the parse, as
  aqr-lzo, with the LZFSE compressor's table of candidates (14-bit hash, 4 ways) emulated as a
  context. Apple's lzfse code (BSD) is not read; libcompression is the black box.
- aqr-gif: the LZW codes coded given the pixels against the longest match (every encoder seen
  writes it), the pixels by a context-mixing model of palette indices (lpaq's style; integers
  only, so Wasm and native agree).
- aqr-bz2 reimplements libbzip2's compressor (1.0.3 on), written from memory of its behaviour;
  libbzip2's code is not read. Python's `bz2` and a shared libbz2 (`BZ2_hbMakeCodeLengths`) are
  the black boxes. Equal rotations of periodic blocks order otherwise, so their `origPtr` goes in
  the params.
- aqr-azo (AZO, ALZip's compressor in EGG archives): a record of the parse, as aqr-lzo, the stream
  coded again by the spec's models (OctopusET's EGG spec, Appendix A; ALZip's output the black
  box); the x86 filter was found by CRC alone (`docs/azo-x86-filter-search.md`).
- aqr-lha (LHA's `-lh4-` to `-lh7-`): a record of the parse, as aqr-lzo, with Okumura's LZSS tree
  as a second match finder; every compressor seen writes the headers of ar002's Huffman coder
  (predicted from the tokens) and cuts blocks by its token buffer. No LHA code is read; lhasa's
  test archives and Aminet are the black boxes (Debian's `lha`, LHa for UNIX, would be an oracle
  in the Linux VM).
- aqr-rar (RAR5, and RAR 3.x/4.x's RAR 2.9 algorithm: LZ, PPMd, the standard filters): a record of
  the parse, as aqr-lzo. Clean room: `docs/rar5-decoding.md` and `docs/rar3-decoding.md` were
  written by agents that read libarchive (BSD) and wrote no code; the code is written from those
  notes only. The PPMd model is ported from lzma-sdk (public domain). rar 7.23 and 6.24
  (`third_party/rar-dl`, 6.24 for `-ma4`) are the black boxes.
- aqr-prores (Apple ProRes in QuickTime or MXF): the frames found by their `icpf` (no container
  parsed), parsed to coefficients and written again (every coefficient set has one coding); a
  context model with the previous frame as context, in chunks of 64 MiB. A part module
  (`prores-recons` 3): the coded chunk is pieces for each frame (side info; one for each stripe of
  rows, a stripe for each 4 MiB of frames, at most 16, and for 16 KiB of a frame), a gang of one
  task for each stripe lives through the chunk, the instance sending each its pieces and taking
  the slices to write the frame, so its plan follows the frames, not the chunk. Natively the same
  code runs through the emulation of aqr-api (`Run::Part(entry, None)`). Written from RDD 36's
  description; no decoder's code read.
- aqr-snappy and aqr-lz4 reimplement the encoders: Chrome's LevelDB Snappy is the
  multiplicative-hash build (Homebrew's arm64 builds use the CRC hash and do not match);
  Firefox's mozlz4 is liblz4's `LZ4_compress_default`.
- aqr-zstd: a `.zst` file's frames are not rebuilt (it would gain little); it is kept as it is and
  the files of its batch that its content holds (the whole, a tar archive's members) are taken
  from its frames, decompressed by Patch instructions of an empty source and split
  (`Formula::split_patch`, `crates/aqr-compress/src/zst.rs`). `compress` emulates libzstd's
  compressor at level 3 (the double-fast strategy, contents under 256 KiB), written from its
  behaviour with libzstd as the black box, not its code: the module `zstd-frames` (a part module)
  makes frames in other data again of their contents, their pieces anywhere in the file, for
  SQLite databases of zstd blobs (aqr-compress's `sqlite-zstd`, aqr-sqlite's `blobs`; Distant
  Horizons' LODs).
- aqr-git is the pack recompressor `git-pack-recons`, at aqr-flate's profile as aqr-compress picks
  it; a pack with its `.idx`/`.rev` is a set (`git-pack-index-recons`, only the ids stored). Left:
  deriving the ids (resolving every delta), `multi-pack-index` from the indexes, the archive's
  16 MiB solid-block bound, pack blobs in `together`.
- aqr-tensors (files of tensors: GGUF, `torch.save`'s ZIP format, safetensors; the formats `gguf`,
  `pth`, `safetensors` of aqr-compress, one recompressor): the tensor data in segments of 16 MiB,
  each an application of the part module `tensors-recons`: floats' exponents and quantised
  blocks' nibbles (and Q6_K's crumbs, the high bits of Q8_0 and Q5_*) as symbols of a static
  order-0 rANS of 16 symbols, 32 states (SIMD: AVX2 natively, SIMD128 in Wasm, no gather), the
  rest as it is. Weights converted from a lower precision (NVFP4, MXFP4, FP8, NF4, GPTQ, INT8 to
  BF16/F16/F32) are found by a sample of the tensor and coded as a grid's indices times scales of
  tiles fitted exactly (`src/grid.rs`). A `.pth` is read by its central directory and the
  persistent ids of its pickle (a small machine of the opcodes, nothing else of the pickle). Speed
  is the point (the files are tens of GB): aim at 500 MB/s through `aqr` (492 MB/s on tenz's 11
  models, 2026-11-14). Samples are in `F:\Software\bai-models`, `F:\Software\lmstudio-models` and
  `F:\Data\sam_vit_h_4b8939.pth` (F32), used in place (never copied); safetensors from Hugging
  Face (and checkpoints dequantised from them) and the models of tenz's README
  (github.com/nsiddiqui248/tenz, a GGUF compressor of context models: about 4 points smaller than
  aqr-tensors on quantised GGUF) in `F:\Data\aqr-corpus\tensors`.
- aqr-log (lines of text: logs, tables of numbers; `log-recons`): each line a template and its
  fields in columns (`codecs/aqr-log/FORMAT.md`, `docs/log-design.md`). aqr-compress's `table`
  chooses it for text by coding a sample (the text up to 2 MiB, else 8 windows of 256 KiB):
  where aqr-log's streams (zstd -3) are no larger than the text's (and aqr-xsv's, where that takes
  it too: the smaller of the two); prose and code are left to aqr-wrt. aqr-xsv codes date and time
  columns (module 2, patterns as aqr-log's).
- aqr-minecraft (Java Edition's region files, `.mca`; `mca-recons`): each chunk's zlib stream made
  again of its NBT by aqr-flate's zlib emulation (Minecraft writes level 6), the NBT's holes (block
  states, light) coded apart by a binary model kept over a file's chunks. A part module, a chunk at
  a time. Samples: `F:\Data\aqr-corpus\minecraft-world-sample` (its Distant Horizons databases:
  aqr-zstd's `zstd-frames`, and `dh-lods`: coarser LODs made of their children, `src/dh.rs`,
  `src/lods.rs`).
- aqr-jpeg's lossless JPEG (SOF3: the pixels of CR2, DNG and the like) is the module
  `jpeg-lossless-recons`, apart from `jpeg-recons`: the streams found by their SOI in a TIFF file
  (`lformula::starts`), the samples coded by `lmodel` (a blend of predictors, a Golomb code by
  adaptive bits; integers only). A big stream is cut in bands of rows, each its own chunk (about
  1M samples); the small ones (a DNG's tiles) share one model. A part module (`lpart`), its plan
  (`lpart::plan`, `lossless_jpeg_plan_holds*`) following the width of the frames and the size of a
  chunk, not the file. A header holds its lists in place (`Small`): the module's allocator takes
  only 64 blocks freed out of order. Samples (CC0): raw.pixls.us, in
  `F:\Data\aqr-corpus\aqr-jpeg-lossless`.

## Archive features in brief

- Archives of several files are committed as they are written and appended to by `-a` (spec,
  "Appending to an archive"; `docs/append-design.md`): a commit writes the segments and a new
  footer after the old one, then makes the old one a spacer block by its first 2 bytes; a stop
  (first Ctrl-C) ends the archive as it is, telling what it left out (`segment-status`). Not with
  `-P` (nonces) nor `-A` (Mirroring owns the footer), which end as they are but do not commit.
- aqr-archive's decoder (`defined()` in `src/parse.rs`) does not accept import blocks 58–5F,
  though the spec has them. Implement them together with import resolution and overlays when
  extraction work starts, not by just adding them to `defined()`.
- Near-duplicate files: reference coding in aqr-png, multi-output formulas (`Node::Output`,
  `Node::Patch`), png-set and generic per-class patches (`Formula::merge_classes`;
  `docs/xform-design.md` §4.6). Left: a byte bound on the dependency closure of top-level entries
  for random access.
