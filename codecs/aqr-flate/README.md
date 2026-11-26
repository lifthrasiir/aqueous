# aqr-flate

Splits a raw DEFLATE stream into its decompressed data and a *record* of everything else the
encoder chose. From the two, the original stream is rebuilt bit for bit. Tokens are predicted
from the data, by a rule (and by zlib emulation where it applies), so the record holds little
more than the mispredictions: a zlib stream costs next to nothing, and the data compresses by
its own format. `FORMAT.md` specifies the record, the modules `flate-set` and `flate-inflate`,
and how it works.

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

`Parsed::estimate` is the record's size without coding it (0.96x to 1.80x of the real one on 10
streams); aqr-compress and aqr-zip use it to rebuild only the streams that may pay
(`aqr_formula::probe`).

The parser is also a library for others (`aqr_flate::inflate::parse_into` hands the data and
tokens to a `Sink`; `aqr_flate::write` writes a stream again bit for bit); `zlib::compress`
writes what zlib 1.3.1 writes in one `deflate(Z_FINISH)` call, for containers known to use fixed
parameters (Clip Studio Paint's tiles). Parsing runs at 400-3,100 MB/s of data and writing at
1,300-12,400 MB/s, one thread of an M4.

## What is taken

- Any DEFLATE stream, however it was made: static codes are predicted by a catalog of 32 rules
  fitted to flate-zoo's encoders, zlib's four parsers (levels 1-9) by emulation (adaptive
  profile), the rest by exact chains. Header builders of zlib, libdeflate and zopfli/ECT are
  matched, so an exact block header costs one bit.
- A raw stream of no container is found by nothing; `aqr` takes one where the attributes say so
  (docs/hints-design.md): `aqr-format=deflate`, with `aqr-offset=N` where it starts within the
  file (aqr-compress's format `deflate`, never detected; what follows the stream as it is).
- Not taken: encoders other than the above are coded by the catalog's closest rule at a larger
  record (below).

## Profiles

- **0-9: static codes**, built for speed: at least 100 MB/s of plain text on one core both ways
  from profile 6 down. Each has its own rule catalog, fitted with a speed penalty λ.
- **10: adaptive coding**, built for size: corrections are binary arithmetic coded under
  adaptive contexts with a lookahead, and predictors beyond the catalog's rule (exact chains and
  zlib emulation).

## Results

Held-out split, 924 streams (45.2 MB of DEFLATE, 85.1 MB of plain text), one thread of an M4.
All round-trip.

| profile | λ | record | split | join |
|---|---|---|---|---|
| 0 | 200 | 1,560,843 B (3.450%) | 0.68 s (125 MB/s) | 0.45 s (189 MB/s) |
| 3 | 30 | 848,192 B (1.875%) | 0.76 s (112 MB/s) | 0.52 s (164 MB/s) |
| **6 (default)** | **5** | **650,173 B (1.437%)** | **0.94 s (91 MB/s)** | **0.68 s (125 MB/s)** |
| 9 | 0 | 602,111 B (1.331%) | 1.00 s (85 MB/s) | 0.74 s (115 MB/s) |
| **10 (adaptive)** | | **289,911 B (0.641%)** | **6.23 s (13.7 MB/s)** | **1.57 s (54 MB/s)** |

Speeds are of the plain text. Profile 10 is less than half the size of profile 9, joins at half
its speed and splits at a sixth. On a Ryzen 7 9700X, one thread, raw streams of zlib -9 (-6):
dickens splits in 0.75 s (0.44), mozilla in 4.5 s (1.6).

Against preflate (C++) and preflate-rs (`bench/`: `bench/fetch.sh` gets them, every output is
verified by a full round trip; `cd bench && RAYON_NUM_THREADS=1 cargo run --release -- [-P
PROFILE]`). Sizes over the 807 streams every tool round-trips (40.6 MB of DEFLATE), times over
all 924, one thread of an M4:

| tool | reconstruction data | streams failed | split | join |
|---|---|---|---|---|
| aqr-flate, profile 10 | 207,798 B (0.512%) | 0 | 6.2 s | 1.6 s |
| aqr-flate, profile 9 | 434,083 B (1.069%) | 0 | 1.0 s | 0.8 s |
| aqr-flate, profile 6 | 468,358 B (1.153%) | 0 | 1.0 s | 0.7 s |
| preflate | 965,499 B (2.378%) | 15 | 3.8 s | 3.0 s |
| preflate-rs | 316,814 B (0.780%) | 117 | 1.7 s | 0.8 s |

preflate-rs is smaller on encoders it models exactly or nearly (gzip, miniz-oxide, sdefl,
zlib-cloudflare, zlib-ng) and refuses 13% of the streams.

As gzip files through `aqr-show` (one thread, level `mid`; see the
[file formats overview](../../docs/file-formats.md#how-it-was-measured)), four gzip files
(4.2 MB) come to 0.932 (zstd -3: 0.977) at 16 MB/s compressing and 58 MB/s decompressing; two
of them (Unicode data files) are kept as they are. aqr-compress deconstructs a gzip or zlib
stream only where its record and its data may archive in at least 1/32 less than the stream
(`Parsed::estimate`): on 153 gzip files of one machine (280.5 MB, mostly Homebrew bottles),
compressing runs at 9.0 MB/s for 0.88123 of the input.

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
`cache/samples` (~3 minutes). Every fifth input file is held out. `rules` takes about 2.5 minutes
per λ, `priors` about 15 seconds. The catalogs, default models and priors are compiled in.
The table above is `RAYON_NUM_THREADS=1 ... aqr-flate-train -- eval -P N`; `aqr-flate-train
parse [--png FILE...]` measures the parser on the held-out samples or PNG files' IDAT streams.

## Tests

- `tests/recons.rs`: round trips over every flate-zoo encoder in every kind of profile; segments,
  and zlib emulation resumed (or not, where it cannot be) at a segment.
- `tests/zlib.rs`: zlib's emulation, detection, `zlib::compress` and `MultiSlow` against real
  zlib output.
- `tests/malformed.rs`: malformed DEFLATE through every entry point: all fail alike, none panics.
- `tests/part.rs`, `tests/pulled.rs`: `flate-set` as a part module gives what the whole decode
  does; the data read a piece at a time rebuilds what `reconstruct` does.
- `tests/alloc.rs`: counts read from untrusted data are not reserved before they are found wrong.

## Left

- **Block layout of zlib streams.** The layout (stored lengths, fixed versus dynamic, block
  sizes) is coded up front, not predicted from the emulation: 5.3 KB over 95 zlib streams of the
  held-out split, most of it zlib's many stored blocks at small memLevels.
- **More emulated encoders.** zlib-ng (`deflate_medium`, its own hash), chromium/cloudflare (CRC
  hashes), libdeflate, miniz and stb are coded by the catalog's rule or exact chains at a larger
  record (preflate-rs is smaller on gzip, miniz-oxide, sdefl, zlib-cloudflare and zlib-ng); no
  libdeflate emulation also means BGZF is unmeasured.
- **Cheaper splitting in profile 10.** The chain configurations and zlib detection are tried in
  full unless hinted parameters are exact (splitting is 6x slower than profile 9).
- **More Huffman builders.** 7-zip's and miniz's as header candidates.
- **A larger corpus.** A more varied training corpus, and the `full` sweep.
