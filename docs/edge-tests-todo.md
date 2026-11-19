# Edge-case tests: what is left

An audit (2026-11-19) of the tests, by reading them and the code, found the edge cases below
untested. Nothing in it was run; each item is a claim to check first: write the test, see it
fail (or pass, and then it only pins the behaviour), fix, see it pass. Strike an item out (or
delete it) once done; say in the commit which items it closes.

Item ids are `crate/n`. "Bug?" marks what the audit took for a defect, not only a missing test;
"bug" what was confirmed by reading the code.

Decided: no fuel or epoch interruption in aqr-wasmrt (neither deterministic nor exact); a
module that never ends hangs the decoder, by design for now.

## 0. Infrastructure

Done: `profile.release-test` has `overflow-checks = true` (it found the wraps listed in the
commit that turned it on); tests no longer pass silently without the sqlite3 tool, the SFX pack
or a corpus (`#[ignore]` where they need one); `crates/aqr-testkit` has `damaged` (inputs
damaged deterministically), `within` (a bound on time) and `Refusing` (an allocator refusing
large requests).

- infra/3: a helper damaging the inputs of an application and asserting that native and Wasm
  fail alike (as `pipelines_fail_alike`, `gif_same_as_wasm` do), in aqr-decompress's tests.

## 1. Confirmed defects

Done: aqr-msadpcm (the wrap made explicit), aqr-div3 (the plan follows the picture, the walk's
scratch one block), aqr-7z (decoding within the budget), aqr-sfx-shim (the archive located),
aqr-decompress (native verdict on task-limit counts), aqr-zip (the output reserved fallibly),
aqr-disk (`aqr_common::mem::try_zeroed`), aqr-archive (lengths past 2^64 invalid), aqr-ole (a
FAT of the file's sectors). Not a defect: aqr-hdf5's self-pointing B-tree (the depth turns
nodes down before their entries are read; `tree_of_itself` pins it).

- bug aqr-lzo/1: `lzop.rs:88` counts one 4-byte field where both `ADLER32_D` and `CRC32_D`
  are set; libarchive's BSD reader counts one for each (lzop itself sets one): a fixture of a
  fixture's header and blocks patched to both (its header checksum made again).

## 2. By crate

### aqr-compress (detection, dispatch, fallbacks)
- 1: `siblings.rs` has no test: VPK dir without its `_000`, renamed or too short, two dirs
  claiming one archive, `.resS` without its serialized file or shorter than its ranges.
- 2: panic and heap fallbacks are tested for gzip only: `caught` in `scanning.rs:346`,
  `sets.rs:197,233`, nested members (`made.rs:443`); panics in `zst::share`, `git::sets`,
  `inflated::Bodies::of` (caught at all?).
- 3: `verify.rs:428 evaluate` has no catch: a module panicking in prevalidation run inline (no
  worker) — does the file fall back or the archive abort?
- 4: `fits`/`fits_within` (`recompressors.rs:380`) at the boundary, and `-Z heap:FORMAT`.
- 5: detection order claimed in `ROWS` but unasserted (GitPack vs QuakePak, Jsonl/Xsv/Text,
  SqliteZstd vs Sqlite, cpio magics); `starts_as_detect` for Rar, 7z, Cab, ALZ, EGG, xz.
- 6: magic cut right after it (gzip, xz), `PK\3\4` of an empty payload, zip in zip with the
  inner one cut.
- 7: `zst.rs share`: content over `bound`, `CONTENT_MAX`, equal candidates, frames that do not
  decode. `git.rs sets`: two idx of one pack, idx/rev shorter than `2*ID`.
- 8: `-Z lzma` group boundaries; `table` choice at the equality boundary; `hdf5.rs` fallback
  to a non-adaptive profile.

### aqr-unpack
- 1: no ZIP64 fixture (`zip.rs:214-260`).
- 2: lying declared sizes (`plain()`, `codec::room(size)`) for each method; a highly
  compressed honest entry.
- 3: local and central headers disagreeing; data descriptor with zero local sizes; central
  offset past the end.
- 4: duplicate names end to end (ZIP, tar, 7z); NUL in names; `find_end` with the signature in
  a comment, a prefix, a cut central directory.
- 5: tar sparse (old GNU, pax), multi-volume, hard link to a missing or later target, size past
  the data; 7z empty-stream/anti items, header CRC, `power > 24` and `SEVENZ_RAW`.

### aqr-pack, aqr-foreign
- 1: nothing past 4 GiB or 65535 entries: ZIP64 writing (`zip.rs:152,287-323`), 7z, tar pax
  `size`; 0xFFFF entries round-tripped through aqr-unpack.
- 2: `time.rs`: `to_dos` clamp above 2107 and below 1980, `from_dos` invalid fields, FILETIME
  extremes, round trips of such times through the writers.
- 3: ZIP non-UTF-8 names, names over 65535 bytes (`alzip.rs:148,247`), empty, long and
  non-ASCII passphrases, 7z of duplicate or case-colliding names, tar compressors round-tripped,
  charset conflicts between entries.

### aqr-flate
- 1: a table of malformed DEFLATE through every `inflate.rs` entry point (BTYPE 3, NLEN, a
  distance before the start, symbols 286/287/30/31, HLIT 288, repeat 16 first, runs past
  HLIT+HDIST, oversubscribed codes, no end of block): Err everywhere, alike (guards the
  `unsafe copy_match`).
- 2: nonzero tail padding, stored-block padding and trailing bytes through
  `deconstruct`/`reconstruct`.
- 3: `Derived::Zlib` missing from `part_as_whole`, `flate_set_same_as_wasm`,
  `flate_plans_hold` (Gzip too in the last).
- 4: `too_many_blocks_are_not_deconstructed` asserts nothing on Err; damaged flate-set inputs
  native vs Wasm; zlib emulation with `inbuf`/`outbuf`, level 0, window-slide sizes;
  `max_data == 0`.

### aqr-zip
- 1: `zip::extent`/`cut_at` directly: comment past the data, zip64 end record and locator,
  archive after archive.
- 2: `Made::Same` of a body kept; corrupt layout native vs Wasm (`zip_same_as_wasm`); local
  header of 0xFFFFFFFF sizes and no zip64 field; `SAME_MIN` boundary, Deflate64 kept.

### aqr-git
- 1: `index::offsets`/`starts` of 64-bit offsets on the encode side, out-of-range slots.
- 2: `pack::entry_at` rejections one by one (types 0 and 5, FDICT, non-0x78, version 3, count
  short, no entries); Wasm damage of layout/kept/ids; `pack::entries` ignoring the limit.

### aqr-mszip
- 1: CAB structure: PREV/NEXT, Authenticode reserve and signature (`extent`), trailing bytes,
  empty block lists, zero-size blocks, NONE folders of other lengths, overlapping folders.
- 2: MSZIP `join` rejections (signature, bytes after the final block, nonzero padding, no
  final flag); CHM of other windows and resets, PMGI, ITSF versions; `decode_part` near
  `SPAN`/`JOINED`; `corrupt.rs` with `len` 0 and an index of more blocks.

### aqr-png
- 1: plans in Wasm for depths 1/2/4, colour 3 and 4 (`png_plans_hold`).
- 2: 1x1, 1xN, Nx1, interlaced tiny images (empty Adam7 passes).
- 3: FDICT, other zlib headers, zero-length IDAT/fdAT, fdAT under 4 bytes, fcTL not of 26
  bytes, IDAT runs split by another chunk; `enc.len > u32::MAX`.

### aqr-gif
- 1: width or height 0, 65535² with a tiny stream; damaged skeleton (natively and in Wasm).
- 2: GIF87a, no global table, local tables, bytes after the trailer, chains cut; interlaced of
  other pixel counts; min code sizes 0, 1, 9+.

### aqr-jpeg
- 1: mutenc (`variant::reencode`) and fuzz in `tests/` with fixed seeds (today manual only).
- 2: fixtures of grayscale, 4:2:2, 4:4:0, 4:1:1, non-interleaved scans, 4 components, 16-bit
  DQT, successive approximation, DHT/DRI redefinitions; bytes after EOI, no EOI, `FF` fill.
- 3: damaged jpeg-recons native vs Wasm; progressive alloc test returning silently on None
  (assert instead); 65535x1, 1x65535, Σ bw·bh at 2^24.
- 4: lossless: `check()` fallbacks (padding bits, `last_code` duplicates, bad RST numbers, a
  corrupt stream among good ones); bounded heap against `MAX_SAMPLES`; predictors 0 and 8+,
  precisions 2 and 16, width/height 1, a cut header; Wasm damage.

### aqr-psd
- 1: mutation of the source PSD (truncation at every offset, flips of header, resources,
  layers); huge dimensions and channel counts with a bounded heap.
- 2: CMYK, gray, indexed, 16 and 32 bits, `Lr16`/`Lr32`; ZIP compression 2 and 3; negative and
  zero layer counts; PSB with a corrupt 64-bit length.

### aqr-clip
- 1: noise tiles, arbitrary 4th bytes (`Class::Full`, escapes); zlib other than level 1,
  trailing bytes, wrong Adler, other inflated lengths; a real SQLite database embedded.
- 2: table and header edges (`clip.rs:34-37,~105` overflow in 32 bits); corrupt streams with a
  bounded heap and time (the test only catches panics).

### aqr-lzma
- 1: data past `LIMIT`/`limit` gives None and is stored.
- 2: multi-stream `.xz` and stream padding; container mutations (index CRC, flags, backward
  size, cut blocks, block padding); `.lzma` known size with end marker.

### aqr-bz2
- 1: streams of another compressor (perturbed code lengths) give None; randomised blocks; the
  1 GiB bound; a block of exactly 900000 bytes.

### aqr-lz4, aqr-snappy
- lz4/1: `mozlz4` `with_capacity(n)` from the header, `frame::decode` without an output limit:
  high-ratio inputs with a bounded heap; legacy and skippable frames; content size lying.
- snappy/1: `snappy::decode` reserving a claimed length per block; tables mixing rebuilt, raw
  and kept blocks; footer padding.

### aqr-zstd
- 1: mutation of `parse`/`header`/`frame`: reserved block type, blocks over `block_max`,
  dictionary ids, reserved bits, `CONTENT_MAX`, skippable frames of `u32::MAX`, empty input.
- 2: `Pool` (`frames.rs:121-142`) layout varints near `u64::MAX`.
- 3: `compress` of RLE blocks, raw blocks across 128 KiB; checksummed frames rejected; levels
  2 and 4+.

### aqr-lzo, aqr-bvx, aqr-xpress, aqr-lznt1, aqr-lzx, aqr-ppmd
- lzo/1: lzop headers with extra field, filter, old versions; bad block sizes; concatenated
  files.
- bvx/1: mutation of the streams through `extent`/`decode` (LZFSE tables, LZVN opcodes); empty
  data, over `MAX_DATA`, bytes after the end block.
- xpress/1, lznt1/1: `module::decode` and `cut_lens` with inconsistent indexes at the crate
  level; chunks of exactly 65536/65537 bytes; `read(c, n)` of a huge `n`; LZNT1 slack bits and
  displacement widths.
- lzx/1: a fixture of a real compressor in the crate; window bits 18-20; E8 edges; damaged
  streams through `read`.
- ppmd/1: header rejections (restore 3, order 1, name past the buffer, short input, variants).

### aqr-7z, aqr-alz, aqr-egg, aqr-azo, aqr-lha, aqr-rar
- 7z/2: duplicate or out-of-order property ids (`header.rs:318`, then `s.subs[i]`: panic?);
  encrypted folders and headers; `with_capacity(size)` from the header (`formula.rs:78`);
  `Made::Apart` (BCJ2 not round-tripping); empty archive, overlapping pack streams.
- alz/1: truncation over every fixture near the headers; header sizes lying (deflate, bzip2);
  comment section.
- egg/1: solid archives of more than 2 files, mixed methods, size mismatches for deflate, LZMA,
  AZO; LZMA without end marker; split volumes.
- azo/1: damaged data and index; `decode_block` `with_capacity(size)` (`azo.rs:529,706`);
  x86 stored block not inverting.
- lha/1: a bad header checksum between good entries; the 0x42 extended header (64-bit sizes);
  directories, symlinks.
- rar/1: `read()` grouping (split, encrypted in a solid group, stored between solid members,
  unknown size, a dictionary changing) by synthetic headers; `LHD_LARGE`, encrypted headers,
  service headers; damaged data and index (not only the record); RAR4 Unicode names; RAR 1.5/2.0.

### aqr-wim, aqr-asar, aqr-isc, aqr-inno, aqr-ole
- wim/1: no `limit`: 1-byte chunks of a huge `orig`; the 8-byte table (`orig > u32::MAX`);
  spanned resources, overlapping offsets, pipable WIM.
- asar/1: `asar_same_as_wasm`; JSON deeper than `DEPTH`, offsets past 2^53, `blockSize` 0,
  duplicate keys.
- isc/1: chunks over `MAX_CHUNK`, of length 0; truncation sweep.
- inno/1: a table past 8 MiB; Inno 07 CRC mismatch; dictionaries at the extremes.
- ole/2: version 4 sectors, a real DIFAT; directory cycles, more than 256 siblings.

### aqr-mp3, aqr-flac, aqr-vorbis, aqr-ogg, aqr-wwise, aqr-fsb, aqr-ima, aqr-msadpcm
- mp3/1: the example's mutation fuzz as a test (side info and main data of every fixture);
  dual channel, plain stereo, intensity stereo, private/copyright/emphasis bits; dense false
  syncs (linear time).
- flac/1: variable block size (0xF9), verbatim subframes, right/side, block-size code 6,
  sample-rate codes 12-14, sample sizes 0/2/5, bps 12/20; duplicate STREAMINFO, bad escape
  partitions; `flac.rs:75` `d.len() - p`.
- vorbis/1: flips over the setup with `packet::parse` under every mutated setup.
- ogg/1: Vorbis over 2 MiB (pieces) and packets of 0xFFFF bytes or more; `module.rs:78`
  `with_capacity(len)`; Opus TOC codes 1-3.
- wwise/1: `stripped: true` in Wasm (plan and agreement); `mod_packets == false`; other chunks,
  odd sizes, `vorb` sizes, offsets past the data.
- fsb/1: version 0 header; channels chunk (0, over 8).
- ima/1, msadpcm/2: extremes (255 channels, saturation).

### aqr-mkv, aqr-prores, aqr-div3, aqr-ac3
- mkv/1: BlockGroup/Block with lacing; unknown sizes; track numbers of 2+ bytes; two segments,
  bytes after.
- prores/1: `find()` on forged headers, cut sizes, nested `icpf`; truncated and extended
  streams, `prores/rest` damaged natively.
- div3/2: a committed DivX 3 I+P fixture (VLC tables, I frames, MVs untested in CI); plans at
  1920x1080 and 4096x4096; `movi` in a header string, `ix00`/`AVIX` ending the walk, truncated
  AVIs; damaged module inputs.
- ac3/1: an in-test AC-3 syncframe writer (silent blocks, valid CRCs): mono, stereo, 5.1+LFE,
  frame sizes; `check` vs `encode` contexts; throughput on dense `0B 77`.

### aqr-exe, aqr-bcj, aqr-dex, aqr-dwarf, aqr-pdb, aqr-wasm, aqr-arsc
- exe/1: overlapping, past-EOF and zero-offset sections end to end (`aqr-compress/src/exe.rs
  ~752`, `elf.rs:~234`, `macho.rs:~213`), fat archs at one offset.
- exe/2: CIL end to end, native vs Wasm, plan; heap sizes, tables over 65535 rows, `#-`, fat
  exception clauses; mutation.
- exe/3: long structured x86 fuzz (1-64 KiB, dense branches, origins near 2^32 and 2^63, side
  output), whole vs part.
- exe/4: PE (sections past the file, data directories 0 or >16, PE32 of 0x8664, ARM64EC,
  packed), ELF (ET_REL, x32, SHN_XINDEX), Mach-O (32-bit, arm64e, fat64, `nsects * size` in
  32 bits); `extent::pe_sections`; random `.text` stored.
- bcj/1: `unfilter` None (and the 7z/lzma fallbacks on it); property validation; BCJ2
  mismatch; `bcj2::decode` of huge lengths.
- dex/1, pdb/1, dwarf/1, arsc/1: mutation of the input files; allocation guards for dex
  (`dex.rs:887`), pdb, wasm; dwarf forms and unit headers (v2/3, 64-bit, split units),
  abbreviation tables; pdb MSF variants and overlapping streams; arsc sparse and 16-bit
  offsets, long pool strings.
- wasm/1: `extent` taking `\n` plus text after a module as a section; sections out of order or
  twice.

### aqr-disk and file systems
- disk/2: huge metadata ranges read through `Map::get` of a sparse virtual disk (FAT, ext).
- disk/3: GPT fields near 2^55/`u64::MAX` (`partition.rs:114,119`); many partitions of one
  volume; cross-linked container clusters; WOF over 4 GiB; damaged compressed streams; UDIF
  ADC/LZMA, qcow2 chunks of the wrong length.
- iso/1: no directory work budget (N records of the image's size); multi-extent files; UDF
  sparse and chained extents.
- fat/1: exFAT loops and NoFatChain floods (no `steps`); `ValidDataLength < DataLength`.
- ntfs/1: `$FILE_NAME` parent cycles; fragmented MFT (O(records × runs)); extension records
  before their base.
- ext/1: extent and indirect loops, cross-links; `i_size_high`; huge group counts.
- apfs/1: the fuzz's "checksums made right again" is not implemented; B-tree loops.
- hfs/1: extents-overflow loops, catalog link cycles, wrapped volumes.

### aqr-bundle, aqr-obj, aqr-minecraft, aqr-pdf
- bundle/1: 24 of 31 packs outside `packs_fuzz.rs`; overlapping, duplicate, zero-length
  entries; a pack whose one file is a pack.
- obj/1: indices past 2^31/2^32, 20+ digits, `1e400`, mixed line ends, portion boundaries.
- minecraft/1: chunks whose NBT is a candidate but no hole (`model.rs:283`); malformed NBT
  (depth, counts, cut tags); a chunk inflating past `limit()`; gzip and custom chunks.
- pdf/1: a 1000:1 flate stream; filter chains; dictionaries over `MAX_DICT`; indirect
  `/Length` redefined by an update.

### aqr-log, aqr-xsv, aqr-jsonl, aqr-wrt, aqr-b2t, aqr-fasta
- log/1: `rescale` (aqr-num `fixed.rs:165`) at i64 extremes with a scale change, natively and
  in Wasm; literal 0x04/0x05; `MAX_COLUMNS`, `MAX_NUMBER` 40/41, `MAX_DIGITS` 18/19; locale
  sample.
- xsv/1: differences at i64 extremes; a BOM before a quoted field; 4-byte delimiters and
  terminators; wide tables in the plan tests.
- jsonl/1: 64-deep nesting in Wasm; more than 64 array elements; `F_DELTA` at i64 extremes.
- wrt/1: Hangul `uncuttable` (`words.rs:302-319`); mixed words and Hangul segments; CRLF at
  piece boundaries.
- b2t/1: more than 64 separators, separators of 32/33 bytes; mixed line ends in a run.
- fasta/1: mutation of `build`/`decode`; CR in headers only; `W` edges.

### aqr-infer-recs, aqr-gnu-gettext, aqr-sqlite, aqr-hdf5, aqr-fits
- infer-recs/1: NaN, ±Inf, -0.0, subnormals in float columns; big-endian; random and tiny
  inputs; mutation of the decoders.
- gettext/1: mutation of `mo::build`; duplicate originals, msgctxt keys; `hash_table` of size
  1/2.
- sqlite/1: crafted corruption in `cargo test` (freelist and overflow cycles, page counts
  lying, bad page sizes, overlapping cells); the lock-byte page; WAL headers.
- hdf5/2: lying dimensions and sizes; float32 and big-endian datasets; superblocks 1 and 2.
- fits/1: NaN, ±Inf, -0.0 in float images; BITPIX 64/-64; NAXIS products near 2^63, missing
  END; ASCII tables, THEAP, variable-length arrays, padding.

### aqr-tensors, aqr-raw, aqr-tex
- tensors/1: ZIP64 `.pth` (`pth.rs:50-90`), and its malformed variants.
- tensors/2: the grid's escaped tiles (`grid.rs:739`, `module.rs:499-507`): almost-grids, NaN,
  Inf, -0.0, all-zero tiles, scale bits 0xFFFFFFFF.
- tensors/3: special floats of every type through `Run::Float`; mutation of `Run::read`'s
  params; overlapping, tied and unsorted tensors; GGUF and safetensors header lies; pickles;
  the real 16 MiB segment boundary; SIMD paths.
- raw/1: hostile TIFF (IFD cycles, SubIFD back pointers, strip overflow, counts past EOF): it
  also finds extents in other formats.
- raw/2: WAV/AIFF chunk walking (data before fmt, two data chunks, sizes 0 and 0xFFFFFFFF, odd
  chunks); BMP OS/2 and V4/V5, negative heights; DICOM malformed; PAM.
- tex/1: container negatives (KTX, KTX2, PVR, .astc: overlaps, offsets past EOF,
  supercompression, 3D, cube PVR).
- tex/2: `MAX_RATIO` (`module.rs:13,37`) against large uniform textures: could a valid archive
  fail to decode?
- tex/3: corruption loops for BC7, BC6H, ETC, PVRTC, ASTC (today BC1-5 only); ASTC legal-block
  coverage; DDS corners; plans on compressible data.

## 3. The archive (aqr-archive, aqr-decompress, aqr)

### Reading (aqr-archive)
- read/1: the flip and truncation sweep (`decode_errors`) over a multi-file, multi-segment
  archive with links, extras, a journal and a tombstone, through `open`, `open_indexed`,
  `open_indexed_at`.
- read/2: hand-built footer and header failures (about ten messages of `open.rs:337-460`,
  `index.rs:160-210,449`), `prev` loops, a header-only archive.
- read/3: strict opener: truncation exactly at a chunk boundary, chunks swapped, duplicated,
  dropped; work factor under 12.
- read/4: `program.rs:read`: reserved opcode, truncated checksum, checksum of an empty stack,
  ModuleBytes of a shorthand block, wrong output count, bad references, `Swap` past the depth,
  `Concat(0)`/`Pack(0)`, counts past the bytes.
- read/5: intermediate and several Checksum instructions (a wrong one among right ones, on a
  Split input or a literal).
- read/6: `Patch` of a declared 2^36-2^40 length; Apply `output_sizes` uncapped.
- read/7: malformed metadata for `tree::read_with`/`tree_of` (parents after entries, offsets,
  trailing bytes, duplicate names, tombstoned extras, outputs of other sizes); the CBOR depth.
- read/8: dictionary references; import blocks 58-5F rejected (and `inspect.rs:240` counting
  0x50..=0x5F as content); module/resource offsets across segments; `outputs` 0 or
  `u64::MAX`; journal and status ranges; both a block and its mirror damaged; spacers of every
  size field; non-minimal varints outside block types; a solid stream at the 16 MiB bound.

### Writing and appending (aqr)
- write/1: `-a`, `--delete`, `-j` on an `-A` archive (`append::open` has no check): bug?
- write/2: append where the footer crosses a sector (`kill == false`: `set_len`, then the new
  footer), a crash in between; append to a stub-prefixed archive.
- write/3: the lock ("being written by another"); stops on Windows (the tests are
  `cfg(unix)`); `--atomic -a` stopped, an IPC cancel of an append.
- write/4: the op-log replay of `committed` with an existing archive (`Appending`), cut at
  every patch, and `end_stopped`.
- write/5: inputs changing while archived (cut, grown, deleted, unreadable) at the CLI, with
  `--rm`; disk full; the archive inside its input tree; `sfx_into` writing non-atomically.
- write/6: a hundred appends in a row; zero-file inputs; single-file and size-hint archives
  refused; `-a` on truncated and torn archives; edits of the only file, of a path twice.
- write/7: names (non-UTF-8, surrogates, `\`, `:`, trailing dots, long paths), more than
  65535 files, timestamps at extremes, `-T1` vs `-T8` byte for byte.

### Running modules (aqr-decompress, aqr-wasmrt)
- run/1: the sandbox worker dying mid-apply, then the Sandbox used again.
- run/2: the 512 MiB bound and `-M` through Native, Wasm and the sandbox (native ignores
  both).
- run/3: the host functions' violations untested: `aqr_spawn` records and regions (past
  4 GiB), `aqr_result`, joining twice, `aqr_take`, `aqr_chan` capacities, send and receive
  bounds, send after close, request/release/commit from a task or of an index not imported.
- run/4: gang message limits and gang-wide deadlocks; task limit 0, concurrency 1 with a gang
  larger than the slots; outputs shorter than declared; an unknown registered hash.
- run/5: native vs Wasm on damaged inputs beyond flac/pcm, `Run::Threaded` modules under
  limits below their plans; determinism across thread counts.

### Extracting and SFX (aqr, aqr-sfx, aqr-sfx-shim)
- out/1: a shim test needing no pack (a text archive of no module); a truncated or damaged
  archive in an SFX (the shim is all or nothing); bundle lengths past the end.
- out/2: a write failure during `-d`: partial and placeholder files left? (bug?)
- out/3: tree entries (not links) named `..`, `.`, `/x`, `C:\x`, UNC, with NUL, device names as
  directories; aqr and aqr-sfx sanitising alike.
- out/4: `-K`/`-t` naming exactly the files lost; a damaged module block without `-A`.
- out/5: IPC during `-d` (finish, cancel, EOF with an `ask` open), lines not UTF-8 (ending the
  reader), non-ASCII paths; session edge cases.
- out/6: zero-file archives; timestamps at extremes; read-only and in-use destinations; a
  parent that is a file; `--stats` on damaged archives; the shim's links, names colliding after
  sanitising, a name sanitised to nothing.
