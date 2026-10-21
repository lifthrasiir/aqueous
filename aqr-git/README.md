# aqr-git (`gitr`)

Lossless recompression of git checkouts: a directory with its `.git` (submodules under
`.git/modules` included) becomes ten interconnected streams, and every file is rebuilt bit
for bit from them. The working tree costs almost nothing (its files are the repository's
blobs), pack and loose objects lose their zlib layer (which zstd cannot see through), and
object ids in trees, commits, indexes and refs become small references. Meant as a component
of a general-purpose compressor, so speed is bounded: by default the transform runs at about
100 MB/s single-threaded on an Apple M4 both ways, rebuilding as many zlib streams as that
allows ([Trade-offs](#trade-offs)).

```sh
cargo build --release
gitr c [-l LEVEL] [-B NS | -A] [-O ORDER] [-k] [-t N] DIR OUT   # compress a directory
gitr d IN DIR                                                    # decompress into DIR
gitr x IN PATH                                                   # one file to stdout
gitr bench [-l LEVEL] [-B NS | -A] [-O ORDER] [-k] [-t N] [-v] DIR...   # round trip, sizes, times
```

`-B NS`: the time budget, ns per input byte (default 10: 100 MB/s); `-A`: rebuild every zlib
stream (smallest output, about 25 MB/s); `-B 0`: none. `-O largest|smallest|gain`: the order
streams are offered to the budget (default `gain`); `-k`: pass over streams that do not fit.
`-l`: zstd level (default 19); `-t`: zstd workers (the output does not depend on them).

```rust
let d = aqr_git::deconstruct(dir, &aqr_git::Options::default())?;  // streams, symlinks, hashes
let files = aqr_git::reconstruct(&d.streams)?;                         // every file
let a = aqr_git::Archive::open(&streams)?;                             // or one at a time
let bytes = a.get(".git/objects/pack/pack-….idx")?;
```

## Results

`corpus/local`: 15 repositories from `~/Works/git` (`scripts/collect-local.sh`: the whole
`.git` plus the files git would track or not ignore), 1196 MB in all. Apple M4; the
transform on one thread (wall time, `examples/prof.rs`); every file round-trips.

| | output (zstd -19) | of input |
|---|---|---|
| `tar \| zstd -19 --long=27` | 778.6 MB | 65.1% |
| `gitr -B 0` (zlib streams kept) | 668.7 MB | 55.9% |
| **`gitr` (default: 100 MB/s budget)** | **540.9 MB** | **45.2%** |
| `gitr -A` (every zlib stream rebuilt) | 304.8 MB | 25.5% |
| `gitr -l 3` (default budget, zstd -3) | 587.5 MB | 49.1% |

Per repository (default options):

| repository | input | output | deconstruct | reconstruct | working memory (dec./rec.) | `-A` output |
|---|---|---|---|---|---|---|
| pyre | 664.9 MB | 313.5 MB | 100.9 MB/s | 137.2 MB/s | 69 / 62 MB | 160.5 MB |
| SaturnTranslatorInternal | 142.9 MB | 44.1 MB | 99.1 MB/s | 136.9 MB/s | 43 / 7 MB | 32.4 MB |
| wah | 125.6 MB | 36.7 MB | 109.2 MB/s | 455.7 MB/s | 22 / 18 MB | 3.2 MB |
| loupe | 119.0 MB | 49.4 MB | 172.6 MB/s | 749.8 MB/s | 250 / 139 MB | 46.3 MB |
| unison | 59.6 MB | 33.3 MB | 105.1 MB/s | 138.5 MB/s | 32 / 17 MB | 22.1 MB |
| aqueous | 38.7 MB | 32.4 MB | 107.3 MB/s | 435.2 MB/s | 14 / 6 MB | 21.0 MB |
| sudachi.rs | 20.1 MB | 17.8 MB | 113.0 MB/s | 135.0 MB/s | 9 / 7 MB | 13.1 MB |
| wasm3-rs | 12.0 MB | 7.7 MB | 111.8 MB/s | 142.9 MB/s | 11 / 4 MB | 5.3 MB |
| duc | 9.0 MB | 4.5 MB | 110.4 MB/s | 475.7 MB/s | 5 / 1 MB | 0.2 MB |

(The six smallest, 0.003-1.5 MB, run at 71-240 MB/s deconstructing.) With `-A`: pyre 24.5 / 28.1 MB/s,
unison 27.6 / 30.3 MB/s, SaturnTranslatorInternal 42.4 / 48.8 MB/s.

zstd is not in these times. At level 19 with libzstd's workers on 10 cores it takes longer
than the transform (pyre: 24.5 s); `-l 3` makes it negligible (pyre: 0.26 s) for 8.6% more
output.

Working memory is the heap beyond the input and the output (`examples/prof.rs`); see
[Memory](#memory). loupe's is that of its largest objects, held whole.

## Format

[FORMAT.md](FORMAT.md) specifies it. In short:

```text
magic 69 04 7f 82 2b 43 ("aQR/gitD") | u16 LE version (1)
10 streams (files, sha, entry, ops, refs, blob, tree, commit, raw, text), each zstd or stored
symlinks | per file: xxh3
```

## How

- **File table.** Paths front-coded, a kind per file: RAW, SAME (as another file), BLOB and
  BLOB_CRLF (a blob of the repository, possibly with CR LF line ends: the working tree, and
  blobs under `.git` such as `lost-found/other`), LOOSE, PACK, IDX and REV (derived from
  their pack), GIT (any other `.git` file, with its ids and trailing SHA-1 taken out).
- **One table of object ids** for all repositories in the input (`sha`), in first-occurrence
  order, from the packs' .idx files and the loose objects' paths. An id anywhere (tree
  entries, commit and tag headers, ref-deltas, the index, refs, reflogs, commit-graph) is a
  reference relative to the previous one. The table is a dictionary: nothing is hashed to
  rebuild, and any file can be rebuilt alone.
- **Objects.** Every pack entry and loose object has a record: its type, sizes that cannot be
  derived, how its zlib stream is rebuilt, and its parts of the streams. Payloads are coded
  as git would read them: whole objects through a state machine that turns tree ids and the
  `tree`/`parent`/`object` lines of commits and tags into references (the `tree` and
  `commit` streams), deltas as instructions (`ops`: inserts, copies relative to the previous
  copy's end) with their inserts coded the same way, the state following the copied bytes of
  the base. What git would not have written that way (non-canonical headers or deltas) stays
  verbatim.
- **zlib streams** are rebuilt by emulating zlib 1.3.1 (aqr-flate's port, bit-exact) at the
  level the header allows, or kept. Which are rebuilt is the cost model's choice
  ([Trade-offs](#trade-offs)).
- **Packs** are rebuilt entry by entry; .idx (version 2) and .rev files from the pack (ids,
  offsets, CRC-32s) and its trailer (the pack's SHA-1, or the hex in its file name).

## Trade-offs

### Exact zlib, and why it is the bottleneck

git compresses every object with zlib: each pack entry and each loose object is a zlib stream
of its own, and zstd gains little over zlib output (`-B 0` leaves 668.7 MB, against 304.8 MB
with every stream rebuilt). Rebuilding a stream bit for bit means running zlib's match search
again, on both sides:

- aqr-flate's emulation runs at 81-85 MB/s of data, as fast as zlib 1.3.1 itself (78.7
  MB/s on the same objects, level 6). Its `longest_match` is 51% of the time: 18.3 chain steps
  per call at 2.2-2.4 ns a step.
- Pack-heavy repositories have about twice as much data as input (unison: 124 MB of payloads
  for 59.6 MB), so rebuilding everything caps the transform at about 40 MB/s of input; it
  measures 24.5-48.8 MB/s (`-A`).

Alternatives measured and not taken:

- **aqr-flate's predicted corrections instead of emulation** (a rule predicts the tokens,
  misses are coded). Per stream, its records average 88 bytes per git object (2.1 MB for
  unison's 24k objects): too much per small stream. Batched per pack (one rule and one set of
  models, corrections in one rANS stream; prototyped, then removed), the whole corpus came to
  364 MB instead of 305 MB (+19%) for 1.3-1.5× the speed (pyre 31/41 MB/s, unison 34-41/48-64
  MB/s depending on λ). On git's zlib-6 streams the best static rule costs 3.2% of the DEFLATE
  bytes at 106 MB/s of data, the fastest 8.3% at 202 MB/s; speed saturates at λ ≈ 50 because
  mispredictions are expensive to decode (rank walks: 17% of the time).
- **Keeping every zlib stream** and getting the working tree by inflating objects: 160-330
  MB/s deconstructing and 350 MB/s or more reconstructing, but 668.7 MB.
- **Keeping streams below a size threshold**: the small objects are much of the gain (6
  repositories: +3.3% output at 1 KiB, +6.6% at 4 KiB, +20% at 64 KiB, for 1.05-1.6× the
  speed).
- **Interleaving independent chain walks** (SPMD across objects, K = 2-16 lanes, state in
  registers): at best 1.05×. The walks are not latency-bound; the M4's out-of-order core
  already overlaps them, and the cost is in mispredicted comparisons (9.1% of steps pass the
  first check, 6.8% go on to compare a match).

### The budget

So the transform rebuilds what fits in a time budget and keeps the rest. The choice must not
depend on a clock (the output would not be reproducible), so it rests on a cost model of the
time each side takes (`src/budget.rs`):

- The base work: per file, per byte of packs, of loose objects and of the working tree, per
  object. Fitted to 120 runs (15 repositories, several budgets and orders): relative error
  9.3% rms deconstructing, 14.2% reconstructing (the largest errors are runs of a few
  milliseconds, and loupe's huge loose blobs, whose inflating is not modelled).
- Each rebuilt stream: zlib's search, estimated from the stream (bytes of the zlib stream,
  literals, matches, and how many earlier positions in the window share each position's
  hash, capped at 32), by deflate_fast or deflate_slow and by size class. On 82k objects:
  3-19% rms per object, per repository within ±3% (level 1) and ±10% (level 6). Its inputs
  need the stream parsed, which is done only for the streams offered to the budget. Levels 7-9
  are a guess (twice level 6) for want of samples.

The resulting speeds are in [Results](#results): 87-117 MB/s deconstructing (the binding
side), whatever the repository. The exception is loupe, whose largest loose object does not
fit the budget: nothing is rebuilt, and the run is faster (172 MB/s).

**The order** in which streams are offered matters as much as the budget. At 10 ns/byte
(whole corpus, zstd -19):

| order | output | |
|---|---|---|
| largest first | 604.2 MB | |
| smallest first | 631.3 MB | better on 10 of 15 repositories (by 1-15%), worse on pyre (+10%) |
| **gain first** (default) | **540.9 MB** | pyre −15%, unison −10%, SaturnTranslatorInternal −6% against largest first |
| gain first, passing over what does not fit (`-k`) | 538.1 MB | but 64-108 MB/s: the streams passed over are parsed, which the model does not count |

"Gain" is the output saved per time, estimated from the headers alone: the zlib stream's
size times a ratio by class, over a rough cost from the sizes. The ratios are medians of
what rebuilding each class alone saves per byte of zlib stream:

| class | unison | SaturnTranslatorInternal | wah | sudachi.rs | aqueous | used |
|---|---|---|---|---|---|---|
| commits, tags | 0.80 | 0.63 | 0.8 | 0.56 | 0.7 | 0.7 |
| trees | 0.91 | 0.62 | 0.8 | 0.62 | 0.7 | 0.7 |
| blobs | 0.39 | 0.51 | 0.54 | 0.15 | 0.17 | 0.4 |
| deltas | 0.15 | 0.28 | 0.31 | 0.45 | 0.38 | 0.3 |
| loose objects | 0.68 | 0.33 | 0.33 | | 0.38 | 0.4 |

The spread within a class (0.15-0.54 for blobs) is the content: how much an object shares
with others once zstd sees it unzlibbed. A per-object estimate would need its content,
which costs budget; within a class the order is by size, so loose-object repositories (wah,
loupe) are still served largest first.

### Memory

Working memory does not grow with the input beyond a few bytes per object:

- Deconstruction: the table index (8 bytes per id; the ids themselves are the `sha` output),
  16 bytes per pack entry, a 32 MiB cache of resolved objects (delta bases, trees for typed
  literals), and staging buffers flushed to the output after each file or entry.
- Reconstruction: a cursor every 64 files and every 64 entries of each pack; records parsed
  again from the nearest cursor when needed (the last 16k kept); the same object cache; a
  rebuilt pack's offsets and CRC-32s until its .idx is written. The `sha` stream is read in
  place.
- Objects are held whole, so the largest object sets a floor (loupe: 250 / 139 MB).

The first version held per-object records and every pack in memory: a whole `bench` of pyre
peaked at 6.4 GB RSS.

### The table

The ids are stored (`sha`, 18.4 MB, 3.4% of the default output): deriving them would mean
hashing every object, which needs every object resolved, and no file could be rebuilt alone.

### Not kept

File modes, times, empty directories, hard links; non-UTF-8 paths are refused. Symlinks are
kept as targets. SHA-256 repositories, .idx versions other than 2 and packs without an index
are coded as plain git-dir files (correct, but without their objects' gains).

## Tools

- `examples/prof.rs`: transform speed and working memory per directory (`BUDGET`, `ORDER`,
  `SKIP`, `CACHE_MB` in the environment).
- `examples/calib.rs`, `scripts/fit-budget.py`: the base model, from runs at several
  budgets.
- `examples/emucost.rs`, `scripts/fit-emulation.py`: the per-stream emulation model, from
  packs and loose object directories.
- `examples/dump.rs`: a directory's streams, raw.
- `tests/roundtrip.rs` builds repositories with the git CLI (loose objects, gc'd packs,
  ref-deltas, core.compression 0/1/9, CRLF checkouts, a submodule, a tag), checks every file
  rebuilt alone and together with every stream rebuilt, none, and the default budget, and
  flips bytes in every stream.
