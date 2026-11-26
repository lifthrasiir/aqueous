# aqr-xml

XML of records (dumps of wikis, Q&A sites, maps, bibliographies) rebuilt bit for bit from a
skeleton and its values in containers by context, after XMill: the tags and the whitespace
between them in the skeleton, each element's text and each attribute's value in the container
of its context (an element's text by its parent and name, an attribute's by its element and
name). The module `xml-recons` only takes the text apart and puts it back; it codes no numbers.
Each container is text (`Kind::Text`, a content), which aqr-compress is to recompress in turn
(aqr-log or aqr-xsv for a column of numbers or times, aqr-wrt for words). Whatever is not XML
round-trips too, kept in the skeleton. The format is specified in [FORMAT.md](FORMAT.md), the
design is in `docs/xml-design.md`. aqr-compress takes XML with it where it pays (below).

```rust
use aqr_xml::formula::{self, MODULE};
// None where the text is shorter than MIN_LEN or nothing of it is cut out.
let f = formula::encode(&xml, 1 /* threads */).unwrap();
// `f` rebuilds `xml` when evaluated with `formula::decode` as the module `MODULE`.
```

Without the default feature `encode`, the crate is the module's code alone (`module`), which
aqr-modules builds into the Wasm module `xml-recons` (4.2 KB as a block, 10.5 KB as a module).

## Layout

- `src/module.rs`: the module (`decode`, and `plan`, its memory plan: the stack of names, the
  containers, the open-addressing table of contexts and the text, exact to the page).
- `src/formula.rs`: the encoder (`encode`, `encode_with` with its `Limits`, `encode_report` for
  the contexts of the containers), the module as an aqr-formula `Module`, and `starts`; its tests
  are `src/formula/tests.rs`.
- `examples/bench.rs`: `aqr-xml-bench FILE...`: per file the skeleton, `table` and `lens`
  through zstd (`LEVEL`, 3) and each container through it on its own, against the file through
  it, the encoding and decoding speeds (one thread), and that it decodes back; `TOP=1` also tells
  the 20 largest containers (`cargo build --profile release-test -p aqr-xml --example
  aqr-xml-bench`).
- The module is registered in aqr-modules (`src/bin/xml-recons.rs`), aqr-native, aqr-decompress
  (`XML`; its tests `xml_plans_hold` and `xml_alike`) and aqr-dll.

## Results

Through `aqr` at its default level (10 threads), on an Apple M4 (2026-11-26; raw numbers in
`cache/final.tsv`, the choice's ground truth in `cache/gt2.tsv` and `cache/small.tsv`): archive
bytes without aqr-xml (`-Z format:no-xml`) and with it, instructions retired (`/usr/bin/time -l`)
as a ratio of with over without, and peak memory of compressing and decompressing (`-K`, which
decodes and checks; without aqr-xml in brackets). The files marked `+0.0%` are not taken by
aqr-xml: rows of attributes one to a line (OSM's changes, Stack Exchange's `Badges` and `Votes`),
which aqr-log codes smaller, and files under 64 KiB.

| file | MB | without | with | | compress instr. | compress peak MB | decompress instr. | decompress peak MB |
|---|---|---|---|---|---|---|---|---|
| `enwik8` | 100.0 | 26,838,455 | 26,677,235 | -0.6% | 1.64 | 337 (210) | 1.03 | 224 (161) |
| `silesia/xml` | 5.3 | 508,962 | 484,684 | -4.8% | 1.45 | 84 (52) | 1.03 | 11 (10) |
| `dblp/dblp-head.xml` | 268.4 | 39,742,159 | 31,588,741 | -20.5% | 2.31 | 710 (859) | 1.47 | 568 (605) |
| `jmdict/JMdict_e` | 63.3 | 7,739,721 | 6,239,991 | -19.4% | 1.50 | 244 (209) | 1.35 | 115 (120) |
| `osm-hours/503.osc` | 71.7 | 2,537,139 | 2,537,139 | +0.0% | 1.00 | 152 (152) | 1.00 | 90 (90) |
| `osm-hours/504.osc` | 163.9 | 3,874,399 | 3,874,399 | +0.0% | 1.00 | 289 (288) | 1.00 | 179 (179) |
| `pubmed/pubmed26n0001.xml` | 195.5 | 11,828,651 | 9,135,487 | -22.8% | 1.79 | 664 (450) | 1.36 | 362 (362) |
| `se-coffee/Badges.xml` | 1.6 | 127,042 | 127,042 | +0.0% | 1.03 | 29 (29) | 1.00 | 5 (5) |
| `se-coffee/Comments.xml` | 1.6 | 422,144 | 393,538 | -6.8% | 1.71 | 55 (42) | 1.02 | 7 (5) |
| `se-coffee/PostHistory.xml` | 9.5 | 1,603,616 | 1,449,630 | -9.6% | 1.95 | 89 (47) | 1.06 | 23 (18) |
| `se-coffee/PostLinks.xml` | 0.1 | 9,451 | 9,451 | +0.0% | 1.00 | 6 (6) | 1.00 | 3 (3) |
| `se-coffee/Posts.xml` | 5.7 | 1,219,104 | 1,165,724 | -4.4% | 1.75 | 52 (47) | 1.00 | 15 (12) |
| `se-coffee/Tags.xml` | 0.0 | 1,953 | 1,953 | +0.0% | 1.00 | 3 (3) | 1.00 | 3 (3) |
| `se-coffee/Users.xml` | 4.5 | 997,836 | 819,952 | -17.8% | 1.40 | 70 (78) | 0.93 | 14 (31) |
| `se-coffee/Votes.xml` | 2.0 | 102,552 | 102,552 | +0.0% | 1.03 | 30 (31) | 1.00 | 6 (6) |
| `se-history/Badges.xml` | 10.9 | 667,274 | 667,274 | +0.0% | 1.02 | 52 (53) | 1.00 | 16 (16) |
| `se-history/Comments.xml` | 47.8 | 11,539,091 | 9,942,123 | -13.8% | 2.25 | 190 (108) | 1.41 | 94 (78) |
| `se-history/PostHistory.xml` | 301.7 | 34,175,279 | 30,710,023 | -10.1% | 2.02 | 557 (460) | 1.30 | 712 (496) |
| `se-history/PostLinks.xml` | 0.3 | 47,394 | 45,178 | -4.7% | 1.11 | 23 (17) | 1.01 | 4 (3) |
| `se-history/Posts.xml` | 104.2 | 22,855,471 | 22,053,563 | -3.5% | 2.01 | 346 (203) | 1.24 | 257 (134) |
| `se-history/Tags.xml` | 0.1 | 12,717 | 12,717 | +0.0% | 1.00 | 6 (6) | 1.00 | 3 (3) |
| `se-history/Users.xml` | 16.8 | 3,190,329 | 2,600,723 | -18.5% | 1.74 | 114 (118) | 1.34 | 39 (32) |
| `se-history/Votes.xml` | 42.0 | 1,154,924 | 1,154,924 | +0.0% | 1.00 | 128 (128) | 1.00 | 50 (50) |
| `uw/lineitem.xml` | 32.3 | 1,854,258 | 1,173,496 | -36.7% | 1.67 | 126 (155) | 1.52 | 60 (43) |
| `uw/nasa.xml` | 25.1 | 2,475,397 | 2,406,747 | -2.8% | 2.82 | 135 (53) | 1.10 | 52 (41) |
| `uw/SwissProt.xml` | 114.8 | 7,588,475 | 6,794,895 | -10.5% | 2.65 | 444 (304) | 1.46 | 225 (178) |
| `uw/treebank_e.xml` | 86.1 | 28,075,293 | 26,542,009 | -5.5% | 1.25 | 360 (269) | 1.22 | 163 (144) |
| all 28 | 1,675.1 | 211,189,086 | 188,711,190 | -10.6% | | | | |

- Decompressing with `-Z wasm` (WAH's JIT, and its interpreter with `-Z wasm:no-jit`) gives
  JMdict and SwissProt back byte for byte (`cache/wasm-check.txt`; 0.2 s and 1.8 s for JMdict).
- Compressing takes up to 2.3 times the instructions (DBLP: 134 -> 310 G): the sample and the
  encoder, and every container recompressed in turn with aqr-wrt's dictionary of all of them.
- The module alone, on one thread (`aqr-xml-bench`, zstd -3 of the pieces as a proxy): encode 848
  MB/s, decode 990 MB/s over the 27 files; 500-750 on files of many tags, 3000 on text.

### The choice

`aqr-compress` takes a file as XML (`Format::Xml`, `detect.rs`: a document, not HTML, text, 64 KiB
or more) and `made::table` tries it where its sample, coded and compressed by zstd -3 container by
container, is no larger than the sample compressed (`xml:percent` 100%) and, where aqr-log or
aqr-xsv would take the file, at most 135% of theirs (`xml:log-percent`). On the 47 files of the ground truth (the 26 of the table that are not a tar, and the 21 files of
silesia's `xml` tar, each coded both ways, `cache/gt2.tsv`, `cache/gt-silesia-members.tsv`) it picks
the smaller of the two within 0.2% for 46. The miss is `silesia/play3.xml`, a play of 280 KB
whose sample is as good as its neighbours' (0.93 of zstd) and whose archive is 5.3% larger with
aqr-xml (69.7 -> 73.4 KB); a threshold that kept it out would lose `se-history/Posts.xml` (0.97 of
zstd, -3.5%). Files that aqr-xml's formula does not make smaller than the text path's are given up
by `recompress_as` and cost only the sample. See `docs/xml-design.md` §2. A container's text shares one aqr-wrt dictionary
with the others of the file: JMdict -3.8%, Posts -5.5%, PubMed -6.8%, DBLP -4.1%, SwissProt -1.6%
against a dictionary for each (`cache/words.tsv`).

### The prototype

A Python prototype (the pieces archived by `aqr` as files of a directory) said: elements of text
(DBLP, PubMed, JMdict, SwissProt) gain the most, 16-24%; rows of numbers as attributes gain
nothing, as aqr-log takes the lines whole; small files lose to the cost of each container. The
numbers' own coding mattered little, so aqr-xml leaves them to aqr-log and aqr-xsv.

## Status

Left, in the order they are likely to pay:

- The choice's proxy (zstd -3 of the sample's pieces): the containers are recompressed by coders
  that do better than zstd on numbers, so it is checked against the ground truth (Results);
  a sample coded by the real coders would be exact and cost more.
- Segments are 32 MiB: a module holds its inputs, nearly as large as its output, and the host's
  copies of both count against the application bound (256 MiB; 64 MiB segments needed 269 MB).
- The skeleton's structure (attribute lists called by number, as aqr-jsonl's key lists).
- Contexts of other depth (three levels, the element alone), measured on the corpus.
- CDATA sections and comments are kept in the skeleton as they are; text with entities is a
  value as it is written (no decoding of `&amp;`).
- HTML and the like: an unclosed element (`<br>`) leaves a name open, whose end tag then pops
  the wrong one: the text round-trips, but its contexts drift (nothing resyncs them).
- Containers of numbers coded in the module (`docs/xml-design.md`, 1.1), if aqr-log's coding of
  the columns proves slow or large enough to show.

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
