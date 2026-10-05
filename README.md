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

## Next steps

- Encoder-specific predictors behind the same decisions: exact zlib/libdeflate match finder
  emulation (hash, chain limits, lazy rules, block splitting), which should push the main
  families to near zero.
- More Huffman builders (libdeflate, zopfli, 7-zip, miniz) as candidates before the per-symbol
  fallback, and predicting the block type from cost.
- Richer contexts or mixing for tokens; Huffman code lengths as features for optimal parsers
  (zopfli, ect, 7-zip).
- A larger, more varied corpus, plus the `full` sweep.
