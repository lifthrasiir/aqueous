# aqr-xml

XML of records (dumps of wikis, Q&A sites, maps, bibliographies) rebuilt bit for bit from a
skeleton and its values in containers by context, after XMill, so that each element's text and
each attribute's value compress with their own kind (aqr-log or aqr-xsv for numbers and times,
aqr-wrt for words). `FORMAT.md` specifies the module `xml-recons`, and how it works
(`docs/xml-design.md` has the design). Whatever is not XML round-trips too, kept in the
skeleton.

```rust
use aqr_xml::formula::{self, MODULE};
// None where the text is shorter than MIN_LEN or nothing of it is cut out.
let f = formula::encode(&xml, 1 /* threads */).unwrap();
// `f` rebuilds `xml` when evaluated with `formula::decode` as the module `MODULE`.
```

Without the default feature `encode`, the crate is the module's code alone (`module`), which
aqr-modules builds into the Wasm module `xml-recons` (4.2 KB as a block, 10.5 KB as a module).

## What is taken

An XML document (not HTML), text of 64 KiB or more, where a sample of it coded and compressed by
zstd -3 container by container is no larger than the sample compressed (`xml:percent` 100%) and,
where aqr-log or aqr-xsv would take the file, at most 135% of theirs (`xml:log-percent`); a file
under 1 MiB is taken at 92% (`xml:small-percent`). Not taken: rows of attributes one to a line
(OSM's changes, Stack Exchange's `Badges` and `Votes`), which aqr-log codes smaller, HTML, files
under 64 KiB. On the 47 files of the ground truth it takes or not as the better of the two for all
but `silesia/play1.xml` (+0.65%). Files whose formula is not smaller than the file are given up
by `recompress_as` and cost only the sample. A container's text shares one aqr-wrt dictionary
with the others of the file (JMdict -3.8%, Posts -5.5%, PubMed -6.8%, DBLP -4.1%, SwissProt
-1.6% against a dictionary for each).

## Results

Through `aqr` at its default level (10 threads), on an Apple M4 (2026-11-26; raw numbers in
`cache/final.tsv`): archive bytes without aqr-xml (`-Z format:no-xml`) and with it, instructions
retired as a ratio of with over without, and peak memory of compressing and decompressing
(`-K`; without aqr-xml in brackets). The `+0.0%` files are not taken.

| file | MB | without | with | | compress instr. | compress peak MB | decompress instr. | decompress peak MB |
|---|---|---|---|---|---|---|---|---|
| `enwik8` | 100.0 | 26,838,455 | 26,671,615 | -0.6% | 1.10 | 257 (210) | 1.00 | 225 (161) |
| `silesia/xml` | 5.3 | 508,962 | 486,382 | -4.4% | 1.39 | 85 (52) | 0.90 | 10 (10) |
| `dblp/dblp-head.xml` | 268.4 | 39,742,159 | 31,488,429 | -20.8% | 1.58 | 577 (859) | 1.51 | 523 (605) |
| `jmdict/JMdict_e` | 63.3 | 7,739,721 | 6,255,083 | -19.2% | 1.10 | 164 (209) | 1.29 | 116 (120) |
| `osm-hours/503.osc` | 71.7 | 2,537,139 | 2,537,139 | +0.0% | 1.00 | 151 (152) | 0.96 | 90 (90) |
| `osm-hours/504.osc` | 163.9 | 3,874,399 | 3,874,399 | +0.0% | 1.00 | 289 (288) | 0.97 | 179 (179) |
| `pubmed/pubmed26n0001.xml` | 195.5 | 11,828,651 | 9,040,295 | -23.6% | 1.19 | 444 (450) | 1.32 | 360 (362) |
| `se-coffee/Badges.xml` | 1.6 | 127,042 | 127,042 | +0.0% | 0.99 | 28 (29) | 0.87 | 5 (5) |
| `se-coffee/Comments.xml` | 1.6 | 422,144 | 393,480 | -6.8% | 1.30 | 54 (42) | 0.88 | 7 (5) |
| `se-coffee/PostHistory.xml` | 9.5 | 1,603,616 | 1,449,722 | -9.6% | 1.40 | 82 (47) | 0.94 | 23 (18) |
| `se-coffee/PostLinks.xml` | 0.1 | 9,451 | 9,451 | +0.0% | 0.88 | 6 (6) | 0.86 | 3 (3) |
| `se-coffee/Posts.xml` | 5.7 | 1,219,104 | 1,165,722 | -4.4% | 1.21 | 52 (47) | 0.88 | 16 (12) |
| `se-coffee/Tags.xml` | 0.0 | 1,953 | 1,953 | +0.0% | 0.86 | 3 (3) | 0.86 | 3 (3) |
| `se-coffee/Users.xml` | 4.5 | 997,836 | 819,952 | -17.8% | 1.06 | 65 (78) | 0.82 | 14 (31) |
| `se-coffee/Votes.xml` | 2.0 | 102,552 | 102,552 | +0.0% | 0.99 | 32 (31) | 0.87 | 6 (6) |
| `se-history/Badges.xml` | 10.9 | 667,274 | 667,274 | +0.0% | 1.00 | 53 (53) | 0.91 | 16 (16) |
| `se-history/Comments.xml` | 47.8 | 11,539,091 | 9,938,457 | -13.9% | 1.40 | 129 (108) | 1.34 | 104 (78) |
| `se-history/PostHistory.xml` | 301.7 | 34,175,279 | 30,363,013 | -11.2% | 1.37 | 567 (460) | 1.30 | 672 (496) |
| `se-history/PostLinks.xml` | 0.3 | 47,394 | 44,242 | -6.7% | 1.01 | 23 (17) | 0.87 | 4 (3) |
| `se-history/Posts.xml` | 104.2 | 22,855,471 | 22,002,387 | -3.7% | 1.32 | 211 (203) | 1.22 | 229 (134) |
| `se-history/Tags.xml` | 0.1 | 12,717 | 12,717 | +0.0% | 0.88 | 6 (6) | 0.86 | 3 (3) |
| `se-history/Users.xml` | 16.8 | 3,190,329 | 2,600,723 | -18.5% | 1.30 | 90 (118) | 1.24 | 39 (32) |
| `se-history/Votes.xml` | 42.0 | 1,154,924 | 1,154,924 | +0.0% | 1.00 | 129 (128) | 0.95 | 50 (50) |
| `uw/lineitem.xml` | 32.3 | 1,854,258 | 1,187,802 | -35.9% | 1.31 | 103 (155) | 1.43 | 60 (43) |
| `uw/nasa.xml` | 25.1 | 2,475,397 | 2,426,065 | -2.0% | 1.55 | 118 (53) | 1.01 | 53 (41) |
| `uw/SwissProt.xml` | 114.8 | 7,588,475 | 6,708,637 | -11.6% | 1.97 | 291 (304) | 1.44 | 224 (178) |
| `uw/treebank_e.xml` | 86.1 | 28,075,293 | 26,537,341 | -5.5% | 0.89 | 246 (269) | 1.17 | 155 (144) |
| all 28 | 1,675.1 | 211,189,086 | 188,066,798 | -10.9% | 1.29 | | | |

- Decompressing with `-Z wasm` (WAH's JIT, and its interpreter with `-Z wasm:no-jit`) gives
  JMdict and SwissProt back byte for byte.
- Compressing takes 1.29 times the instructions in all (DBLP 1.58, SwissProt 1.97): the
  containers are recompressed in turn.
- The module alone, on one thread (`aqr-xml-bench`): encode 848 MB/s, decode 990 MB/s over the 27
  files; 500-750 on files of many tags, 3000 on text.
- `enwik9`: 233,040,175 -> 230,326,223 B (-1.16%), 362 -> 419 G instructions (+16%), peak 1.28 GB
  (1.35 without); `scalc.jar` -3.5%, `main.xcd` -2.8%.

## Tests

```sh
cargo rt -p aqr-xml
```

- `src/formula/tests.rs`: the encoder and the round trips.
- aqr-decompress's `xml_plans_hold` (the memory plan to the page) and `xml_alike`.
- `aqr-xml-bench FILE...` (`cargo build --profile release-test -p aqr-xml --example
  aqr-xml-bench`): per file the skeleton, `table` and `lens` through zstd (`LEVEL`, 3) and each
  container through it on its own, against the file through it, the speeds, and that it decodes
  back; `TOP=1` also tells the 20 largest containers.

## Corpus

`fetch-corpus.sh` downloads samples into `corpus/` (gitignored), each with its licence:

| sample | what | licence |
|---|---|---|
| `se-coffee`, `se-history` | Stack Exchange's dumps of two sites (rows as attributes) | CC BY-SA 4.0 |
| `osm-hours` | two hours of OpenStreetMap's changes (OsmChange) | ODbL 1.0 |
| `pubmed` | the first file of PubMed's baseline | NLM's terms |
| `jmdict` | JMdict, English glosses (a dictionary of elements) | CC BY-SA 4.0 |
| `dblp` | the first 256 MiB of DBLP's XML, cut at a line | CC0 1.0 |
| `uw` | treebank, SwissProt, lineitem and nasa of the University of Washington's XML Data Repository | for research |

`enwik8`, `enwik9` and silesia's `xml` are read where they are (the 10gb corpus).

## Layout

- `src/module.rs`: the module (`decode`, and `plan`, its memory plan).
- `src/formula.rs`: the encoder (`encode`, `encode_with` with its `Limits`, `encode_report`), the
  module as an aqr-formula `Module`, and `starts`.
- `examples/bench.rs`: `aqr-xml-bench`.
- The module is registered in aqr-modules (`src/bin/xml-recons.rs`), aqr-native, aqr-decompress
  (`XML`) and aqr-dll.

## Left

- **Choice's proxy.** zstd -3 of the sample's pieces stands for coders that do better on numbers,
  so it is checked against the ground truth; a sample coded by the real coders would be exact and
  cost more.
- **Segment size.** Segments are 48 MiB (64 MiB needed 269 MB against the application bound of
  256 MiB): the module holds its inputs, nearly as large as its output.
- **Skeleton.** Its structure could use attribute lists called by number, as aqr-jsonl's key
  lists.
- **Contexts.** Other depths (three levels, the element alone), to measure on the corpus.
- **CDATA and entities.** Sections and comments are kept in the skeleton as they are; text with
  entities is a value as written (no decoding of `&amp;`).
- **HTML and the like.** An unclosed element (`<br>`) leaves a name open, whose end tag then pops
  the wrong one: the text round-trips, but its contexts drift.
- **Numbers in the module.** Containers of numbers coded in the module (`docs/xml-design.md`,
  1.1), if aqr-log's coding of the columns proves slow or large enough to show.
- **Containers freed early.** Containers freed as soon as their contents are made (the spill:
  enwik9 peaks at 1.45 GB there), and one decision of a context's format for all of its
  segments.
