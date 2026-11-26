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
  JMdict and SwissProt back byte for byte (`cache/wasm-check.txt`; 0.2 s and 1.8 s for JMdict).
- Compressing takes 1.29 times the instructions in all (enwik9 +16%, 362 -> 419 G; DBLP 1.58,
  SwissProt 1.97, whose aqr-log alone was cheap): the containers are recompressed in turn, with
  aqr-wrt's dictionary of all of them. The first version took 1.79 (enwik9 +83%): see below.
- The module alone, on one thread (`aqr-xml-bench`, zstd -3 of the pieces as a proxy): encode 848
  MB/s, decode 990 MB/s over the 27 files; 500-750 on files of many tags, 3000 on text.

### The choice

`aqr-compress` takes a file as XML (`Format::Xml`, `detect.rs`: a document, not HTML, text, 64 KiB
or more) and `made::table` tries it where its sample, coded and compressed by zstd -3 container by
container, is no larger than the sample compressed (`xml:percent` 100%) and, where aqr-log or
aqr-xsv would take the file, at most 135% of theirs (`xml:log-percent`); a file under 1 MiB
is taken at 92% (`xml:small-percent`). On the 47 files of the ground truth (the 26 of the table
that are not a tar, and the 21 files of silesia's `xml` tar, each coded both ways,
`cache/gt2.tsv`, `cache/gt-silesia-members.tsv`, `cache/c5-silesia.tsv`) it takes aqr-xml or
not as the better of the two for all but `silesia/play1.xml` (+0.65%); the sizes of the files it
takes differ by up to 1.3% from those of the first version (the choices of format of the
containers on a smaller sample). The member that was a miss, `play3.xml` (0.93 of zstd, 5.3% larger
with aqr-xml), is kept out by `xml:small-percent`; `se-history/Posts.xml` (0.97, -3.5%) is big enough for the 100%. Files that aqr-xml's formula does not make smaller than
the file are given up by `recompress_as` and cost only the sample. See `docs/xml-design.md` §2. A container's text shares one aqr-wrt dictionary
with the others of the file: JMdict -3.8%, Posts -5.5%, PubMed -6.8%, DBLP -4.1%, SwissProt -1.6%
against a dictionary for each (`cache/words.tsv`).

### The cost of compressing

The first version took 1.79 times the instructions of the corpus (enwik9: 363 -> 665 G, +83%).
By innermost label (`--stats`; enwik9, thread-seconds, without -> first version), what was added:
the looks that every content makes (`table:sample` 0.1 -> 4.9, `b2t:look` 0.4 -> 2.1,
`quopri:look` 0.4 -> 1.1: the text container of each of 31 segments is a file of its own, which
made the looks a whole file does, 125 ms for aqr-log's sample alone), aqr-log on three of its
pieces (0 -> 1.8), the comparison of the formula with the file (zstd 6.8 -> 9.7), and the
dictionary shared by the containers (`words`, 7.3, as aqr-wrt's own counting was in `text`
before: the same work). The encoder itself is 0.2 s for a GB (4.8 GB/s). What was done:

- The encoder tells aqr-compress what a container is, by the kind of its stream (`Kind::Words`
  and `Kind::Column` of aqr-formula, text for all that takes text): `words` for prose (values of 40 bytes on average, a space
  for 16 bytes) is made as aqr-wrt's text and nothing else, `column` for the other containers
  of 16 KiB or more is made as any text but with a sample of a sixteenth of the size for the
  choice of aqr-log, aqr-xsv or text (`table:column-window`, 16 KiB windows). The rest (under
  16 KiB, one stream) is as before. 665 -> 435 G for enwik9; the archives vary within 1.6% either
  way for 16 KiB windows instead of 64 KiB (PubMed -1.0%, lineitem +1.2%).
- An XML file of 4 MiB or more is taken without the comparison of its formula with the file
  (`xml:uncompared`, as ProRes, tensors and Wwise are): the sample chose it, and the containers are
  recompressed in turn; a small file's formula is compared still, as the module's block alone
  is 4 KB.
- Segments of 48 MiB (not 32): a third fewer files to look at (enwik9 435 -> 425 G); a segment of
  64 MiB passes the application bound (256 MiB), which 48 leaves 13 MiB of record to spare for.
- A file under 1 MiB is taken only where its sample is at most 92% of the text's (`xml:small-percent`):
  the fixed costs of the format are most of what a small file would gain (silesia's `play3.xml`,
  0.93, was 5.3% larger).

enwik9 now: 233,040,175 -> 230,326,223 B (-1.16%), 362 -> 419 G (+16%), peak 1.28 GB (1.35
without); the spill peaks at 1.45 GB, not 0.73: the containers are a copy of the text (987 MB of
it spilled, until their contents are made), which is what a split costs. DBLP 134 -> 212 G (1.58;
it was 2.31), SwissProt 55 -> 108 G (aqr-log alone is cheap there), OpenOffice's `scalc.jar`
325,310 -> 313,952 B (-3.5%) for 5.1 -> 6.5 G, `main.xcd` (1.7 MB) 107,698 -> 104,670 B (-2.8%)
for 1.8 -> 3.5 G. The corpus: -10.95% (-10.64% at the first version) for 1.29 times the
instructions (1.79). Not done: containers freed as soon as their contents are made (the spill),
and one decision of a context's format for all of its segments (each segment's container is a
file of its own, and looked at as such, now a sixteenth of the sample).

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
- Segments are 48 MiB: a module holds its inputs, nearly as large as its output, and the host's
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
