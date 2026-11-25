# aqr-xml

XML of records (dumps of wikis, Q&A sites, maps, bibliographies) rebuilt bit for bit from a
skeleton and its values in containers by path, after XMill: the tags and the whitespace
between them in the skeleton, each element's text and each attribute's value in the container
of its path. Not written yet: the format is drafted in [FORMAT.md](FORMAT.md), the design in
`docs/xml-design.md`, and a prototype's measurements are below.

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
