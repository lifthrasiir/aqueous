# aqr-wrt

Text rebuilt bit for bit from a coded text (`FORMAT.md`). The words and markup (XML tags,
character references) that the texts use most are in a dictionary, and the text has them
replaced by codes of one to three bytes. A word's code takes the space before it, and flags
before it tell its case. This is a word replacing transform after XWRT. zstd then finds more
and shorter matches, and codes a word's code as one literal.

Text of Hangul (Korean) goes to a module of its own, `wrt-recons-hangul` (FORMAT.md §6): its
syllables are coded in two bytes rather than UTF-8's three, and its words of Hangul (each a run
of syllables with the space before it) by codes of a dictionary. ASCII letters are no words
there, and Korean words are no letters to `wrt-recons`, which left such text as it was.

- **Dictionary**: the texts of a batch of the archive share one (aqr-compress). Text that
  holds other text then codes to what holds the other's code, so a file and the same file in
  an archive, or a file and its start, are still stored once.
- **What is taken**: text of 256 KiB or more, where coding pays by the text alone (zstd -3 on
  samples). The same text is then taken alike wherever it is.

## How it does

Measured 2026-10-26 on an Apple M4 (10 cores), through `aqr` at level `mid`.

| | before | after | compress | decompress |
|---|---|---|---|---|
| `enwik9` alone (1 GB) | 283.9 MB | 253.5 MB (−10.7%) | 3.8 → 6.8 s | 1.15 → 3.44 s |
| the 10gb corpus | 2.775 GB | 2.739 GB (−35.8 MB) | 111 → 133 s | not measured |

- In the 10gb corpus, the files taken come to −36.7 MB. Of that, −30.4 MB is `enwik9` with
  `enwik9.zip`, which hold the same text and are stored once still. The files not taken come
  to −0.6 MB. Their shares of the solid blocks move both ways, by about 350 B a file, as the
  text taken leaves the blocks of the text kept as it is.
- Peak memory is 9.7 GB, against 8.2 GB before.
- The module codes 110 MB/s and decodes about 400 MB/s on a thread (`aqr-wrt-bench`, enwik8).
  - Coding a text of more than 16 MiB is spread over threads, in pieces that end with a
    byte no token holds (LF, `"`, `,`, …), so minified JSON too; the coded text is the same.
  - Decoding goes by the same pieces, on threads of their own: the module's params tell where
    they are (version 2). `enwik9` decodes at 409 MB/s on a thread and 2,875 MB/s on 10
    (`aqr-wrt-bench`, 2.5 -> 0.35 s against version 1's single pass). In Wasm the module
    still decodes on one thread.

Text of Hangul, measured 2026-10-29 on the same machine through `aqr -5`: 512 MiB from the
middle of the namuwiki dump of 2021-03-01 (JSON of one line, 72% of its bytes above 0x7F).

| | before | after | compress | decompress |
|---|---|---|---|---|
| 512 MiB of namuwiki | 164.68 MB | 145.77 MB (−11.5%) | 5.80 → 3.07 s | 0.56 → 0.42 s |

- The whole dump (8.83 GB), on a Ryzen 7 9700X: 2,565,721,176 → 2,237,433,596 bytes (−12.8%),
  compressed in 57.54 s against 64 s before.

- zstd picks level 9 for both. The coded text is 248 MB, so zstd has less to do: 143 → 105 G
  instructions in all, with the module's coding. At `-l zstd12` it is 140.21 MB in 4.4 s.
- The dictionary takes 3.2 MB at zstd -9 (694,348 entries).
- The module counts about 390 MB/s and codes about 230 MB/s on a thread, and decodes about
  560 MB/s; it does all three in pieces of 16 MiB on threads of their own. The pieces end
  before an ASCII byte other than a space (JSON need not have LF), and the module's params
  tell where.
- Peak memory 2.20 → 2.23 GB.

zstd at fixed levels (`aqr-wrt-bench`, each text with its own dictionary, the dictionary
compressed counted):

| | zstd -9 | zstd -19, window 2^27, long matching |
|---|---|---|
| enwik8 | −12.6% | −9.1% |
| bible.txt, dickens | −16.5%, −16.1% | |
| silesia `webster`, `xml`, `samba` | −12.3%, −8.8%, −0.6% | |

The design rests on an earlier prototype (Python, `zstd -19 --long`):

- On enwik8, the words alone take −6.5%, the flags of case −0.5% more, and the space taken by
  a word −1.6% more. The least count of an entry hardly matters from 3 to 32.
- Tags and character references as entries take another −0.5% on enwik8, and −1% on the
  corpus's XML and HTML files (75 MB, −5.7% in all).
- The rest of the text of the 10gb corpus (780 MB, a dictionary for all) gains −9% at
  zstd -9 but −1.5% at zstd -19. At high levels, the level itself is what pays.

Not done:

- Closing tags as one byte for the open one (XWRT's stack). They are few in enwik, and the
  common ones already have one-byte codes.
- Numbers in binary, and enwik's metadata (`<id>`, `<timestamp>`) as streams of their own.
- Text shorter than 256 KiB: `Held` does not tell whether other files hold text that short,
  and coding such text apart from the copies it shares broke the sharing of tar members with
  their files.

## Tests

`cargo test --release -p aqr-wrt` covers round trips, dictionaries shared and of pieces, and
inputs it rejects. aqr-compress `text_words` and `held_once` cover the archive.
`aqr-wrt-bench FILE...` takes `LEVEL`, `LONG` and `THREADS`.
