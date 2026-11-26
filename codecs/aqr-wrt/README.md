# aqr-wrt

Text rebuilt bit for bit from a coded text. The words and markup (XML tags,
character references) that the texts use most are in a dictionary, and the text has them
replaced by codes of one to three bytes. A word's code takes the space before it, and flags
before it tell its case. This is a word replacing transform after XWRT. zstd then finds more
and shorter matches, and codes a word's code as one literal.

Text of Hangul (Korean) goes to a module of its own, `wrt-recons-hangul` (FORMAT.md §6): its
syllables are coded in two bytes rather than UTF-8's three, and its words of Hangul (each a run
of syllables with the space before it) by codes of a dictionary. ASCII letters are no words
there, and Korean words are no letters to `wrt-recons`, which left such text as it was.

`FORMAT.md` specifies the modules, and how it works (the dictionary, the flags, the pieces and
segments on threads, and the design notes). The texts of a batch of the archive share one
dictionary (aqr-compress): text that holds other text codes to what holds the other's code, so
a file and the same file in an archive, or a file and its start, are still stored once.

## What is taken

Text of 256 KiB or more, where coding pays by the text alone (zstd -3 on samples); the same text
is then taken alike wherever it is. Not taken: shorter text.

## Results

Through `aqr` at level `mid`, on an Apple M4 (10 cores), 2026-10-26 (`enwik9`'s size of 2026-11-26):

| | before | after | compress | decompress |
|---|---|---|---|---|
| `enwik9` alone (1 GB) | 283.9 MB | 233.04 MB (-17.9%; version 4, 2026-11-26) | 3.8 -> 6.8 s | 1.15 -> 3.44 s |
| the 10gb corpus | 2.775 GB | 2.739 GB (-35.8 MB) | 111 -> 133 s | not measured |

Peak memory on the 10gb corpus is 9.7 GB, against 8.2 GB before. The module codes 110 MB/s and
decodes about 400 MB/s on a thread (`aqr-wrt-bench`, enwik8); `enwik9` decodes at 2,875 MB/s on
10 threads (409 MB/s on one; in Wasm one thread).

At fixed levels (`aqr-wrt-bench`, each text with its own dictionary, the dictionary counted):

| | zstd -9 | zstd -19, window 2^27, long matching |
|---|---|---|
| enwik8 | -13.4% (26.862 MB) | -9.1% |
| bible.txt, dickens | -16.5%, -16.1% | |
| silesia `webster`, `xml`, `samba` | -12.3%, -8.8%, -0.6% | |

Text of Hangul (`wrt-recons-hangul`), 512 MiB from the middle of the namuwiki dump of
2021-03-01 (JSON of one line, 72% of its bytes above 0x7F), through `aqr -5`, same machine,
2026-10-29:

| | before | after | compress | decompress |
|---|---|---|---|---|
| 512 MiB of namuwiki | 164.68 MB | 145.77 MB (-11.5%) | 5.80 -> 3.07 s | 0.56 -> 0.42 s |

The whole dump (8.83 GB), on a Ryzen 7 9700X: 2,565,721,176 -> 2,237,433,596 bytes (-12.8%),
compressed in 57.54 s against 64 s before; peak memory 2.23 GB. The module codes about 230 MB/s
and decodes about 560 MB/s on a thread, in pieces on threads of their own.

## Tests

`cargo test --release -p aqr-wrt`:

- round trips, dictionaries shared and of pieces, and inputs it rejects.
- aqr-compress `text_words` and `held_once` cover the archive.

`aqr-wrt-bench FILE...` takes `LEVEL`, `LONG`, `THREADS` and `Z` (`-Z` options, separated by
spaces).

## Left

- **Closing tags.** As one byte for the open one (XWRT's stack); few in enwik, and the common
  ones already have one-byte codes.
- **Numbers and metadata.** Numbers in binary, and enwik's metadata (`<id>`, `<timestamp>`) as
  streams of their own.
- **Short text.** Text under 256 KiB is not taken: `Held` does not tell whether other files hold
  text that short, and coding such text apart from the copies it shares broke the sharing of
  tar members with their files.
