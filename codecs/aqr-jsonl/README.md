# aqr-jsonl

JSON Lines rebuilt bit for bit from their values, stored by key path (`.actor.login`,
`.payload.commits[]`): strings as they are written, for aqr-wrt, and numbers as integers in the
way they were printed ([`aqr-num`](../../crates/aqr-num)); the structure apart. `FORMAT.md`
specifies the module `jsonl-recons`, and how it works.

```rust
use aqr_jsonl::formula::{self, MODULE};
// None where the file is not JSON lines this crate takes.
let f = formula::encode(&jsonl, 1 /* threads */).unwrap();
// `f` rebuilds `jsonl` when evaluated with `formula::decode` as the module `MODULE`.
```

## What is taken

Files of at least 16 KiB whose first lines are objects or arrays (nine in ten), of the extensions
`.jsonl`, `.ndjson`, `.geojsonl` or any text; the lines of a JSON array (`{...},` each) too. Lines
that are not JSON values are kept as they are, so any file round-trips. Not taken: pretty-printed
JSON, a JSON array on one line.

## Results

Measured on 2026-11-01 on an Apple M4 (`cache/results-segments-2026-11-01.txt`). Through `aqr` at
level mid (10 threads), against `aqr` without this crate (aqr-wrt on the whole file, or aqr-xsv
where it took the file); peaks against one segment per file:

| file | MB | before | after | | compress (instr.) | compress, peak | decompress, peak |
|---|---|---|---|---|---|---|---|
| GH Archive, `2024-06-03-14` (GitHub events) | 1,123.1 | 71,668,221 | 65,659,049 | −8.4% | +34% | 2,569 → 789 MB | 2,131 → 505 MB |
| Amazon Reviews 2023, `All_Beauty` | 326.6 | 70,059,143 | 57,387,199 | −18.1% | +36% | 1,097 → 591 MB | 658 → 357 MB |
| Amazon Reviews 2023, `meta_All_Beauty` | 213.0 | 29,103,795 | 26,838,409 | −7.8% | +10% | 903 → 490 MB | 533 → 182 MB |
| OpenAlex works, part of 2026-08-21 | 208.0 | 17,013,357 | 12,282,271 | −27.8% | +38% | 816 → 631 MB | 405 → 258 MB |
| Microsoft building footprints (GeoJSON) | 249.1 | 69,025,939 | 58,608,001 | −15.1% | −13% | 840 → 638 MB | 455 → 266 MB |
| OTRF Security-Datasets (Windows events) | 49.5 | 814,796 | 508,348 | −37.6% | −25% | 195 → 193 MB | 116 → 116 MB |
| C4, `en` validation shard 0 (text) | 103.7 | 29,155,269 | 28,703,853 | −1.5% | +61% | 487 → 473 MB | 253 → 223 MB |
| all | 2,273.1 | 286,840,520 | 249,987,130 | −12.85% | +25% | | |

Decompressing, the GH Archive file runs at 775 MB/s (1,929 without this crate): the segments
are decoded in turn, each on one thread. Files are coded in segments of 64 MiB of lines, so that
decompressing two hours of GH Archive (2.24 GB) peaks at 376 MB of heap and compressing at 536 MB,
whatever the file's length. The archives are 0.9% larger than one segment per file would give
(`meta_All_Beauty` +5.5%, OpenAlex +3.5%, GH Archive -0.03%).

`aqr-jsonl-bench` (one thread; the streams of each class one after another through zstd -3,
against the file through zstd -3):

| file | segments | ratio | encode | decode |
|---|---|---|---|---|
| GH Archive | 17 | 0.853 | 490 MB/s | 1,598 MB/s |
| `All_Beauty` | 5 | 0.797 | 425 MB/s | 1,709 MB/s |
| `meta_All_Beauty` | 4 | 0.830 | 323 MB/s | 1,336 MB/s |
| OpenAlex works | 4 | 0.675 | 259 MB/s | 1,166 MB/s |
| building footprints | 4 | 0.799 | 179 MB/s | 741 MB/s |
| Windows events | 1 | 0.474 | 318 MB/s | 1,686 MB/s |
| C4 | 2 | 0.985 | 1,012 MB/s | 3,080 MB/s |

## Corpus

`corpus/` is not in the repository. The files above (4.7 GB in all, with a second GH Archive hour,
two more OpenAlex parts and two more event logs) are at `/Volumes/Data/aqr-jsonl-corpus`, whose
`README.txt` lists each file's source and terms: [GH Archive](https://www.gharchive.org/), [Amazon
Reviews 2023](https://amazon-reviews-2023.github.io/) (for research only), [allenai/c4](https://huggingface.co/datasets/allenai/c4),
the [OpenAlex](https://openalex.org/) snapshot, [OTRF Security-Datasets](https://github.com/OTRF/Security-Datasets)
and Microsoft's [Global ML Building Footprints](https://github.com/microsoft/GlobalMLBuildingFootprints).

## Tests

```sh
cargo rt -p aqr-jsonl
aqr-jsonl-bench FILE...   # streams through zstd -3, speeds, round trip
```

- Unit tests: lines (values, separators, depth, commas after values), round trips of edge
  cases, styles, maps and tuples, key lists dropped and defined again, numbers, and damaged
  streams (errors, never panics).

## Layout

| File | Contents |
|---|---|
| `src/formula.rs` | The module: the formula of a file (the two passes, paths, key lists, numbers) |
| `src/parse.rs` | Lines, their tokens and separators, the style from a sample (encoder) |
| `src/tree.rs` | Params, stream codes, the file rebuilt (the module's code) |
| `src/module.rs` | The module's entry |
| `examples/bench.rs` | Sizes of the streams, speeds, round trip |

## Left

- **Decoding speed.** The segments are decoded in turn on one thread (775 MB/s on GH Archive);
  aqr-formula's evaluation of a concatenation would have to run them in parallel.
- **Numbers within strings.** Dates, times and ids written as strings stay text, as aqr-xsv's
  Left has it.
- **Coordinates.** The binary floats of GeoJSON's shortest doubles change descriptor often:
  `desc` and `types` are a fifth of the streams there.
- **Other layouts.** Pretty-printed JSON (a value over several lines), and a JSON array on one
  line (`[{...},{...}]`).
