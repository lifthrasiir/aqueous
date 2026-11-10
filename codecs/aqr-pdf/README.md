# aqr-pdf

Lossless PDF recompression: a PDF file is stored smaller, and restored bit for bit. Meant as
a component of a general-purpose compressor, so speed comes first: the target is 100 MB/s
of PDF file single-threaded on an Apple M4, both ways. Compression ratio is pursued only
within that time. DEFLATE streams go through [aqr-flate](../aqr-flate) (parsing,
writing, Huffman header coding, its fast representation), images through
[aqr-png](../aqr-png) and [aqr-jpeg](../aqr-jpeg).

```sh
cargo build --release
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

## Results

Measured on 2026-10-23 at `2abe944` on an Apple M4, one thread, through `aqr-show` (see the
[file formats overview](../../docs/file-formats.md#how-it-was-measured)): each file archived at level `mid` with
its round-trip check, then extracted. Ratios are tentative, since the decoder modules were not
in the archives then. Memory is the peak RSS of that process (input, archive and output
included) over the input size.

| set | files | MB | ratio | zstd -3 | compress | decompress |
|---|---|---|---|---|---|---|
| cc | 138 | 380.3 | 0.861 | 0.899 | 45.9 MB/s | 202.1 MB/s |
| arxiv | 6 | 22.2 | 0.753 | 0.843 | 48.3 MB/s | 147.9 MB/s |
| all | 144 | 402 | **0.855** | 0.896 | **46 MB/s** | **198 MB/s** |

Encrypted files, measured on 2026-11-10 (Apple M4, `aqr -c` at the default level, then `aqr -K`;
times include `cargo aqr`'s 0.14 s start). 19 of the first 1,000 files of CC have an `/Encrypt`
entry: RC4 40 bits (revision 2: 2 files), RC4 128 bits (revision 3: 7), the `/V2` crypt filter
(revision 4: 4), AESV2 (revision 4: 5, one of them with a user password, whose `/U` does not
match the empty one: kept raw) and AESV3 (revision 6: 1, `cc/0000120.pdf`). 18
are understood. The ratio of their streams alone is 0.55-0.95 (`aqr-pdf bench`); in an archive
the modules' bytes (about 50 KB) decide, so only the files with images that gain much pay (the
modules of JPEG were rebuilt for this measurement: they were stale in the tree):

| file | bytes | kept raw (before) | archive (after) | compress | test | RSS compress / test |
|---|---|---|---|---|---|---|
| cc/0000849 | 62,090,949 | 62,090,983 | 32,128,869 | 0.33 -> 1.67 s | 0.19 -> 0.82 s | 390 -> 500 / 122 -> 167 MB |
| cc/0000311 | 1,725,114 | 1,725,148 | 1,441,220 | 0.16 -> 0.28 s | 0.14 -> 0.24 s | 23 -> 60 / 7 -> 17 MB |
| cc/0000693 | 1,244,289 | 1,244,322 | 1,061,974 | 0.16 -> 0.26 s | 0.14 -> 0.23 s | 19 -> 41 / 6 -> 13 MB |
| cc/0000397 | 559,033 | 501,146 | 411,230 | 0.17 -> 0.25 s | 0.14 -> 0.22 s | 34 -> 38 / 5 -> 11 MB |
| cc/0000325 | 356,672 | 356,706 | 313,472 | 0.16 -> 0.24 s | 0.14 -> 0.23 s | 13 -> 19 / 5 -> 12 MB |

The other 14 are unchanged in size (the formula is smaller than the file but not than the file
and the modules); 19 files: 70,250,369 -> 39,580,951 bytes. The rest of CC (981 files) is
not affected: all 1,000 as one archive are 1,240,289,829 bytes before and after (13 s, 2.8 GB
peak both; the modules of JPEG as they are in the tree).

18 files are kept as they are. Memory, re-measured on 2026-10-24 at `da98d11` with one
process per file, is 41 MB + 5.7× the file (a linear fit). Nine files take over twice that,
up to 331 MB for 3.4 MB (`cc/0000192.pdf`), 202 MB for 1.8 MB (`cc/0000360.pdf`) and 295 MB
for 13.7 MB (`arxiv/2307.09288.pdf`); most files under 1 MB take less than half of it.

The speed target (100 MB/s of PDF file single-threaded on an Apple
M4, both ways) binds: everything coded costs more per byte than the budget allows on average,
so the ratio is what fits in it, and the target is met per file, not just per set: the budget
is per file (below) and planned with a 10% margin for the scatter of the estimates.
aqr-compress keeps the target and profile 6 up to level 6; from level 7 on it lifts the
target and takes aqr-flate's adaptive profile, as for DEFLATE elsewhere (on the cmz test
set's `irs_p17.pdf`, 3.09 MB: 2.80 MB at level 8 before, 2.26 MB after; 1.99 MB at 9).

## How

### The budget

Everything that pays costs about the budget or more: inflating a DEFLATE stream alone runs
at about 200 MB/s of DEFLATE data (5 ns/B, half the 10 ns/B the target allows), and the
coders measured here take 10-115 ns per byte of stream (below). So the compressor does not
code everything it can; it plans:

- Every stream gets the modes it allows, each with an estimated size and time: a cost
  model fitted to per-item measurements of the corpus (ns per item, per stream byte and per
  inflated byte, each way; for JPEG, per 8×8 block from the frame header instead, which
  halves the error).
  Inflated sizes come from the dictionary (images' geometry, fonts' `/Length1`), else
  from the first DEFLATE block's code lengths (expected bytes per bit with probabilities
  2^-length: a third of the class prior's error on content streams), else a per-class
  prior; planning inflates nothing.
- The mode of each stream minimises *size + λ·time*, with the smallest λ that keeps the
  file's total time (the larger of compression and decompression per item) within its
  budget, `file size / (1.1 × target speed)`. The estimates are within a few percent of
  measured times over each set and scatter by about 10% per file (content the features
  do not see: aqr-flate on some encoders' streams, JPEG entropy), hence the margin.
  The estimates, not timing, decide, so the output does not depend on the machine's
  load.

### Streams (`scan.rs`, `kind.rs`)

A stream is any `N G obj << ... >> stream` whose dictionary parses, wherever it is
(incremental updates, broken cross-reference tables and junk do not matter); its data runs
for `/Length` bytes when that is direct and followed by `endstream`, else to the next
`endstream`. What is not recognised stays in the skeleton, so a miss costs ratio, never
correctness. Streams are classed (content, font, image, object stream, cross-reference
stream, other data) by their dictionaries; indirect objects (colour spaces, decode
parameters) are looked up through an index of every `N G obj` (`objects.rs`).

The file is cut into the skeleton and *items*:

| mode | what | coder |
|---|---|---|
| raw | stream data kept as it is (incompressible, or not worth its time) | none |
| dup | a stream equal to an earlier one | reference |
| tok | FlateDecode: the DEFLATE tokens recoded (`tok.rs`) | own, rANS |
| full | FlateDecode: inflated data (pooled per class) + aqr-flate's fast metadata (a record: u16 profile + body) | aqr-flate |
| png | FlateDecode image with PNG predictors, wrapped as a PNG file | aqr-png |
| jpeg | DCTDecode | aqr-jpeg |

Uncompressed and ASCII-encoded streams stay in the skeleton, as does anything after a
DEFLATE stream's data. A file with an `/Encrypt` entry that is not understood keeps its
streams raw (they look random).

### Encrypted files (`standard.rs`, `crypt.rs`)

The Standard security handler with the empty user password is read (the `/Encrypt`
dictionary and the first `/ID`; the file key is checked against `/U`, so a wrong key is never
used): RC4 of 40 to 128 bits (`/V` 1, 2 and the `/V2` crypt filter of `/V` 4; revisions 2–4),
AES-128 (`/AESV2`, revision 4) and AES-256 (`/AESV3`, revisions 5 and 6). Each stream is
decrypted with its object's key, coded as an unencrypted one would be, and encrypted again by
the module: the item is prefixed (`ENC`) with the object number and generation, the stored IV
(AES) and the few bytes after the DEFLATE data that the length counts. A stream is sealed
only if the plain bytes encrypt back to it exactly (AES: whole blocks after the IV, canonical
PKCS#5 padding); cross-reference streams, the metadata of a file with `/EncryptMetadata
false`, streams with their own `/Crypt` filter and streams shorter than 64 bytes stay as they
are, as do strings (they are in the skeleton and cost the same as before).

The formula keeps only the file key, as the module's parameters (a byte for the method and
5 to 32 bytes of key). The module derives each object's key with MD5 (algorithm 1 of ISO
32000-1) and encrypts with RC4 or AES (encryption only: `crypt.rs`, about 200 lines, no
decryption tables); the decoder needs no SHA-2, no AES decryption and no password logic.
Keeping the per-object keys instead would drop MD5 (1.5 KB of the module) and cost 16 bytes
of incompressible key for each stream (8 bytes for the object number and generation, against
about 3 bytes now); the file key is smaller as soon as a file has a handful of streams. The
module grew from 80,782 to 86,598 bytes (block 25,234 to 27,721).

### Token mode (`tok.rs`, `rans.rs`)

The stream's own LZ77 parse is kept, so rebuilding needs no match search, only Huffman
coding (the aqr-png approach, for any stream):

- Literals and length codes share one alphabet; the table is chosen by the byte before
  (per class and file, at the resolution that costs least). Distance codes have a table per
  length code.
- Images without a predictor are coded as pixels: a literal is the residual against MED
  (left, up, up-left), plus the previous channel's residual for every channel but the
  first, with tables by channel, local activity and whether a match came just before.
- Dynamic Huffman headers go through aqr-flate's header coder against the block's
  frequencies (zlib's own trees cost a bit).
- Static per-file tables and four interleaved rANS states (after aqr-png's coder, with
  alphabets beyond 256). All token streams of a file share one rANS stream, decoded in file
  order.
- Memory does not follow the inflated data: a stream is parsed straight into its *part*
  (its symbols, the extra bits of its matches), the literals' contexts read from a window of
  the data (32 KiB, or a row and a pixel more for pixels, sliding every 256 KiB), and rebuilt
  block by block through the same window. What the encoder holds is the file's symbols (4
  bytes each) for the static tables; the full mode takes only the data's length and
  Adler-32 from its parse. `tests/memory.rs`: 56 MB of data in a 1.55 MB file takes 18 MB
  to code and 7 MB to rebuild (109 and 80 MB before).

### Format

The format is specified in [FORMAT.md](FORMAT.md).

### Threads

`-t N` (`Options::threads`): the formula is the same whatever the number of threads, and one
thread runs the code as before (aqr-jpeg on rayon's global pool). With more, a pool of N
threads codes the items ahead, in order, at most 4N + 4 of them and about 256 MB of estimated
working memory in flight (always one): JPEG, PNG and aqr-flate items whole, and FlateDecode
streams for the token mode made into their parts (below); the calling thread adds them to the
sections in order, and to the token coder, whose tables and bit streams carry over from
stream to stream. aqr-jpeg's own
parallelism runs on the same pool, so it takes N threads in all (aqr-flate, one thread per
item). The token coder's final pass runs beside the zstd that measures the table, skeleton
and pools. The decoder of the module takes no threads: the nested modules rebuild their
items, and the tokens are one rANS stream.

## Measured, not kept

- Everything through aqr-flate's fast representation, no budget: 0.810 on CC at 29 / 55
  MB/s. It rebuilds at 60-270 MB/s of *inflated* data, and PDF streams inflate 2-12×.
- Measuring every FlateDecode stream at planning with a counting parse: 5 ns/B, half the
  budget; the dictionary and class priors do nearly as well for free.
- 11- and 10-bit probabilities for the token tables (smaller decoding tables): 0.2-1%
  larger, decoding only a little faster; it is latency-bound (each literal's table depends
  on the byte before), not cache-bound.
- Spending what the budget leaves on stronger zstd levels for the skeleton and pools
  (level 19 makes skeletons 10% smaller than level 9, at 27× the time): a few KB per set.

## Next steps

- JPEG: DCT data is 35% of CC, and only about half of it fits the budget. aqr-jpeg takes
  about 15 ns/B plus 250 µs (compressing) / 125 µs (decompressing) per image on PDF
  images, mostly per-file table building; default tables for small images would help.
- Encrypted files with a non-empty user password (the `/U` check fails: 1 of 19 encrypted
  files in CC), crypt filters named per stream, and equal plain streams (`DUP` of encrypted
  streams: their ciphertexts differ).
- `[/ASCII85Decode /FlateDecode]` and `[/FlateDecode /DCTDecode]` chains (2.8 and 28 MB in
  CC) are kept raw or in the skeleton.
- Token decoding is 11-15 ns/B; its order-1 contexts form a serial chain of table lookups.
- The skeleton is about 15% of the output; classic cross-reference tables could be rebuilt
  from object offsets (about 0.8 MB of the 18 MB on CC), object headers predicted.

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

## Testing

```sh
cargo test --release
../../target/release/aqr-pdf fuzz -n 5000 $(find corpus -name '*.pdf' -size -600k)
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
| `FORMAT.md` | The format, in full |
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
