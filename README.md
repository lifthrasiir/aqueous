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
  three bytes, within 32 KiB), so they don't depend on any implementation's hash.
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

## Next steps

- More emulated encoders behind the same scheme (exact prefix, then hit/miss): zlib-ng
  (`deflate_medium`, its own hash), chromium/cloudflare (CRC hashes), libdeflate, miniz,
  stb. The zlib emulation already shows the gain.
- Cheaper zlib detection (preflate-style chain-depth elimination before emulating, or
  emulating the candidates in lockstep).
- Parallel deconstruction of long zlib streams: checking the exact blocks still runs at
  zlib's speed, although the actual tokens give every segment's starting state.
- More Huffman builders (libdeflate, zopfli, 7-zip, miniz) as candidates before the per-symbol
  fallback.
- Richer contexts or mixing for tokens; Huffman code lengths as features for optimal parsers
  (zopfli, ect, 7-zip).
- A larger, more varied corpus, plus the `full` sweep.
