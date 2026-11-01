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

Measured on 2026-11-01 on an Apple M4 (`cache/results-2026-11-01.txt`,
`cache/results-segments-2026-11-01.txt`). Through `aqr` at level mid (10 threads), against `aqr`
before this crate (aqr-wrt on the whole file, or aqr-xsv where it took the file); the peak
footprints (`/usr/bin/time -l`) against this crate when it coded a file as one segment:

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

Decompressing, the GH Archive file runs at 775 MB/s (919 as one segment, 1,929 before this
crate): the segments are decoded in turn, each on one thread.

### Memory

A file is coded in segments of 64 MiB of lines ([`formula::SEGMENT`]), each an application of its
own, so that the module holds a segment at a time: decompressing two hours of GH Archive (2.24 GB)
peaks at 376 MB of heap (one hour, 311 MB; as one segment, one hour took 1.97 GB). The encoder
holds a segment's streams and what the first pass keeps, of bounds of its own (the key lists of a
path, the sample of strings), and spills the streams it makes. aqr-compress evaluates a
concatenation part by part to check it (`Formula::eval_parts`), the applications run ahead of it
a window at a time.

Against coding a file as one segment, the archives are 0.9% larger: segments lose the reach of a
path's values across the file (`meta_All_Beauty` +5.5%, OpenAlex +3.5%), and GH Archive's events
gain as much (−0.03%). The segments' text is coded with one aqr-wrt dictionary of all of it
(aqr-compress's `segment_words`): with a dictionary of each segment's own, GH Archive was 70.31 MB
rather than 65.66. Counting that dictionary is most of the instructions compressing takes more.

Compressing still holds more as the file grows (two hours of GH Archive: 1.16 GB of heap, against
591 MB): aqr-compress makes all the contents of a formula (the segments' text, coded by aqr-wrt)
before it puts them in place, and the archive's zstd blocks are held until written
(`docs/streaming-encoder-design.md`, steps 3 and 4).

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

- Strings by path, or in the order of the file: GitHub's events repeat their ids and URLs across
  fields, which strings by path take apart (the GH Archive file 1.051 of zstd -3, against 0.876
  with all strings in order); reviews and metadata are the other way (0.833 against 0.888). The
  choice by path (`shared_paths`) gets 0.888 and 0.833 (as one segment).
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

- Decoding the segments on more threads (aqr-formula's evaluation of a concatenation is in turn).
- Numbers within strings (dates, times, ids written as strings), as aqr-xsv's next steps have it.
- The binary floats of coordinates (GeoJSON's shortest doubles change descriptor often: `desc` and
  `types` are a fifth of the streams there).
- Pretty-printed JSON (a value over several lines), and a JSON array on one line (`[{...},{...}]`).
