# aqr-pdf

Lossless PDF recompression: a PDF file is stored smaller, and restored bit for bit. Speed comes
first: the target is 100 MB/s of PDF file single-threaded on an Apple M4, both ways, and the
compression ratio is pursued only within that time (the encoder plans each file's streams to its
budget). DEFLATE streams go through [aqr-flate](../aqr-flate), images through
[aqr-png](../aqr-png) and [aqr-jpeg](../aqr-jpeg). `FORMAT.md` specifies the module, and how it
works.

```sh
cargo build --release --example aqr-pdf   # target/release/examples/aqr-pdf
aqr-pdf bench [-t N] [-n ITERS] [-s MB/S] [-l LEVEL] [-p PROFILE] [-Z] [-V] [-T] [-F] [-P] [-v] FILE...   # formula and .aqr: ratio, CPU-time speed, per-mode sizes
aqr-pdf fuzz [-n ROUNDS] [-r SEED] FILE...            # mutated files, formulas and archives
```

```rust
// The formula of a PDF file (None where it would not be smaller), and the file back from it.
let f = aqr_pdf::formula::encode(&pdf, 1).unwrap();
let run = |a: &Apply, inputs: &[&[u8]]| aqr_pdf::formula::apply(a, inputs, 1);
assert_eq!(f.eval(&run)?, pdf);
```

The formula is a file of the Aqueous archives (`.aqr`) through aqr-formula, as aqr-compress
makes it. `-t` is the number of threads (`bench`: 1 by default, timed in CPU time with one
thread and wall time with more), `-s` is the speed target (`inf`: no limit), `-l` is the zstd
level the encoder measures the table, skeleton and pools at to judge whether the formula pays,
`-p` is aqr-flate's profile of the full mode's records (6 by default; `-Z` turns off zlib
emulation with an adaptive profile), `-V` verifies by evaluating the formula (and on a mismatch codes the file again without
models), `-T`/`-F`/`-P` turn off the token, full and PNG modes. Any input is accepted; what is
not a PDF goes through as skeleton.

## What is taken

Any input is accepted; what is not a PDF goes through as skeleton. Streams are found wherever
they are (incremental updates, broken cross-reference tables and junk do not matter):
FlateDecode (re-parsed by aqr-flate, or its tokens recoded), FlateDecode images with PNG
predictors, and DCTDecode (JPEG). Encrypted files with the Standard security handler and the
empty user password (RC4 40-128 bits, AES-128, AES-256) are decrypted, coded and sealed again.
Not taken (kept raw or in the skeleton): uncompressed and ASCII-encoded streams, JPX, CCITT, JBIG2,
files with a non-empty user password.

## Results

Measured on 2026-10-23 at `2abe944` on an Apple M4, one thread, through `aqr-show` (see the
[file formats overview](../../docs/file-formats.md#how-it-was-measured)): each file archived at
level `mid` with its round-trip check, then extracted. Ratios are tentative (the decoder modules
were not in the archives then). Measured again with the modules in the archives on 2026-11-01
(`docs/file-formats.md`, 123 files, 456 MB): 0.822-0.836 of the input (zstd -3 0.918).

| set | files | MB | ratio | zstd -3 | compress | decompress |
|---|---|---|---|---|---|---|
| cc | 138 | 380.3 | 0.861 | 0.899 | 45.9 MB/s | 202.1 MB/s |
| arxiv | 6 | 22.2 | 0.753 | 0.843 | 48.3 MB/s | 147.9 MB/s |
| all | 144 | 402 | **0.855** | 0.896 | **46 MB/s** | **198 MB/s** |

Encrypted files (2026-11-10, Apple M4, `aqr -c` at the default level, then `aqr -K`): 19 of the
first 1,000 files of CC have an `/Encrypt` entry, 18 understood (RC4 40 and 128 bits, the `/V2`
crypt filter, AESV2, AESV3; one with a user password is kept raw). The ratio of their streams
alone is 0.55-0.95; in an archive the modules' bytes (about 50 KB) decide, so only the files with
images that gain much pay:

| file | bytes | kept raw (before) | archive (after) | compress | test | RSS compress / test |
|---|---|---|---|---|---|---|
| cc/0000849 | 62,090,949 | 62,090,983 | 32,128,869 | 0.33 -> 1.67 s | 0.19 -> 0.82 s | 390 -> 500 / 122 -> 167 MB |
| cc/0000311 | 1,725,114 | 1,725,148 | 1,441,220 | 0.16 -> 0.28 s | 0.14 -> 0.24 s | 23 -> 60 / 7 -> 17 MB |
| cc/0000693 | 1,244,289 | 1,244,322 | 1,061,974 | 0.16 -> 0.26 s | 0.14 -> 0.23 s | 19 -> 41 / 6 -> 13 MB |
| cc/0000397 | 559,033 | 501,146 | 411,230 | 0.17 -> 0.25 s | 0.14 -> 0.22 s | 34 -> 38 / 5 -> 11 MB |
| cc/0000325 | 356,672 | 356,706 | 313,472 | 0.16 -> 0.24 s | 0.14 -> 0.23 s | 13 -> 19 / 5 -> 12 MB |

The other 14 are unchanged in size; the 19 files together: 70,250,369 -> 39,580,951 bytes. The
rest of CC (981 files) is not affected: all 1,000 as one archive are 1,240,289,829 bytes before
and after (13 s, 2.8 GB peak both).

Memory (2026-10-24, one process per file) is 41 MB + 5.7x the file (a linear fit); nine files
take over twice that, up to 331 MB for 3.4 MB (`cc/0000192.pdf`).

The speed target binds per file, not just per set. aqr-compress keeps the target and aqr-flate's
profile 6 up to level 6; from level 7 on it lifts the target and takes the adaptive profile (the
cmz test set's `irs_p17.pdf`, 3.09 MB: 2.80 MB at level 8 before, 2.26 MB after; 1.99 MB at 9).

## Corpus

`corpus/` is not tracked; the scripts rebuild it.

- `scripts/link-home-corpus.sh`: the PDF files under `$HOME`, symlinked (deduplicated) into
  `corpus/home/`: 20 files, 314 MB, mostly Unicode code charts (PDFlib, Apache FOP: fonts
  and small glyph images), a few papers and diagrams.
- `scripts/fetch-corpus.sh`: `corpus/cc/`, 1,000 PDF files from Common Crawl (SafeDocs
  CC-MAIN-2021-31-PDF-UNTRUNCATED, digitalcorpora.org, zip 0000; 1.38 GB of every kind of
  producer), `corpus/arxiv/`, 20 arXiv papers (94 MB), and `corpus/pdf20/`, the
  PDF Association's 7 PDF 2.0 example files (pdf20examples, pinned; 54 KB, nearly all
  skeleton: a check that PDF 2.0 parses, not a measure of ratio).
- `scripts/baseline.sh SET...`: zstd and xz on each file, cached in `cache/`.

What CC holds (`examples/survey.rs`): DCT images 35%, FlateDecode images 29% (11.8×
inflated), fonts 12% (2.2×), content streams 10% (4.5×), JPX/CCITT/JBIG2 images 6%,
skeleton 4%.

## Tests

```sh
cargo test --release
../../target/release/examples/aqr-pdf fuzz -n 5000 $(find corpus -name '*.pdf' -size -600k)
```

- `tests/formula.rs`: synthetic PDF files through `formula::encode_with` and `Formula::eval`
  with the nested modules, under every set of options and with 1 to 8 threads; the
  module's input slots corrupted.
- `tests/memory.rs`: peak heap of a file whose streams inflate 36×, with 1 and 4 threads.
- `tests/data/enc-*.pdf` (`scripts/make-encrypted.py`, which writes the files by ISO 32000's
  algorithms with pycryptodomex; poppler's `pdftotext` opens all of them): RC4 40 and 128
  bits, the `/V2` crypt filter, AESV2 (also with plain metadata), AESV3 revisions 5 and 6
  (with salts for which the two readings of "the round number" of algorithm 2.B differ). Each
  goes through the formula, and with a wrong `/U` it is not understood. `src/crypt.rs` has
  the RFC 1321, RFC 6229 and FIPS 197 vectors, and AES against the `aes` crate.
- `tests/roundtrip.rs`: FlateDecode streams from every encoder of flate-zoo, through every
  mode; zlib wrapper variants (bare DEFLATE, wrong Adler-32, junk after the data,
  truncated and damaged streams); syntax (indirect and wrong lengths, CR line ends,
  keywords in strings, incremental updates, unterminated streams); images with and without
  predictors, geometry that does not match the data; JPEG; duplicates; encrypted files;
  PDF 2.0 (junk before the header, UTF-8 strings, object and cross-reference streams,
  AES-256); non-PDF and cut files.
- Unit tests: lexer, scanner, rANS and bit I/O, token coding (and its window sliding).
- `aqr-pdf fuzz`: mutated files (bit flips, cuts, deletions, junk, copied pieces, bytes inside
  streams) must round-trip through the formula under every set of modes with the safety
  nets off; corrupted archives must fail cleanly; with 2 to 4 threads, the same formula and
  the same outcome.
- Every file of the four sets round-trips through `.aqr` (`aqr-pdf bench` checks).

## Layout

| File | Contents |
|---|---|
| `src/formula.rs` | The module: its decoder, and the encoder's entry points |
| `src/encode.rs` | The cost model and planner, and the formula's encoder |
| `src/scan.rs`, `src/lex.rs` | Finding streams (and their object numbers), PDF objects |
| `src/standard.rs`, `src/crypt.rs` | The Standard security handler (encoder); MD5, RC4, AES encryption (module) |
| `src/kind.rs`, `src/objects.rs` | Stream classes and filters, indirect objects |
| `src/flate.rs`, `src/image.rs` | zlib wrapper; image geometry and the PNG wrapper |
| `src/tok.rs`, `src/rans.rs` | Token mode; static rANS (`aqr_common::rans32` with 12-bit probabilities) |
| `src/module.rs`, `src/plan.rs` | The decoder alone, for the Wasm module; its memory plan (`aqr_api::stack`) |
| `src/main.rs` | Command-line tool, bench, fuzz |
| `examples/` | Corpus surveys and experiments behind the choices here; `memprobe.rs`, peak heap and RSS per file (`LOG=MB` logs large allocations) |
| `scripts/` | Corpus, baselines, profiling (`profile.py` over samply) |

## Left

- **JPEG speed.** DCT data is 35% of CC, and only about half of it fits the budget. aqr-jpeg takes
  about 15 ns/B plus 250 us (compressing) / 125 us (decompressing) per image on PDF images, mostly
  per-file table building; default tables for small images would help.
- **Encrypted files.** A non-empty user password (the `/U` check fails: 1 of 19 encrypted files
  in CC), crypt filters named per stream, and equal plain streams (`DUP` of encrypted streams:
  their ciphertexts differ).
- **Filter chains.** `[/ASCII85Decode /FlateDecode]` and `[/FlateDecode /DCTDecode]` (2.8 and 28
  MB in CC) are kept raw or in the skeleton.
- **Token decoding speed.** 11-15 ns/B; its order-1 contexts form a serial chain of table lookups.
- **Skeleton.** About 15% of the output; classic cross-reference tables could be rebuilt from
  object offsets (about 0.8 MB of the 18 MB on CC), object headers predicted.
