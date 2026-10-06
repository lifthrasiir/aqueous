# flate-recons

Splits a raw DEFLATE stream into its decompressed data and a stream of *decisions*. From the
two, the original stream can be rebuilt bit-exactly. The decisions are predicted from the
data, so almost all of them are "as predicted", and they are meant to be entropy coded
separately (CABAC-style).

```rust
let (data, meta) = flate_recons::deconstruct(&raw)?;          // Metadata: decisions + contexts
assert_eq!(flate_recons::reconstruct(&data, &meta)?, raw);

let priors = flate_recons::Priors::builtin();                 // reference entropy coding
let packed = flate_recons::model::pack(&meta, priors);
assert_eq!(flate_recons::model::unpack(&data, &packed, priors)?, raw);
```

## Architecture

```
raw ─ deflate::parse ─► Stream (every encoder choice explicit) ─┐
                         data ─► predict::code_stream ◄─────────┘
                                   │  x = coder.bit(ctx, x)   (one code path, both ways)
                                   ▼
                       Coder: Recorder → Metadata (decisions per channel)
                              Player   ← Metadata
                              CostCoder (size estimate) / model::pack / your own CABAC
```

- **`deflate`**: a parser and bit-exact writer. `Stream` keeps everything an encoder can choose:
  block types and boundaries, stored-block padding and lengths, exact Huffman headers (HLIT,
  HDIST, HCLEN, the code-length RLE ops), the tokens, symbol 284+31 used for length 258, the
  tail padding and any trailing bytes. `Parsed::new` checks `write(parse(raw)) == raw`.
- **`predict`**: codes a `Stream` as binary decisions against predictions. The same function
  runs in both directions: `x = c.bit(ctx, x)` passes `x` through when deconstructing and
  returns the decoded value when reconstructing, so the two can't drift apart. Per block:
  - type, then size: stored length, or token count ("until end of data" / "same as last
    block" / Exp-Golomb);
  - tokens: whether this is a match, conditioned on the predicted length bucket, the next
    position's match length (laziness), the previous token's kind and zlib's TOO_FAR. The
    length is coded relative to the predicted one. The distance is coded as its **rank among
    the closer candidates that match at least that length**, which is 0 for "closest
    longest match" whatever hash function the encoder used;
  - Huffman header, coded *after* the tokens and predicted from their statistics with zlib's
    `build_tree` emulated bit-exactly. The RLE ops are picked from known generators (zlib
    per-tree, zlib concatenated, libdeflate), falling back to coding them op by op.
- **`matchfind`**: exact match candidates (chains of all earlier positions with the same
  three bytes, within 32 KiB), so they don't depend on any implementation's hash. Short
  streams cache every search across passes. A single pass (reconstruction, or a long
  stream) keeps the chains only for the window behind the search.
- **`ctx`**: context ids in named groups. Each group belongs to a **channel** (`block`,
  `token`, `length`, `distance`, `tree`), so related decisions are stored together and
  can be modelled or stored separately. `LAYOUT` versions the id assignment.
- **`model`**: `ContextModel` gives adaptive per-context probabilities (count-based learning
  rate, starting from trained priors). `pack`/`unpack` are a complete reference coder: one
  binary arithmetic coded stream per channel.
- **`Config`**: search depth and nice length of the token predictor, and optionally zlib
  parameters. It is chosen per stream and stored at the head of the metadata. A generic
  configuration never searches deeper than the 99.5th percentile of how deep the stream's
  matches lie in their candidate chains. Deeper matches are still coded by rank, and the cap
  keeps reconstruction fast: on the held-out set generic streams reconstruct 22% faster, with
  0.8% less metadata, since the in-between depths often predict better than the catalog's own.
- **`zlib`**: a port of zlib 1.3.1's `deflate_fast`/`deflate_slow`/`deflate_rle`/`deflate_huff`,
  `longest_match` and `fill_window`. It uses the same 2 × w_size window, slides and zero-filling,
  so its output matches zlib's bit for bit. It is driven by the actual tokens (`predict`, then
  `commit` of the actual token), so after a misprediction it carries on from where the stream
  really is. In zlib mode the metadata holds the zlib parameters and the number of leading
  blocks zlib reproduces exactly (`predict::zlib_block`: tokens, `lit_bufsize - 1` block
  sizes, `_tr_flush_block`'s stored/fixed/dynamic choice, trees). A stream that is zlib's
  output from start to end costs a few bytes in total. After the first divergence each
  token costs one "as zlib would" decision, with the generic predictor coding the misses. Block
  sizes, stored lengths and fixed versus dynamic are also predicted from zlib.
  Streams of 1 MiB or more also carry **checkpoints** in their exact blocks (at most 32,
  at least 256 KiB apart; about 7 B each): the lazy levels (4-9) insert every position, so
  apart from a few counters (`zlib::Checkpoint`) the emulator's state follows from the data,
  and `Emulator::resume` rebuilds it from the last window. Reconstruction regenerates the
  segments between checkpoints in parallel (`reconstruct_with_threads`, the available
  parallelism by default; the output does not depend on it), checks that each ends in the
  state the next one starts from, and writes the bitstream in parallel chunks. On 32 MB
  of mixed data with 10 threads (4 performance cores), zlib streams cost about 230 B and
  reconstruct 5.3× (level 4), 5.6× (level 6) and 6.6× (level 9) faster than on one thread.
- **`detect`**: a cheap check of whether a stream can come from zlib, after preflate's
  estimator. It checks that every dynamic header is exactly zlib's, that full blocks hold
  `2^(mem+6) - 1` tokens (which gives memLevel), the maximum distance (which bounds windowBits),
  that no match refers to position 0 or uses 284+31, and that there are no far length-3 matches
  for lazy levels. The candidates left are ranked by emulating them over growing prefixes,
  keeping only those tied for the fewest misses (1K, 4K, 16K... tokens). The lazy levels
  (4-9, default or filtered) that share windowBits and memLevel are emulated together, SPMD
  style (`zlib::MultiSlow`). Driven by the actual tokens, they keep identical windows and hash
  chains. Only their lazy-matching state differs, and one walk of each chain gives every
  level's `longest_match` result. The usual settings
  (levels 6, 9 and 1) are tried first, over a single round. The first of them with no miss in
  the first 1,024 tokens is used to code the stream straight away. If that reproduces every
  block, nothing else is tried; ties between levels are not chased, since the coding pass
  checks the whole stream anyway.
  Otherwise all candidates are detected, and the best one competes with the generic
  configurations by cost. Every candidate is recorded while its cost is measured, the cost
  pass stops once it exceeds the best so far, and streams of 512 KiB or more rank the generic
  configurations on their first 256 KiB. memLevel normally comes from the full blocks' size.
  For a single-block stream the default is assumed, and if the best candidate isn't exact,
  only that candidate is retried with the other memLevels the block size allows. When every
  block is fixed, Z_FIXED candidates go first.
- **`Hints`** (in `Options`): what a container says about the encoder. A zlib header's FLEVEL
  gives the level class (0: level 1 or the huffman/RLE/fixed strategies, 1: levels 2-5, 2: 6,
  3: 7-9) and its CINFO gives windowBits exactly. A gzip header's XFL also gives the level class.
  Hints only narrow the candidates; nothing is stored for them, and reconstruction does not
  need them. `cargo run --release -- --hints` in `bench/` feeds the zlib-family samples the
  header their library would have written. Their deconstruction gets about 1.8× faster
  (levels 7-9, including the expensive level 9, are skipped for default-level streams), for
  about 4% more metadata on the near-zlib encoders (zlib-ng, chromium, cloudflare), whose
  best approximation is sometimes a level outside their declared class.

## Training

```
./scripts/make-corpus.sh                                   # corpus/ from local files
cargo run --release --features train --bin flate-recons-train -- samples corpus/*
cargo run --release --features train --bin flate-recons-train -- fit    # writes model/priors.txt
cargo run --release --features train --bin flate-recons-train -- eval
```

`samples` runs every flate-zoo preset (sweep `one`) and caches the distinct outputs under
`cache/samples` (~3 minutes). `fit` (~3 minutes) alternates two steps. First it picks a
catalog of configurations by greedy set cover over a grid. Then it fits per-context initial
probabilities (from the first 16 uses of each context in each stream) and a per-group initial
confidence (by replaying the recorded decisions). Every fifth input file is held out and
reported per backend. `model/priors.txt` is compiled in, and packed metadata records its
hash.

Current held-out result (924 streams, 45 MB of DEFLATE): metadata ≈ **0.92%** of the
DEFLATE size. zlib/zlib-ng ≈ 0.5%; stb, uzlib, ect, zopfli are the worst (2–5.5%).
Most of it is in the token (41%), length (27%) and distance (20%) channels.

## Benchmark

`bench/` compares the size of the reconstruction data with
[preflate](https://github.com/deus-libri/preflate) (C++, v0.3.5) and
[preflate-rs](https://github.com/microsoft/preflate-rs) (0.7.6), with every output
verified by a round trip:

```
bench/fetch.sh                                   # pinned checkouts into third_party/
cd bench && cargo run --release                  # held-out split; per-stream rows in cache/bench.tsv
```

Held-out split, 924 streams. Sizes and CPU times are over the 807 streams all three
round-tripped, measured single-threaded (`RAYON_NUM_THREADS=1`; with all cores busy, CPU times
are inflated unevenly):

| | total | median | p90 | max | failures | split / join CPU |
|---|---|---|---|---|---|---|
| flate-recons | 271,658 B (0.67%) | 112 B | 973 B | 2,991 B | 0 | 7.3 s / 1.9 s |
| flate-recons, `--no-zlib` | 341,146 B (0.84%) | | | | 0 | 3.6 s / 1.1 s |
| preflate | 965,499 B (2.38%) | 521 B | 3,184 B | 23,048 B | 15 | 3.2 s / 2.4 s |
| preflate-rs | 316,814 B (0.78%) | 180 B | 1,021 B | 2,704 B | 117 | 1.7 s / 0.8 s |

Without zlib emulation (`Options::zlib = false`) metadata is 7.7% larger than preflate-rs'.
Almost all of the difference zlib mode makes is on the zlib family: zlib 26.8 → 1.3 KB,
chromium 22.8 → 9.1 KB, zlib-ng 20.4 → 12.1 KB, at 2-4× the time on those streams.

Reconstruction time goes mostly to match searching, which is bound by the latency of
following chain links. The arithmetic decoder takes about 10% of it. zlib-mode streams
reconstruct at zlib's own speed for their level on one thread (about 20 MB/s on average here;
long ones in parallel from checkpoints, see `zlib`), and generic
ones about 1.3× slower than preflate-rs. The extra time mostly comes from the next-position
lookahead, which preflate-rs does not have and which is worth 12.8% of the metadata.

- zlib streams cost a median of 10 B (1,355 B for all 95 of them, against 81,979 B for
  preflate and 11,688 B for preflate-rs). Most of that is the packed container. The emulation
  also helps encoders that only resemble zlib: chromium 19.4 → 5.2 KB, cloudflare
  16.9 → 5.5 KB, zlib-ng 21.7 → 11.8 KB, gzip 8.2 → 4.4 KB.
- flate-recons is smallest on 490 of the 807 streams, preflate-rs on 228 (zlib-ng,
  cloudflare, miniz-oxide and stb, where it has exact models) and preflate on 95. The best of
  the three per stream would total 189,375 B.
- preflate is cheap on its best cases but explodes on others. preflate-rs refuses 13% of the
  streams (`NoCompressionCandidates`, `InvalidDeflate`).
- CPU times are over all 924 streams (85 MB of plain text): flate-recons splits at about
  6 MB/s and joins at about 19 MB/s. `cargo run --release -- --phases` breaks this down.
  Split time is mostly the generic configurations' passes (the depth-4096 one is the best
  for 30% of the non-zlib streams) and zlib detection (about 4.5 s, emulating candidates over
  prefixes). The emulator runs as fast as zlib itself (56 MB/s at level 6, 18 MB/s at
  level 9), so a zlib-mode stream costs about one zlib compression to split and another to join.
  Every candidate configuration is recorded while its cost is measured, so the winner is
  never coded a second time.

### One long stream

The held-out samples are short (93 KB of plain text on average), so they say little about a
single long stream. `-f RAW` runs the tools on one raw stream instead, reporting wall and
process CPU times and the peak RSS (one tool per process). preflate-rs goes through its stream
processor in 128 MiB chunks (`$PREFLATE_RS_CHUNK`), as its own container does. Its
whole-stream API refuses more than that.

```
curl -O https://mattmahoney.net/dc/enwik9.zip && unzip enwik9.zip
../flate-zoo/target/release/flate-zoo compress zlib:level=6 enwik9 enwik9.zlib6.raw
cd bench && cargo run --release -- -f ../enwik9.zlib6.raw -t recons    # also -j 1
```

enwik9 (1 GB of text) deflated to about 323 MB, on an M4 (10 cores, 32 GB). The peak RSS
includes about 1.65 GB of the harness' own buffers (raw, plain text and the reconstruction):

| stream | tool | size | split | join | peak RSS |
|---|---|---|---|---|---|
| zlib -6, 323.9 MB | flate-recons | 318 B | 33.3 s | 23.0 s; 4.3 s on 10 threads | 2.8 GB; 3.5 GB |
| | preflate | 3,209 B | 30.6 s | 23.8 s | 9.0 GB |
| | preflate-rs | 378,216 B (0.117%) | 20.9 s | 17.6 s | 2.2 GB |
| zlib -9, 322.8 MB | flate-recons | 333 B | 39.1 s | 27.4 s; 5.2 s on 10 threads | 2.8 GB; 3.6 GB |
| | preflate | 2,000 B | 34.3 s | 27.8 s | 9.0 GB |
| | preflate-rs | 5,770 B | 24.1 s | 20.5 s | 2.2 GB |
| libdeflate -6, 323.9 MB | flate-recons | 7,383,814 B (2.28%) | 38.4 s | 25.8 s | 4.7 GB |
| | preflate | 10,986,341 B (3.39%) | 40.0 s | 33.0 s | 9.2 GB |
| | preflate-rs | fails in 128 MiB chunks; one chunk: 9,711,758 B (3.00%) | 25.9 s | 15.3 s | 5.3 GB |
| zlib-ng -6, 326.4 MB | flate-recons | 2,227,014 B (0.68%) | 59.1 s | 27.7 s | 4.5 GB |
| | preflate | 14,001,299 B (4.29%) | 37.8 s | 30.8 s | 8.9 GB |
| | preflate-rs | 8,501,612 B (2.60%) | 16.6 s | 13.7 s | 2.4 GB |

The same encoders on the first 256 MB of enwik9 (about 93 MB of DEFLATE):

| stream | tool | size | split | join | peak RSS |
|---|---|---|---|---|---|
| libdeflate -6, 92.9 MB | flate-recons | 2,192,283 B (2.36%) | 11.3 s | 7.7 s | 1.4 GB |
| | preflate | 3,100,326 B (3.34%) | 11.5 s | 9.5 s | 2.5 GB |
| | preflate-rs | fails in 128 MiB chunks; one chunk: 2,649,475 B (2.85%) | 8.3 s | 4.4 s | 1.4 GB |
| zlib-ng -6, 93.7 MB | flate-recons | 625,453 B (0.67%) | 16.7 s | 8.3 s | 1.6 GB |
| | preflate | 3,936,727 B (4.20%) | 10.9 s | 8.9 s | 2.4 GB |
| | preflate-rs | 2,444,022 B (2.61%) | 5.6 s | 4.0 s | 1.1 GB |

- **Size.** The ranking is that of the short streams. A zlib stream costs a constant few
  hundred bytes (0.0001%), against 2-3 KB for preflate. preflate-rs is exact on zlib -9 but
  loses its model on zlib -6 (0.117%, about 1,200× flate-recons). On the near-zlib zlib-ng,
  zlib emulation gives 0.68%, about a quarter of preflate-rs'. The generic predictor gives
  2.28% on libdeflate, 24% less than preflate-rs in one chunk. Its ratio barely moves from
  32 MB to 1 GB of plain text (2.34%, 2.35%, 2.36%, 2.28%).
- **Speed.** flate-recons is the slowest to split zlib streams (about 30 MB/s of plain text:
  parsing, detection and one emulated compression), 1.6× preflate-rs' time. Joining a zlib
  stream runs at zlib's own speed: 1.3× preflate-rs' time, on par with preflate. The
  checkpoints bring it down to 4-5 s on 10 threads (5.3×, for 1.5× the CPU time), the only
  parallel step of the three tools here. A generic stream joins on one thread at 31-39 MB/s,
  1.7-2.1× preflate-rs' time. Its split runs at 15-26 MB/s, 1.4-3.6× preflate-rs' time: it
  ranks several configurations, and for zlib-ng the zlib candidates too.
- **Memory.** Beyond the harness' buffers, flate-recons needs about 1.2 B per byte of plain
  text for a zlib stream (1.9 B when joining on 10 threads) and 3-4.5 B for a generic one,
  against about 7.5 B for preflate and 0.6 B for preflate-rs (3.6 B in one chunk). What
  remains is the parsed tokens (6 B each, about 1.3 B per byte) and, for a generic stream,
  the decision log (2 B per decision, about 1.2 B per byte); the data itself is in the
  harness' share. It used to be 40-55 B per byte for a generic stream, so that 1 GB did not
  fit in 32 GB. The search cache (20 B per position plus the records) is now kept only for
  short streams, which are coded in full under every configuration. Long ones keep the
  chains for the window behind the search only, in a sliding buffer of links relative to its
  base, so a step along a chain is still a single load. Their candidates' decisions are
  logged in 2 B each, in chunks. Nothing else is kept while the winner's decisions are
  replayed, and the coded blocks are checked one by one rather than collected. Long generic
  streams also split and join 7-12% faster. On the short held-out streams, sizes are
  unchanged and splitting is about 3% slower.
- **Robustness.** preflate-rs' chunking breaks on libdeflate's output: the next chunk fails
  (`PredictionFailure`, "Didnt find a match") with 64 MiB and 128 MiB chunks, but the
  whole stream in one chunk round-trips. Its own container uses 128 MiB chunks, and stops
  after 512 MiB of plain text in total, so it would not process any of these streams whole.

## Fast representation (`fast`)

`flate_recons::fast` is an alternative built for speed: at least 100 MB/s of plain text on
one core, both ways (parallel segments are a bonus, not a requirement). It uses no context
modelling and no encoder emulation, so splitting costs about what joining does:

```rust
let (data, meta) = flate_recons::fast::deconstruct(&raw)?;
assert_eq!(flate_recons::fast::reconstruct(&data, &meta)?, raw);
```

- **Rules.** A `Rule` predicts every token from the data alone, with a small hash table of
  recent positions. Its parameters are the key (3 or 4 bytes) and ways per bucket (1-8), plus
  an optional one-entry table keyed by 5-8 bytes. Every position is inserted, or only token
  starts and short matches' insides. Parsing is greedy or one step lazy, with a minimum
  length, zlib-style TOO_FAR and a window. The rules are derived empirically from flate-zoo's
  encoders (the oracle). `flate-recons-fast rules` fits the best rule of each preset class by
  coordinate descent, then a greedy set cover picks a catalog of 32
  (`model/rules.txt`). Each stream's fingerprint maps to one or two of them; the fingerprint
  covers the common shortest match, farthest distance, far short matches, full-block token
  count, block types and how the first Huffman header was built. Two candidates, or an
  unknown fingerprint, are settled by a trial on a 4 KB prefix.
- **Corrections.** Only mispredicted tokens are stored, as a sparse list per class (whether
  the table offered a candidate). Each miss records the gap since the previous one, its kind,
  its length (fresh, or relative to the prediction), and its distance. The distance is coded
  as an entry of the bucket, as its rank among earlier positions matching as long (walking a
  hash chain at the misses only), or verbatim. Fields are coded with static per-stream
  frequencies and rANS (bucketed values plus raw bits). Each field uses a trained default
  model (`model/fast-models.txt`) or the stream's own, whichever is smaller. Nothing adapts.
- **Headers.** Dynamic Huffman headers are coded against candidate builders, chosen per
  stream and carried from block to block: zlib's heap, and package-merge (zopfli, ECT,
  plain) at limits 15-9. Run-length coders cover zlib, libdeflate and zopfli/ECT. An exact
  block costs one bit.
- **Speed.** Tokens are cut into ~1 MiB segments that are coded independently. The parser
  gathers the statistics everything else needs (symbol frequencies, fingerprint, segment
  cuts) as it decodes. Candidate matches are measured on 8 bytes without branching. Exact
  zlib trees use a bottom-up heap with packed keys. Package-merge is computed as Huffman's
  two-queue algorithm whenever the limit does not bite; tests check that it matches zopfli
  bit for bit.

Every rule's cost includes a speed penalty: λ bits per KB of plain text for every ns/B it
takes, by `Rule::ns_per_byte` (measured with `flate-recons-fast speed`). Each λ gives its
own catalog (`rules -l λ`, about 2.5 minutes). Held-out split, 924 streams (45.2 MB of
DEFLATE, 85.1 MB of plain text), single-threaded on an M4. The default models are those
fitted for the λ = 5 catalog. All round-trip.

| λ | metadata | split | join |
|---|---|---|---|
| 0 | 607,711 B (1.343%) | 0.88 s (97 MB/s) | 0.78 s (109 MB/s) |
| 2 | 618,864 B (1.368%) | 0.83 s (103 MB/s) | 0.74 s (115 MB/s) |
| **5 (built in)** | **649,249 B (1.435%)** | **0.81 s (105 MB/s)** | **0.71 s (120 MB/s)** |
| 10 | 679,812 B (1.502%) | 0.78 s (109 MB/s) | 0.68 s (125 MB/s) |
| 20 | 783,402 B (1.731%) | 0.69 s (123 MB/s) | 0.59 s (144 MB/s) |
| 30 | 855,261 B (1.890%) | 0.64 s (133 MB/s) | 0.55 s (155 MB/s) |
| 50 | 1,118,565 B (2.472%) | 0.60 s (142 MB/s) | 0.51 s (167 MB/s) |
| 100 | 1,312,634 B (2.901%) | 0.56 s (152 MB/s) | 0.49 s (174 MB/s) |

```
RAYON_NUM_THREADS=1 cargo run --release --features train --bin flate-recons-fast -- eval \
    -r cache/rules-l5.txt                        # after: rules -l 5 -o cache/rules-l5.txt
```

- Split time at λ = 5 breaks down to parsing 0.14 s, rule choice 0.04 s, token coding
  0.53 s, headers 0.07 s and the rest 0.03 s. Only token coding depends on λ. Joining spends
  0.08 s on headers and 0.09 s on writing.
- Speed costs little ratio below λ = 10: dropping the penalty altogether (λ = 0) gains only
  0.09 points over λ = 5, and misses 100 MB/s. A higher λ first drops the 8-way tables
  (17 of the 32 rules at λ = 5, 4 at λ = 20, none at λ = 100), then ranked distances and
  the second table.
- Against the main representation (0.92% on this split, splitting at about 6 MB/s and
  joining at about 19 MB/s, see above): 1.56× the metadata, splitting 17× and joining 6×
  faster.
- Gaps and kinds (where the misses are and what they are) make 70% of the corrections at
  λ = 5. Lengths and distances make the rest.

## Next steps

- More emulated encoders behind the same scheme (exact prefix, then hit/miss): zlib-ng
  (`deflate_medium`, its own hash), chromium/cloudflare (CRC hashes), libdeflate, miniz,
  stb. The zlib emulation already shows the gain.
- Cheaper zlib detection (preflate-style chain-depth elimination before emulating, or
  emulating the candidates in lockstep).
- Memory for long streams: the parsed tokens and the decision log are kept whole (about
  2.5 B per byte of plain text). Parsing and coding segment by segment would bound them.
- Parallel deconstruction of long zlib streams: checking the exact blocks still runs at
  zlib's speed, although the actual tokens give every segment's starting state.
- More Huffman builders (libdeflate, zopfli, 7-zip, miniz) as candidates before the per-symbol
  fallback.
- Richer contexts or mixing for tokens; Huffman code lengths as features for optimal parsers
  (zopfli, ect, 7-zip).
- A larger, more varied corpus, plus the `full` sweep.
