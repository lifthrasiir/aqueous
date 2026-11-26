# aqr-xml

XML of records (dumps of wikis, Q&A sites, maps, bibliographies) rebuilt bit for bit from a
skeleton and its values in containers by context, after XMill: the tags and the whitespace
between them in the skeleton, each element's text and each attribute's value in the container
of its context (an element's text by its parent and name, an attribute's by its element and
name). The module `xml-recons` only takes the text apart and puts it back; it codes no numbers.
Each container is text (`Kind::Text`, a content), which aqr-compress is to recompress in turn
(aqr-log or aqr-xsv for a column of numbers or times, aqr-wrt for words). Whatever is not XML
round-trips too, kept in the skeleton. The format is specified in [FORMAT.md](FORMAT.md), the
design is in `docs/xml-design.md`. aqr-compress does not choose it yet.

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

The module on the corpus below, on an Apple M4 (`cache/bench-2026-11-26.txt`; speeds on one
thread, of the file's bytes). Sizes in bytes at zstd -3: the file itself, against the skeleton,
`table` and `lens` and each container compressed on its own (what aqr-compress hands to the
coders; aqr-wrt, aqr-log and aqr-xsv do better than zstd on the containers). The speeds are the
module's alone, at the order of what the file's text is made of (most bytes in values: faster).

| file | MB | segments | containers | file | split | | encode MB/s | decode MB/s |
|---|---|---|---|---|---|---|---|---|
| `enwik8` | 100.0 | 2 | 33 | 35,419,425 | 35,071,278 | -1.0% | 5100 | 7985 |
| `silesia/xml` | 5.3 | 1 | 526 | 635,299 | 569,468 | -10.4% | 508 | 714 |
| `dblp/dblp-head.xml` | 268.4 | 4 | 297 | 47,990,209 | 41,888,078 | -12.7% | 734 | 740 |
| `pubmed/pubmed26n0001.xml` | 195.4 | 3 | 279 | 16,412,957 | 12,663,672 | -22.8% | 749 | 871 |
| `jmdict/JMdict_e` | 63.3 | 1 | 23 | 11,139,643 | 8,516,809 | -23.5% | 539 | 640 |
| `osm-hours/503.osc` | 71.7 | 2 | 39 | 5,773,624 | 4,265,481 | -26.1% | 504 | 731 |
| `osm-hours/504.osc` | 163.8 | 3 | 48 | 8,693,516 | 5,948,930 | -31.6% | 509 | 735 |
| `uw/lineitem.xml` | 32.3 | 1 | 17 | 2,891,098 | 1,565,369 | -45.9% | 722 | 802 |
| `uw/nasa.xml` | 25.1 | 1 | 60 | 3,396,104 | 3,075,601 | -9.4% | 924 | 1159 |
| `uw/SwissProt.xml` | 114.8 | 2 | 379 | 11,370,776 | 8,408,307 | -26.1% | 517 | 648 |
| `uw/treebank_e.xml` | 86.1 | 2 | 1399 | 33,181,033 | 29,618,945 | -10.7% | 654 | 904 |
| `se-coffee/Badges.xml` | 1.6 | 1 | 6 | 231,469 | 168,792 | -27.1% | 466 | 695 |
| `se-coffee/Comments.xml` | 1.6 | 1 | 7 | 517,597 | 465,999 | -10.0% | 1181 | 1361 |
| `se-coffee/PostHistory.xml` | 9.5 | 1 | 10 | 2,173,064 | 1,954,804 | -10.0% | 1867 | 1745 |
| `se-coffee/PostLinks.xml` | 0.1 | 1 | 5 | 11,052 | 8,331 | -24.6% | 444 | 660 |
| `se-coffee/Posts.xml` | 5.7 | 1 | 22 | 1,632,455 | 1,553,098 | -4.9% | 1980 | 1749 |
| `se-coffee/Tags.xml` | 0.0 | 1 | 5 | 1,926 | 1,659 | -13.9% | 238 | 475 |
| `se-coffee/Users.xml` | 4.5 | 1 | 12 | 1,108,417 | 972,966 | -12.2% | 801 | 961 |
| `se-coffee/Votes.xml` | 2.0 | 1 | 6 | 173,598 | 113,832 | -34.4% | 508 | 706 |
| `se-history/Badges.xml` | 10.9 | 1 | 6 | 1,552,686 | 1,170,721 | -24.6% | 479 | 687 |
| `se-history/Comments.xml` | 47.8 | 1 | 7 | 15,534,232 | 14,048,627 | -9.6% | 1246 | 1319 |
| `se-history/PostHistory.xml` | 301.7 | 5 | 50 | 48,720,964 | 45,562,240 | -6.5% | 3272 | 2295 |
| `se-history/PostLinks.xml` | 0.3 | 1 | 5 | 51,823 | 40,724 | -21.4% | 504 | 687 |
| `se-history/Posts.xml` | 104.2 | 2 | 44 | 31,744,210 | 30,720,903 | -3.2% | 3141 | 2107 |
| `se-history/Tags.xml` | 0.1 | 1 | 5 | 14,050 | 10,498 | -25.3% | 352 | 543 |
| `se-history/Users.xml` | 16.8 | 1 | 12 | 3,936,904 | 3,336,130 | -15.3% | 773 | 911 |
| `se-history/Votes.xml` | 42.0 | 1 | 6 | 3,025,439 | 2,207,515 | -27.0% | 520 | 700 |
| all 27 | 1,675.1 | | | 287,333,570 | 253,928,777 | -11.6% | 848 | 990 |

- Elements of text and records of many fields (DBLP, PubMed, JMdict, SwissProt, lineitem) gain
  the most; rows of attributes in one element (Posts, PostHistory, Comments: a long `Body`)
  gain least, as their text is most of the bytes. Small files gain too at zstd, but the
  prototype's measurements through `aqr` (above) say that aqr-log takes rows of attributes
  whole already: choosing is aqr-compress's (phase B).
- The skeleton is where the structure is: 31 KB of enwik8's 35.1 MB, but 3.5 MB of DBLP's 41.9,
  1.4 of PubMed's 12.7, 1.5 of SwissProt's 8.4 and 5.6 of treebank's 29.6 (zstd -3). `table`
  and `lens` are small except where values hold LF (enwik8's `text`: 22 KB of lengths).
- Containers: 5 to 60 for most files, 297 for DBLP, 1399 for treebank (its elements' parents
  vary most; the bound is 4096 for a segment). Few are big.
- Memory: the module's plan is the text plus the containers' table and the stack of names (a
  few KiB): decoding PostHistory (301.7 MB) takes five segments of 64 MiB or a little more.

## Status

Left, in the order they are likely to pay:

- aqr-compress choosing it (phase B): by a sample, against aqr-log for files of rows of
  attributes; a minimum length (`MIN_LEN`, 64 KiB now); which classes recompress each container
  (see `formula.rs`: one class `xml/values`; `segment_words` of aqr-compress takes the contents
  of a segmented text only where the root is a concatenation).
- The skeleton's structure (attribute lists called by number, as aqr-jsonl's key lists).
- Contexts of other depth (three levels, the element alone), measured on the corpus.
- CDATA sections and comments are kept in the skeleton as they are; text with entities is a
  value as it is written (no decoding of `&amp;`).
- HTML and the like: an unclosed element (`<br>`) leaves a name open, whose end tag then pops
  the wrong one: the text round-trips, but its contexts drift (nothing resyncs them).
- Containers of numbers coded in the module (`docs/xml-design.md`, 1.1), if aqr-log's coding of
  the columns proves slow or large enough to show.

## Prototype

A prototype in Python (2026-11-26) split each sample into a skeleton (the tags, the whitespace
between them, comments; a byte for each value taken out) and containers (each element's text by
its parent and name, each attribute's value by its element and name; the values of a container
one after another, separated by LF), and the pieces were archived by `aqr` as files of a
directory: aqr-wrt takes the containers of text (with one dictionary), aqr-log those of numbers
and times. Against `aqr` on the sample itself (sizes in MB):

| sample | MB | `aqr` | split, then `aqr` | |
|---|---|---|---|---|
| `enwik8` | 100.0 | 26.899 | 26.664 | −0.9% |
| silesia `xml` | 5.3 | 0.509 | 0.473 | −7.0% |
| `dblp` | 268.4 | 39.742 | 31.671 | −20.3% |
| `pubmed` | 195.5 | 11.829 | 9.032 | −23.6% |
| `jmdict` | 63.3 | 7.740 | 6.342 | −18.1% |
| `uw` SwissProt | 114.8 | 7.588 | 6.350 | −16.3% |
| `uw` lineitem | 32.3 | 1.854 | 1.098 | −40.8% |
| `uw` treebank | 86.1 | 28.075 | 27.055 | −3.6% |
| `uw` nasa | 25.1 | 2.478 | 2.371 | −4.3% |
| `se-history` Comments | 47.8 | 11.554 | 9.931 | −14.0% |
| `se-history` PostHistory | 301.7 | 34.268 | 30.071 | −12.2% |
| `se-history` Posts | 104.2 | 22.895 | 21.900 | −4.3% |
| `se-history` Users | 16.8 | 3.195 | 2.529 | −20.8% |
| `se-history` Votes | 42.0 | 1.155 | 1.181 | +2.3% |
| `se-history` Badges | 10.9 | 0.667 | 0.663 | −0.7% |
| `osm-hours` (two) | 235.6 | 6.411 | 6.345 | −1.0% |
| all 27 files (with `se-coffee` and the rest) | 1,675 | 211.406 | 187.788 | −11.2% |

- Elements of text (DBLP, PubMed, JMdict, SwissProt) gain the most: their values, apart from
  the tags, are what aqr-wrt and zstd code well.
- Rows of numbers as attributes, one to a line (`Votes`, `Badges`, OSM's changes), gain
  nothing: aqr-log takes the lines whole already, a template for each and its fields in
  columns. Small files lose (`se-coffee`'s `Badges` +22%, `Votes` +15%): every container costs.
- The numbers' own coding mattered little: coded by the prototype as changes from the
  previous value, they were larger than as aqr-log codes them.

So aqr-xml would take the XML apart and leave the containers to the coders there are (aqr-wrt,
aqr-log), and be chosen where it pays, as aqr-compress's `table` chooses aqr-log.

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
