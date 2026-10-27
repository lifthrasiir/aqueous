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

// parsed once: the data and an estimate of the record before anything is coded
let opts = aqr_flate::Options::default();
let parsed = aqr_flate::Parsed::head(&file[start..], &opts)?;  // what follows is not part of it
if parsed.estimate(&opts)? < budget {
    let (data, record, _) = parsed.deconstruct(&opts, &aqr_flate::Hints::default())?;
}
```

`Parsed::estimate` codes runs of tokens spread over the stream under its rule and scales them
to the whole (the blocks and headers exactly); what it computes is not computed again by
`deconstruct`. The parser keeps the tokens of the first 16 KiB, then of runs of 16 KiB placed
64 KiB apart, or an eighth of the data before apart (about 8 per doubling, the stream's length
not known as it is parsed), the first 8 KiB of each only filling the rule's tables. All are
counted into one coder, as the segments share their codes. On 10 streams (the 9 gzip files of
aqr-compress's check, below, and a fuzzing input that starts with 600 KB of one byte) it comes
to 0.96× to 1.80× of the record, 1.14× in all; the first 128 KiB alone came to 0.27× to 5.2×
(the fuzzing input). aqr-compress and aqr-zip use it to rebuild only the streams that may pay
(`aqr_formula::probe`).

## Profiles

- **0-9: static codes**, built for speed: at least 100 MB/s of plain text on one core both
  ways from profile 6 down (parallel segments are a bonus, not a requirement). Each profile
  has its own rule catalog, fitted with a speed penalty λ (bits per KB of plain text for
  every ns/B a rule takes, by `Rule::ns_per_byte`, measured with `aqr-flate-train speed`).
- **10: adaptive coding**, built for size: corrections are binary arithmetic coded under
  adaptive contexts, with a lookahead, and predictors beyond the catalog's rule (exact
  chains and zlib emulation).

Held-out split, 924 streams (45.2 MB of DEFLATE, 85.1 MB of plain text), single-threaded on
an M4. All round-trip.

| profile | λ | record | split | join |
|---|---|---|---|---|
| 0 | 200 | 1,560,843 B (3.450%) | 0.68 s (125 MB/s) | 0.45 s (189 MB/s) |
| 1 | 100 | 1,305,100 B (2.884%) | 0.70 s (122 MB/s) | 0.46 s (185 MB/s) |
| 2 | 50 | 1,110,953 B (2.455%) | 0.73 s (117 MB/s) | 0.48 s (177 MB/s) |
| 3 | 30 | 848,192 B (1.875%) | 0.76 s (112 MB/s) | 0.52 s (164 MB/s) |
| 4 | 20 | 776,419 B (1.716%) | 0.81 s (105 MB/s) | 0.57 s (149 MB/s) |
| 5 | 10 | 676,811 B (1.496%) | 0.91 s (94 MB/s) | 0.65 s (131 MB/s) |
| **6 (default)** | **5** | **650,173 B (1.437%)** | **0.94 s (91 MB/s)** | **0.68 s (125 MB/s)** |
| 7 | 2 | 615,883 B (1.361%) | 0.96 s (89 MB/s) | 0.70 s (122 MB/s) |
| 8 | 1 | 613,582 B (1.356%) | 0.97 s (88 MB/s) | 0.71 s (120 MB/s) |
| 9 | 0 | 602,111 B (1.331%) | 1.00 s (85 MB/s) | 0.74 s (115 MB/s) |
| **10 (adaptive)** | | **289,911 B (0.641%)** | **6.23 s (13.7 MB/s)** | **1.57 s (54 MB/s)** |

```
RAYON_NUM_THREADS=1 cargo run --release --features train --bin aqr-flate-train -- eval -P 6
```

- Split time at λ = 5 breaks down to parsing 0.14 s, rule choice 0.03 s, token coding
  0.70 s (with the second parse, by segment) and headers 0.07 s. Only token coding depends on
  λ. Joining spends 0.08 s on headers and 0.05 s on writing.
- Speed costs little size below λ = 10: dropping the penalty altogether (λ = 0) gains only
  0.1 points over λ = 5. A higher λ first drops the 8-way tables, then ranked distances and
  the second table. λ = 200 is hardly faster than λ = 100.
- With static codes, gaps and kinds (where the misses are and what they are) make 70% of the
  corrections at λ = 5. Lengths and distances make the rest.
- Profile 10 is less than half the size of profile 9, joins at half its speed and splits at
  a sixth. Splitting codes every stream two or three times (with the catalog's rule, a chain
  rule, and zlib emulation for streams that look like zlib's).

## How it works

- **Rules.** A `Rule` predicts every token from the data alone, with a small hash table of
  recent positions. The rules are derived empirically from flate-zoo's encoders (the
  oracle). `aqr-flate-train rules` fits the best rule of each preset class by coordinate
  descent, then a greedy set cover picks a catalog of 32 (`model/rules-lλ.txt`). Each
  stream's fingerprint maps to one or two of them; the fingerprint covers the common
  shortest match, farthest distance, far short matches, full-block token count, block types
  and how the first Huffman header was built. Two candidates, or an unknown fingerprint, are
  settled by a trial on a 4 KB prefix.
- **Chain rules** (adaptive profiles). A `chain` rule walks exact chains (`matchfind`), so
  nothing depends on an encoder's hash. Its depth never exceeds the 99.5th percentile of how
  deep the stream's matches lie in their chains (deeper ones are coded by rank anyway, and
  it keeps joining fast). Up to four configurations are compared by cost, on the first
  256 KiB of long streams.
- **Corrections.** Only mispredicted tokens are corrected. A distance is coded by its entry
  in the bucket, else by its rank among earlier positions matching as long (walking a chain at
  the misses only), else verbatim. Static codes: each field uses a trained default model
  (`model/models.txt`) or the stream's own, whichever is smaller. Nothing adapts. Adaptive
  coding (`aqr_flate::adapt`): the contexts include a lookahead, how the match at the next
  position compares. Priors (`model/priors-10.txt`) come from the first 16 uses of each
  context in every segment of the training split, worth 2 observations each.
- **zlib emulation** (adaptive profiles). `zlib` reproduces the output of zlib 1.3.1's four
  parsers (greedy for levels 1-3, lazy for 4-9, runs and Huffman only) bit for bit, as
  FORMAT.md describes them; it is written from that description, not from zlib's code. It is driven by the actual tokens (`predict`, then
  `commit` of the actual token), so after a misprediction it carries on from where the stream
  really is. Each segment resumes it from a checkpoint (`zlib::Checkpoint`, a few counters:
  the lazy levels insert every position, so the rest of the state follows from the data).
  deflate_fast (levels 1-3) cannot be resumed, since its hash chains depend on the tokens, so
  such a stream is one segment.
- **Detection** (`detect`): a cheap check of whether a stream can come from zlib, on the idea
  of preflate's estimator. It checks that every dynamic header is exactly zlib's, that full
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
- **Headers.** Dynamic Huffman headers are coded against the builders and run-length coders of
  zlib, libdeflate and zopfli/ECT, so an exact block costs one bit.
- **Speed.** Tokens are cut into ~1 MiB segments that are coded independently. The parser
  (see [below](#parser-and-writer-flate_reconsinflate-flate_reconswrite)) gathers the statistics everything
  else needs (symbol frequencies, fingerprint, segment cuts) as it decodes. Candidate matches
  are measured on 8 bytes without branching. Exact zlib trees use a heap with packed
  keys. Package-merge is computed as Huffman's two-queue algorithm whenever the limit does not
  bite; tests check that it matches the full package-merge.
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
| aqr-flate, profile 10 | 207,798 B (0.512%) | 0 | 6.2 s | 1.6 s |
| aqr-flate, profile 9 | 434,083 B (1.069%) | 0 | 1.0 s | 0.8 s |
| aqr-flate, profile 6 | 468,358 B (1.153%) | 0 | 1.0 s | 0.7 s |
| preflate | 965,499 B (2.378%) | 15 | 3.8 s | 3.0 s |
| preflate-rs | 316,814 B (0.780%) | 117 | 1.7 s | 0.8 s |

- preflate-rs is smaller on encoders it models exactly or nearly (gzip, miniz-oxide, sdefl,
  zlib-cloudflare, zlib-ng), and refuses 13% of the streams (`NoCompressionCandidates`,
  `InvalidDeflate`). preflate is cheap on its best cases but explodes on others.
- aqr-flate's zlib streams cost 5.3 KB over 95 streams, most of it the block layout: zlib
  at small memLevels writes many stored blocks, each with its length coded up front.

As gzip files through `aqr-show` (one thread, level `mid`; see the
[file formats overview](../docs/file-formats.md#how-it-was-measured), ratios tentative), the four gzip files at
hand (4.2 MB) come to 0.932 (zstd -3: 0.977) at 16 MB/s compressing and 58 MB/s decompressing;
two of them (Unicode data files) are kept as they are.

aqr-compress deconstructs a gzip or zlib stream only where its record and its data may archive
in at least 1/32 less than the stream: the record by `Parsed::estimate`, the data at the fewest
bytes the archive's level may take it in (zstd 1 and 9 on a sample), the files in it that are
recompressed in turn counted as nothing. The two Unicode files above went from 6.7 and 8.0 MB/s
to 50 and 33 MB/s, kept as they were. On 153 gzip files of this machine (280.5 MB, mostly
Homebrew bottles; 20 MB each at most), compressing went from 8.0 to 9.0 MB/s (the 55 kept as
they are: 8.8 to 25.6 MB/s; the 98 coded: 7.9 to 8.4, the stream parsed once rather than three
times), for 8 KB more (0.88120 to 0.88123): three files that gained 1.6-1.7% are kept as they
are, as their estimates (2.9%) fell short of 1/32.

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
