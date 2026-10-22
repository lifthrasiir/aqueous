# aqr-flate

Splits a raw DEFLATE stream into its decompressed data and a *record* of everything else the
encoder chose. From the two, the original stream is rebuilt bit for bit. Tokens are predicted
from the data, by a rule (and by zlib emulation where it applies), so the record holds little
more than the mispredictions. The record's format is in [FORMAT.md](FORMAT.md).

```rust
let (data, record) = aqr_flate::deconstruct(&raw)?;      // default profile (6)
assert_eq!(aqr_flate::reconstruct(&data, &record)?, raw);

let opts = aqr_flate::Options::for_profile(aqr_flate::ADAPTIVE)?;
let (data, record) = aqr_flate::deconstruct_with(&raw, &opts)?; // smallest records
```

## Profiles

The record starts with a profile (u16). The rest of it is laid out the same way for every
profile (the rule, the blocks, the Huffman headers, the segments), and only the coding of
corrections differs.

- **0-9: static codes**, built for speed: at least 100 MB/s of plain text on one core both
  ways from profile 6 down (parallel segments are a bonus, not a requirement). Each profile
  has its own rule catalog, fitted with a speed penalty λ (bits per KB of plain text for
  every ns/B a rule takes, by `Rule::ns_per_byte`, measured with `aqr-flate-train speed`).
  A rule is coded as its index in the profile's catalog.
- **10: adaptive coding**, built for size: corrections are binary arithmetic coded under
  adaptive contexts, with a lookahead, and predictors beyond the catalog's rule (exact
  chains and zlib emulation).

Held-out split, 924 streams (45.2 MB of DEFLATE, 85.1 MB of plain text), single-threaded on
an M4. All round-trip.

| profile | λ | record | split | join |
|---|---|---|---|---|
| 0 | 200 | 1,560,843 B (3.450%) | 0.56 s (152 MB/s) | 0.46 s (185 MB/s) |
| 1 | 100 | 1,305,100 B (2.884%) | 0.57 s (149 MB/s) | 0.47 s (181 MB/s) |
| 2 | 50 | 1,110,953 B (2.455%) | 0.61 s (140 MB/s) | 0.50 s (170 MB/s) |
| 3 | 30 | 848,192 B (1.875%) | 0.65 s (131 MB/s) | 0.53 s (161 MB/s) |
| 4 | 20 | 776,419 B (1.716%) | 0.70 s (122 MB/s) | 0.58 s (147 MB/s) |
| 5 | 10 | 676,811 B (1.496%) | 0.80 s (106 MB/s) | 0.68 s (125 MB/s) |
| **6 (default)** | **5** | **650,173 B (1.437%)** | **0.82 s (104 MB/s)** | **0.69 s (123 MB/s)** |
| 7 | 2 | 615,883 B (1.361%) | 0.84 s (101 MB/s) | 0.71 s (120 MB/s) |
| 8 | 1 | 613,582 B (1.356%) | 0.87 s (98 MB/s) | 0.73 s (117 MB/s) |
| 9 | 0 | 602,111 B (1.331%) | 0.90 s (95 MB/s) | 0.77 s (111 MB/s) |
| **10 (adaptive)** | | **289,911 B (0.641%)** | **10.80 s (7.9 MB/s)** | **1.61 s (53 MB/s)** |

```
RAYON_NUM_THREADS=1 cargo run --release --features train --bin aqr-flate-train -- eval -P 6
```

- Split time at λ = 5 breaks down to parsing 0.14 s, rule choice 0.04 s, token coding
  0.53 s, headers 0.07 s and the rest 0.03 s. Only token coding depends on λ. Joining spends
  0.08 s on headers and 0.05 s on writing.
- Speed costs little size below λ = 10: dropping the penalty altogether (λ = 0) gains only
  0.1 points over λ = 5. A higher λ first drops the 8-way tables, then ranked distances and
  the second table. λ = 200 is hardly faster than λ = 100.
- With static codes, gaps and kinds (where the misses are and what they are) make 70% of the
  corrections at λ = 5. Lengths and distances make the rest.
- Profile 10 is less than half the size of profile 9, and joins at half its speed. Splitting
  codes every stream two or three times (with the catalog's rule, a chain rule, and zlib
  emulation for streams that look like zlib's).

## How it works

- **Rules.** A `Rule` predicts every token from the data alone, with a small hash table of
  recent positions. Its parameters are the key (3 or 4 bytes) and ways per bucket (1-8), plus
  an optional one-entry table keyed by 5-8 bytes. Every position is inserted, or only token
  starts and short matches' insides. Parsing is greedy or one step lazy, with a minimum
  length, zlib-style TOO_FAR and a window. The rules are derived empirically from flate-zoo's
  encoders (the oracle). `aqr-flate-train rules` fits the best rule of each preset class by
  coordinate descent, then a greedy set cover picks a catalog of 32
  (`model/rules-lλ.txt`). Each stream's fingerprint maps to one or two of them; the
  fingerprint covers the common shortest match, farthest distance, far short matches,
  full-block token count, block types and how the first Huffman header was built. Two
  candidates, or an unknown fingerprint, are settled by a trial on a 4 KB prefix.
- **Chain rules** (adaptive profiles). A `chain` rule predicts the closest longest match
  among the first `depth` candidates of exact chains (`matchfind`: every earlier position with
  the same three bytes, within 32 KiB, so nothing depends on an encoder's hash), stopping at
  `nice`. Its depth never exceeds the 99.5th percentile of how deep the stream's matches lie
  in their chains (deeper ones are coded by rank anyway, and it keeps joining fast). Up to
  four configurations are compared by cost, on the first 256 KiB of long streams.
- **Corrections.** Only mispredicted tokens are corrected. Each miss records its kind, its
  length (fresh, or relative to the prediction), and its distance: as an entry of the bucket,
  as its rank among earlier positions matching as long (walking a chain at the misses only),
  or verbatim.
  - Static codes: a sparse list per class (whether the table offered a candidate), with the
    gap since the previous miss. Fields are coded with static per-stream frequencies and rANS
    (bucketed values plus raw bits). Each field uses a trained default model
    (`model/models.txt`) or the stream's own, whichever is smaller. Nothing adapts.
  - Adaptive coding (`aqr_flate::adapt`): every token gets a hit flag, and a miss its fields down
    binary trees, binary arithmetic coded. The contexts are the class, the predicted length
    and the class's last two outcomes, plus, for a match the rule predicts, how the match at
    the next position compares (a lookahead). Priors (`model/priors-10.txt`) come from
    the first 16 uses of each context in every segment of the training split, worth 2
    observations each.
- **zlib emulation** (adaptive profiles). `zlib` ports zlib 1.3.1's `deflate_fast`,
  `deflate_slow`, `deflate_rle`, `deflate_huff`, `longest_match` and `fill_window`, with the
  same 2 × w_size window, slides and zero-filling, so its output matches zlib's bit for bit.
  It is driven by the actual tokens (`predict`, then `commit` of the actual token), so after a
  misprediction it carries on from where the stream really is. While it follows the stream
  it predicts the tokens; the rule's table still serves the corrections, and takes over if
  the emulation is lost. Each segment resumes the emulator from a checkpoint
  (`zlib::Checkpoint`, a few counters: the lazy levels insert every position, so the rest of
  the state follows from the data). deflate_fast (levels 1-3) cannot be resumed, since its
  hash chains depend on the tokens, so such a stream is one segment.
- **Detection** (`detect`): a cheap check of whether a stream can come from zlib, after
  preflate's estimator. It checks that every dynamic header is exactly zlib's, that full
  blocks hold `2^(mem+6) - 1` tokens (which gives memLevel), the maximum distance (which
  bounds windowBits), that no match refers to position 0 or uses 284+31, and that there are no
  far length-3 matches for lazy levels. The candidates left are ranked by emulating them over
  growing prefixes, keeping only those tied for the fewest misses (1K, 4K, 16K... tokens). The
  lazy levels (4-9, default or filtered) that share windowBits and memLevel are emulated
  together, SPMD style (`zlib::MultiSlow`): driven by the actual tokens, they keep identical
  windows and hash chains, and one walk of each chain gives every level's `longest_match`.
  `Hints` narrow the candidates by what a container says (a zlib header's FLEVEL and CINFO,
  a gzip header's XFL), given to `deconstruct_hinted`; reading the record needs no hints.
  A hint can also name the parameters themselves (`Hints::params`, e.g. the previous
  stream's `Report::zlib` in a container of many streams): if they pass the structural
  checks and predict every token, they are taken without detection, the rule's trial or the
  chain rule (short streams below 1 KiB still compare the rule alone). On small git objects
  (zlib -6, profile 10) this splits 1.5-5× faster than with the header's hints alone.
- **Headers.** Dynamic Huffman headers are coded against candidate builders, chosen per
  stream and carried from block to block: zlib's heap, and package-merge (zopfli, ECT,
  plain) at limits 15-9. Run-length coders cover zlib, libdeflate and zopfli/ECT. An exact
  block costs one bit.
- **Speed.** Tokens are cut into ~1 MiB segments that are coded independently. The parser
  (see [below](#parser-and-writer-flate_reconsinflate-flate_reconswrite)) gathers the statistics everything
  else needs (symbol frequencies, fingerprint, segment cuts) as it decodes. Candidate matches
  are measured on 8 bytes without branching. Exact zlib trees use a bottom-up heap with packed
  keys. Package-merge is computed as Huffman's two-queue algorithm whenever the limit does not
  bite; tests check that it matches zopfli bit for bit.
- **`deflate`**: a parser and bit-exact writer. `Stream` keeps everything an encoder can
  choose: block types and boundaries, stored-block padding and lengths, exact Huffman headers
  (HLIT, HDIST, HCLEN, the code-length RLE ops), the tokens, symbol 284+31 used for length 258,
  the tail padding and any trailing bytes.

## Training

```
./scripts/make-corpus.sh                                   # corpus/ from local files
cargo run --release --features train --bin aqr-flate-train -- samples corpus/*
cargo run --release --features train --bin aqr-flate-train -- rules -l 5     # model/rules-l5.txt
cargo run --release --features train --bin aqr-flate-train -- models         # model/models.txt
cargo run --release --features train --bin aqr-flate-train -- priors         # model/priors-10.txt
cargo run --release --features train --bin aqr-flate-train -- eval -P 10
```

`samples` runs every flate-zoo preset (sweep `one`) and caches the distinct outputs under
`cache/samples` (~3 minutes). Every fifth input file is held out. `rules` takes about 2.5
minutes per λ, and `priors` about 15 seconds. The catalogs, default models and priors are
compiled in.

## Benchmark

`bench/` compares aqr-flate with preflate (C++) and preflate-rs on the held-out split
(`bench/fetch.sh` gets them; every output is verified by a full round trip):

```
cd bench && RAYON_NUM_THREADS=1 cargo run --release -- [-P PROFILE]
```

Sizes over the 807 streams that every tool round-trips (40.6 MB of DEFLATE); CPU times over
all 924 streams, one thread, on an M4:

| tool | reconstruction data | streams failed | split | join |
|---|---|---|---|---|
| aqr-flate, profile 10 | 207,798 B (0.512%) | 0 | 11.2 s | 1.6 s |
| aqr-flate, profile 6 | 468,358 B (1.153%) | 0 | 0.8 s | 0.7 s |
| preflate | 965,499 B (2.378%) | 15 | 3.9 s | 3.1 s |
| preflate-rs | 316,814 B (0.780%) | 117 | 1.8 s | 0.8 s |

- preflate-rs is smaller on encoders it models exactly or nearly (gzip, miniz-oxide, sdefl,
  zlib-cloudflare, zlib-ng), and refuses 13% of the streams (`NoCompressionCandidates`,
  `InvalidDeflate`). preflate is cheap on its best cases but explodes on others.
- aqr-flate's zlib streams cost 5.3 KB over 95 streams, most of it the block layout: zlib
  at small memLevels writes many stored blocks, each with its length coded up front.

## Parser and writer (`aqr_flate::inflate`, `aqr_flate::write`)

`aqr_flate::inflate::parse` gives what `deflate::parse` gives, with the tokens in one list and
the per-block statistics gathered on the way. `parse_into` hands the data and tokens to a
`Sink` (`lit`, `mat`, `stored`) instead, for callers that process tokens as they are decoded
(aqr-png models them in the same pass); `parse` is `parse_into` with a collecting sink.

```rust
struct Count(usize);
impl aqr_flate::inflate::Sink for Count {
    fn lit(&mut self, _: u8) { self.0 += 1 }
    fn mat(&mut self, len: u16, _dist: u16, _alt258: bool) { self.0 += len as usize }
    fn stored(&mut self, b: &[u8]) { self.0 += b.len() }
}
let layout = aqr_flate::inflate::parse_into(&raw, usize::MAX, &mut Count(0))?;
```

- Decoding tables are two-level: 11 bits (literal/length) and 8 bits (distance) up front,
  subtables for longer codes, so they stay in L1. Single-level tables as long as the
  longest code took up to 128 KB with the 15-bit literal codes common in image data.
- Up to three literals per refill, the last two straight from the primary table.
- Matches closer than 8 bytes copy their first period byte by byte and the rest in 8-byte
  chunks from a multiple of the distance (a distance of 4, an RGBA pixel, used to go byte
  by byte for the whole match).
- The collecting sink keeps its vectors as raw parts grown by value, so that their lengths
  stay in registers in the token loop.
- The writer flushes its 64-bit buffer once per token without a branch: the 8 bytes go to
  the end of the output (capacity is reserved per block) and the output grows by the whole
  bytes among them.
- `header::read_freqs` reads a header against frequencies the caller counted itself (as
  aqr-png does while decoding), sparing `header::read`'s pass over the tokens.

One thread, best of 7 runs (`aqr-flate-train parse` on the held-out samples; `parse --png
FILE...` on PNG files' IDAT streams, from [codec-corpus](https://github.com/imazen/codec-corpus)).
Speeds are of the decompressed data.

| input | data | parse before | parse after | write before | write after |
|---|---|---|---|---|---|
| held-out samples (924 streams, 45.2 MB) | 85.1 MB | 630 MB/s | 627 MB/s | 1,042 MB/s | 1,720 MB/s |
| kadid10k (81 photos, 25.9 MB, zlib -6) | 47.8 MB | 451 MB/s | 461 MB/s | 719 MB/s | 1,278 MB/s |
| clic2025 final-test (30 photos, 121.2 MB, zlib -9) | 258.8 MB | 408 MB/s | 524 MB/s | 825 MB/s | 1,385 MB/s |
| qoi-benchmark screenshot_web (14 files, 40.3 MB) | 455.1 MB | 1,600 MB/s | 2,441 MB/s | 4,982 MB/s | 8,138 MB/s |
| gb82-sc (10 screenshots, 3.0 MB) | 92.7 MB | 1,785 MB/s | 3,141 MB/s | 8,238 MB/s | 12,427 MB/s |

Metadata sizes are unchanged. In `eval`, parsing takes 0.14 s either way and joining 0.68 s
instead of 0.71 s (the built-in row of the λ table above; the other rows were measured with
the old writer and would gain about as much).

## Next steps

- Predict the block layout of zlib streams from the emulation (stored lengths, fixed versus
  dynamic, block sizes), rather than coding it up front.
- More emulated encoders behind the same scheme: zlib-ng (`deflate_medium`, its own hash),
  chromium/cloudflare (CRC hashes), libdeflate, miniz, stb.
- Cheaper splitting in profile 10: the chain configurations and zlib detection are tried in
  full unless hinted parameters are exact.
- More Huffman builders (libdeflate, 7-zip, miniz) as header candidates.
- A larger, more varied corpus, plus the `full` sweep.
