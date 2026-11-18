# Glossary

Words of this codebase whose meaning isn't obvious at a glance. Words that mean what they
say (encoder, batch, level, …) are left out. Terms the spec defines are marked *(spec)*, and
the spec stays normative for them. For implementation terms the doc comment at the named place
is the precise definition.

## Recompression

**recons** — Short for "reconstruct", the suffix of most module names (`png-recons`,
`lzma-recons`). A recons module rebuilds the original coded bytes from the decoded data and a
**record**.

**record** — What a recompressor stores besides the decoded data: the encoder's choices that
can't be derived from the data (mispredicted tokens, block boundaries, header shapes). Each
codec's `FORMAT.md` describes its record.

**emulation** vs. **record of the parse** — The two ways a codec makes its record.
- *Emulation* reimplements one encoder bit for bit (aqr-flate's zlib, aqr-bz2's libbzip2) and
  stores only where it misses.
- *A record of the parse* emulates nothing. It codes the encoder's choice of matches given the
  data. It is used where there are too many encoders (aqr-lzo, aqr-lzma, aqr-rar, …).

**profile** (aqr-flate) — The first field of an aqr-flate record.
- 0–9: static codes tuned for speed; 6 is the default.
- 10: the adaptive one, about half the size and much slower. It is kept off big streams by
  default.

**hints** — What a container says about its encoder, such as a zlib header's FLEVEL. Hints only
narrow the search and are never needed to decode.

## The archive format

**leaf** / **non-leaf** / **auxiliary block** *(spec)*
- A *leaf block* gives a file's content: a data block, or a formula block.
- A *non-leaf block* only holds data that formulas slice.
- An *auxiliary block* (dictionary, module, resource) helps decode the others.

**parcel** *(spec)* — A leaf block and the non-leaf blocks after it. It is about all a decoder
reads to extract one file.

**critical** *(spec)* — Told by a block type's leading hex digit (0–9). An unknown critical
block fails the read; an unknown non-critical one is skipped.

**shorthand block** *(spec)* — A module or dictionary block holding only the SHA-512/256 of a
registered entry instead of its bytes. aqr writes them only for self-extracting archives.

**tombstone** *(spec)* — An entry of a later segment that removes an entry of an earlier one.

**spacer block** *(spec)* — Zero-filled room. Appending turns the old footer into one (by its
first 2 bytes), which is how a **commit** keeps the file a valid archive at every moment.

**tree archive** vs. **single-file archive** — With a metadata block, the archive holds a tree
of entries. Without one, it holds one unnamed file, as gzip does. The code branches on this
often (`Payload::of_tree`).

**packed byte sequence** *(spec)* — Several byte sequences given to a module as one input, for a
module whose number of things varies (an executable's sections). The other direction is a
**Split** of one output. "No variable arity" in CLAUDE.md means using these and separate
modules instead.

**released modules** (`crates/aqr-formula/src/released.rs`) — Hashes of modules shipped in past
releases. Only these may use the old Apply (opcode 0x11), and their memory limits are not
enforced.

## Formulas in memory (aqr-formula)

**Stream** / **class** / **Kind**
- A `Stream` is data a recompressor extracted, not yet placed in blocks.
- Its `class` (`x86/OP`, `flate/record`) groups streams that are alike: they are merged, sunk
  together and given one dictionary.
- Its `Kind` (`Text`, `Pixels`, `Compressed`, `Format(name)`, …) says what the bytes are.
  aqr-compress decides from it whether to look inside, and the encoder decides how hard to
  compress (`Compressed` → `How::Light`, stored unless zstd gains).

**content** (of a stream) — The stream is data as it is, so aqr-compress may recompress it in
turn. "The contents" of a formula are the nested data it recurses into: a DEFLATE stream's
data, a ZIP entry. In the spec, *content* is just a file's bytes.

**apart** — `Stream::apart` is a stream never merged with others, because other formulas slice
it in pieces that each must decode alone. "Made apart" in aqr-compress means made on its own
thread in its own `Cx`.

**sunk** / **lazy** (`Bytes`) — A *sunk* stream's bytes are already in a sink: only its length is
left, and reading it is a bug. A *lazy* one is made again whenever it is needed, so nothing
holds it in the meantime.

**Shared** / **Output** — An application with several outputs, or a node cut by a Split, is
listed apart in `Formula::shared`. Each output is taken exactly once by a `Node::Output`. There
is no `Node::Split`.

**Either** — Two ways to make the same output: the raw data (`kept`) and the formula (`made`).
It exists only in memory; the encoder refuses one. It is left where a formula lost by little,
and resolved once the batch is laid out (see **defer**).

**share_slices** — A slice holding the same bytes as an earlier slice points at it instead, so a
file and its copy inside a ZIP are stored once. The sink does the same across formulas when it
**seals** them.

**merged** — `merge_classes` concatenates the streams of a class and kind. `merged()` does it
without moving bytes and returns the pieces of each merged stream for the encoder to read.

**embed** — Takes a nested recompressor's formula into its parent's.

## Writing an archive

**smaller** — The question asked everywhere: *does this formula archive in fewer bytes than the
data kept raw?*
- `Encoder::smaller` (and `_with`, `_but`) answers it from real encoder sizes.
- aqr-compress's `smaller` / `verdict_as` adds the policy: data held elsewhere is free, and
  some formats are taken without comparing. The doc comment of `smaller` in
  `crates/aqr-compress/src/lib.rs` lists them and why.
- The answer is a **Verdict**: `Take`, `Reject`, or `Close` (lost by little).

**sink** (`crates/aqr-formula/src/aqr/sink.rs`) — Streams written block by block, each block
compressed as soon as it is full. No whole stream is held, yet the archive is byte for byte
what whole streams would give. A **direct sink** writes the blocks straight into the output
file, and the formula block goes into room left before them.

**seal** — `Sink::seal(formula)`: the formula's bytes go into the sink, its slices are pointed
there, and its streams become sunk. The archive's bytes depend on the order of sealing, so
aqr-compress seals in a fixed depth-first order whatever the threads
(`crates/aqr-compress/src/seal.rs`: an `Order` of `Slot`s). `wait("seal")` in `--stats` is time
spent waiting for that turn. "봉인" in the design docs means this. `Sealer` (encryption) is an
unrelated sense.

**spill** — Large buffers kept in a sparse temporary file next to the output and read through
mappings, so the OS can page them out.

## Compressing (aqr-compress)

**Cx** — The context carried through recompressing one piece of data: the sizing encoder,
**held**, the seal slot, the budgets and the notes.

**Nest**, **scan**, **extent**
- `Nest` says how data is looked into: as one file of a format (`Exact`), by a scan for files
  anywhere (`Scan`), or as a given format (`As`).
- A *scan* cuts each file found out by its format's *extent*, where such a file ends, and keeps
  the gaps raw.

**probe** — A cheap guess, before recompressing a coded stream, of whether it will pay. It is
off from level 7.

**held** — An index of what the files of a batch contain. Data held by other files of the batch
costs nothing when sizing, because it will be stored once (`note_held`).

**defer** — For formulas that lost by little: keep both as an `Either`, within a budget, and
choose once the batch is laid out (`choose.rs`, `docs/cross-file-design.md` §4.3, `-Z defer:*`).

**laid** — The step that lays out a batch's formulas: a leaf per file, and streams merged into
solid blocks by class and kind.

**together** — Two senses:
- In `archive.rs`, files that may hold each other (a file and a gzip of it) are put in one
  batch.
- `-Z together:always` puts every stream of a batch into one solid stream.

**bundle** — An archive that only bundles files: tar, ar, cpio, disk images, game packs. It
gains nothing itself; its members become contents to recompress and deduplicate. The image's
other bytes become a `rest` stream, with no module.

**siblings** — Files with no header of their own, taken apart by another file archived beside
them (a VPK's numbered archives through its `_dir.vpk`, Unity's `.resS`).

**sets** — Near-duplicate PNGs coded against each other along a spanning tree, as one
application.

**Undo** — A decoding a recompressor asks to be put back if nothing comes of its data.

**prevalidation** — Each formula is evaluated on other threads while the file is still being
made. Its applications are remembered by hashes, so verifying the written archive checks hashes
instead of running modules again.

## Modules

**whole module** / **part module** / **window** *(spec)* — A whole module takes all its inputs at
once. A part module streams them through fixed *windows* of pages, so its memory does not grow
with the data. Natively, some part modules are still decoded whole on threads
(`Run::Threaded`, `crates/aqr-native`).

**task class**, **slots**, **gang** *(spec)* — Tasks are copies of a module instance that return
bytes. Each class has 1–64 slots, the most that may run at once. A gang's tasks start together,
so they may talk through channels. The usual way in is `aqr_common::pool::par_map_apart`.

**plan** — The exact bound, to the page, on a module's working memory, computed from an
application's lengths and params (`Module::with_plan`). The module allocates in the LIFO order
the plan replays (`aqr_api::stack`). The encoder declares it in the Apply, and the `*_plans_hold`
tests check it. `docs/arena-design.md`.

The three memory caps are easy to confuse:
- **application bound** caps one application's declared memory: 256 MiB when compressing (a
  formula over it is not used) and 512 MiB when decompressing (over it fails).
- **allowance** caps the heap one recompression may take. Over it, the recompression unwinds
  and the data is kept raw.
- **budget** (`-M`) caps the heap the whole process's work holds at once. Work waits for room.

**needed_by** (aqr-decompress) — The memory an application needs. The encoder declares limits
with it, and the decoder checks native runs with the same function.

**emu** (`aqr_api::emu`) — The native stand-in for the Wasm host (tasks as threads), so module
code runs natively. Unrelated to emulating an encoder.

**known module** — One whose hash `crates/aqr-modules/src/known.rs` lists, so it runs natively.

## Self-extracting archives

**blob** — A native module library digested to what placing it in memory takes. Blobs are
loaded at run time (`aqr_blob::load`) or merged into a stub executable (`aqr-ld merge`).

**interpreter blob** — A blob of Wasmtime alone. It runs any module that has no native blob, and
on Windows aqr.exe runs Wasm through it.

**stand-in module** — A Wasm module with no code, only its hash and task slots. An SFX gives it
for a shorthand block, and its runner routes it to the native blob.

**pack** (`sfx.pack`) — Everything `aqr --format sfx` needs (the stub and the blobs), embedded in
aqr.exe. Unrelated to the Pack instruction and to aqr-pack.

## Overloaded words

| Word | Senses |
| --- | --- |
| class | a stream's class · a task class *(spec)* |
| seal | into the sink · encryption (`Sealer`) |
| Sink | the block sink · the byte consumer `aqr::Sink` · aqr-flate's `inflate::Sink` |
| segment | an archive segment *(spec)* · a codec's unit coded on its own (aqr-flate's ~1 MiB) |
| slice | `Node::Slice` · a recovery-set slice *(spec)* |
| params | an Apply's params · aqr-flate's `zlib::Params` |
| bundle | tar-like formats · an SFX's bundle of blobs |
| stand-in | an SFX's code-less module · `smaller_with`'s stream given as pieces · a `Module` with no plan, in tests |
| level | `-0`..`-9` · a conformance level *(spec)* · zstd's level |
