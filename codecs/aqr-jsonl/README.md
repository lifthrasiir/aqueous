# aqr-jsonl

JSON Lines rebuilt bit for bit from their values, stored by key path: the values of a path
(`.actor.login`, `.payload.commits[]`) one after the other, strings as they are written (escapes
and all) and numbers as integers in the way they were printed
([`aqr-num`](../../crates/aqr-num)), as they are or as differences from the path's previous one.
The structure is apart: of each object a key list, kept by each path most recent first (one
varint where it is the last one), with the paths of its members; of each array its length. Lines
that are not JSON values are kept as they are, so any file round-trips. The lines of a JSON array
(`{...},` each) are taken too.

```rust
use aqr_jsonl::formula::{self, MODULE};
// None where the file is not JSON lines this crate takes.
let f = formula::encode(&jsonl, 1 /* threads */).unwrap();
// `f` rebuilds `jsonl` when evaluated with `formula::decode` as the module `MODULE`.
```

- **Paths.** A path's objects whose keys are data (ids, hashes, words: many keys, each seen a
  few times) are a *map*: their members are one child, their keys stored as data. A path's arrays
  of one length (coordinates, pairs) are *tuples*: each index a child of its own.
- **Separators.** The file's style (`,` or `, `, `:` or `: `) is in the params; a separator not of
  the style, and a space within braces or brackets (`{ "a": 1 }`), is a bit of the shape, so
  files written by string concatenation are taken too. Other whitespace keeps the line as it is.

aqr-compress takes a file as JSON lines by its text (`formula::starts`: its first lines objects
or arrays, nine in ten times), before aqr-xsv and aqr-wrt, and where its formula is smaller than
the file otherwise is; a file named `.jsonl`, `.ndjson` or `.geojsonl` is tried as text too. The
streams of strings (`text`, `keys`, `names`, and the lines kept) are text, which aqr-wrt takes in
turn. Without the default feature `encode`, the crate is the module's code alone (`module`),
which aqr-modules builds into the Wasm module `jsonl-recons`.

The format is specified in [FORMAT.md](FORMAT.md).

## Results

Measured on 2026-11-01 on an Apple M4 (`cache/results-2026-11-01.txt`). Through `aqr` at level
mid (10 threads), against `aqr` before this crate (aqr-wrt on the whole file, or aqr-xsv where it
took the file):

| file | MB | before | after | | compress (instr.) | decompress |
|---|---|---|---|---|---|---|
| GH Archive, `2024-06-03-14` (GitHub events) | 1,123.1 | 71,668,221 | 65,678,897 | −8.4% | +10% | 1,929 → 919 MB/s |
| Amazon Reviews 2023, `All_Beauty` | 326.6 | 70,059,143 | 57,140,447 | −18.4% | +7% | 1,263 → 869 MB/s |
| Amazon Reviews 2023, `meta_All_Beauty` | 213.0 | 29,103,795 | 25,445,129 | −12.6% | +6% | 1,393 → 832 MB/s |
| OpenAlex works, part of 2026-08-21 | 208.0 | 17,013,357 | 11,873,469 | −30.2% | +27% | 1,660 → 778 MB/s |
| Microsoft building footprints (GeoJSON) | 249.1 | 69,025,939 | 58,520,247 | −15.2% | −30% | 1,552 → 524 MB/s |
| OTRF Security-Datasets (Windows events) | 49.5 | 814,796 | 508,348 | −37.6% | −22% | 3,723 → 1,257 MB/s |
| C4, `en` validation shard 0 (text) | 103.7 | 29,155,269 | 28,651,511 | −1.7% | +26% | 1,008 → 767 MB/s |
| all | 2,273.1 | 286,840,520 | 247,818,048 | −13.6% | +4.5% | |

Peak footprint compressing goes up by up to a fifth (the GH Archive file 2.13 → 2.57 GB); the
module decodes a file on one thread, hence the slower decompression of a file alone.

`aqr-jsonl-bench` (one thread; the streams through zstd -3 against the file through zstd -3):

| file | ratio | encode | decode |
|---|---|---|---|
| GH Archive | 0.888 | 473 MB/s | 1,497 MB/s |
| `All_Beauty` | 0.833 | 408 MB/s | 1,652 MB/s |
| `meta_All_Beauty` | 0.821 | 277 MB/s | 1,321 MB/s |
| OpenAlex works | 0.702 | 236 MB/s | 1,111 MB/s |
| building footprints | 0.799 | 177 MB/s | 725 MB/s |
| Windows events | 0.474 | 289 MB/s | 1,572 MB/s |
| C4 | 0.984 | 1,045 MB/s | 3,171 MB/s |

- Strings by path, or in the order of the file: GitHub's events repeat their ids and URLs across
  fields, which strings by path take apart (the GH Archive file 1.051 of zstd -3, against 0.876
  with all strings in order); reviews and metadata are the other way (0.833 against 0.888). The
  choice by path (`shared_paths`) gets 0.888 and 0.833.
- The strings are text that aqr-wrt takes in turn: they end at LF, not a zero byte (C4 34.44 →
  28.65 MB through `aqr`, as zero bytes kept aqr-wrt off).

## Corpus

`corpus/` is not in the repository. The files above (4.7 GB in all, with a second GH Archive hour,
two more OpenAlex parts and two more event logs) are at `/Volumes/Data/aqr-jsonl-corpus`, whose
`README.txt` lists each file's source and terms: [GH Archive](https://www.gharchive.org/), [Amazon
Reviews 2023](https://amazon-reviews-2023.github.io/) (for research only), [allenai/c4](https://huggingface.co/datasets/allenai/c4),
the [OpenAlex](https://openalex.org/) snapshot, [OTRF Security-Datasets](https://github.com/OTRF/Security-Datasets)
and Microsoft's [Global ML Building Footprints](https://github.com/microsoft/GlobalMLBuildingFootprints).

## Testing

```sh
cargo rt -p aqr-jsonl
aqr-jsonl-bench FILE...   # streams through zstd -3, speeds, round trip
```

Unit tests: lines (values, separators, depth, commas after values), round trips of edge cases,
styles, maps and tuples, key lists dropped and defined again, numbers, and damaged streams
(errors, never panics).

## Layout

| File | Contents |
|---|---|
| `src/formula.rs` | The module: the formula of a file (the two passes, paths, key lists, numbers) |
| `src/parse.rs` | Lines, their tokens and separators, the style from a sample (encoder) |
| `src/tree.rs` | Params, stream codes, the file rebuilt (the module's code) |
| `src/module.rs` | The module's entry |
| `examples/bench.rs` | Sizes of the streams, speeds, round trip |

## Next steps

- Decoding on more threads (the lines cut into segments, each path's parts in segments too).
- Numbers within strings (dates, times, ids written as strings), as aqr-xsv's next steps have it.
- The binary floats of coordinates (GeoJSON's shortest doubles change descriptor often: `desc` and
  `types` are a fifth of the streams there).
- Pretty-printed JSON (a value over several lines), and a JSON array on one line (`[{...},{...}]`).
