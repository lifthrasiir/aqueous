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
- **`Config`**: search depth and nice length of the token predictor. It is chosen per stream
  from a trained catalog by estimated cost and stored at the head of the metadata.

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

Held-out split, 924 streams. Sizes are over the 807 streams all three round-tripped:

| | total | median | p90 | max | failures | split / join CPU |
|---|---|---|---|---|---|---|
| flate-recons | 358,867 B (0.88%) | 185 B | 1,218 B | 3,026 B | 0 | 9.1 s / 3.5 s |
| preflate | 965,499 B (2.38%) | 521 B | 3,184 B | 23,048 B | 15 | 5.8 s / 4.6 s |
| preflate-rs | 316,814 B (0.78%) | 180 B | 1,021 B | 2,704 B | 117 | 2.8 s / 1.2 s |

- Where an encoder matches a tool's built-in model exactly, that tool wins by a wide margin:
  preflate on zlib (often 3 B, against our 55–2,358 B for `zlib` at levels 1–9 on one
  file), and preflate-rs on zlib-ng, cloudflare, miniz-oxide and stb. Our generic
  predictor is weakest on zlib's greedy levels 1–3 and on stb/uzlib/miniz-oxide.
- Elsewhere flate-recons is smallest (7-zip, zopfli, ect, libflate, fflate, uzip, libdeflate...).
- preflate is cheap on its best cases but explodes on others. preflate-rs refuses 13% of the
  streams (`NoCompressionCandidates`, `InvalidDeflate`).
- Picking the best of the three per stream would give 208,066 B, so exact encoder models
  and our generic fallback complement each other.
- `model::pack` spends about 30 B of fixed overhead per stream (header, channel sizes,
  4 flush bytes × 5 channels). It dominates on tiny or stored streams (p10: 40 B against
  preflate's 14 B).
- CPU times are over all 924 streams (85 MB of plain text): flate-recons splits at about
  9 MB/s and joins at about 24 MB/s. `cargo run --release -- --phases` breaks this down.
  Most of the split time is the depth-4096 catalog configuration walking long candidate
  chains. That configuration is the best one for 30% of the streams, so it can only go faster by
  changing the model (e.g. a zlib-like `good_length` cutoff) or by choosing the configuration
  from part of the stream.

## Next steps

- Encoder-specific predictors behind the same decisions: exact zlib/libdeflate match finder
  emulation (hash, chain limits, lazy rules, block splitting), which should push the main
  families to near zero (preflate and preflate-rs show what is possible there).
- Less fixed overhead in the packed format (one arithmetic coded stream, or byte-aligned
  channel ends), and cheaper config selection (on a prefix, or a smaller head table).
- More Huffman builders (libdeflate, zopfli, 7-zip, miniz) as candidates before the per-symbol
  fallback, and predicting the block type from cost.
- Richer contexts or mixing for tokens; Huffman code lengths as features for optimal parsers
  (zopfli, ect, 7-zip).
- A larger, more varied corpus, plus the `full` sweep.
