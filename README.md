# pdf-recons (`pdfr`)

Lossless PDF recompression: a PDF file is stored smaller, and restored bit for bit. Meant as
a component of a general-purpose compressor, so speed comes first: the target is 100 MB/s
of PDF file single-threaded on an Apple M4, both ways. Compression ratio is pursued only
within that time. DEFLATE streams go through [flate-recons](../flate-recons) (parsing,
writing, Huffman header coding, its fast representation), images through
[png-recons](../png-recons) and [jpeg-recons](../jpeg-recons).

```sh
cargo build --release
pdfr c [-t N] [-s MB/S] [-l LEVEL] [-V] [-T] [-F] [-P] in.pdf out.pdfr   # compress
pdfr d [-t N] out.pdfr back.pdf                                           # decompress
pdfr bench [-t N] [-n ITERS] [-s MB/S] [-v] ... FILE...   # round trip, ratio, CPU-time speed, per-mode costs
pdfr fuzz [-n ROUNDS] [-r SEED] FILE...            # mutated files and containers
```

```rust
let packed = pdf_recons::compress(&pdf);              // never fails: stores what it cannot code
assert_eq!(pdf_recons::decompress(&packed)?, pdf);    // checked against an xxh3 of the original
```

`-t` is the number of threads (`c` and `d`: the available cores; `bench`: 1, timed in CPU
time with one thread and wall time with more), `-s` is the speed target (`inf`: no limit), `-V` verifies by decompressing (and on a mismatch
codes the file again without models), `-T`/`-F`/`-P` turn off the token, full and PNG modes.
Any input is accepted; what is not a PDF goes through as skeleton.

## Results

Apple M4, one thread, process CPU time, best of 2 (`pdfr bench -n 2`); MB/s of PDF file,
both ways; output size / input size. Every file round-trips. Generic compressors on each
file, summed (`scripts/baseline.sh`): zstd 1.5.7 `--single-thread`, xz `-9 -T1`.

| set | files | size | **pdfr** | compress | decompress | zstd -3 | zstd -19 | xz -9 |
|---|---|---|---|---|---|---|---|---|
| home (code charts, papers) | 20 | 314 MB | **0.8738** | 116 MB/s | 172 MB/s | 0.9145 | 0.8900 | 0.8879 |
| cc (Common Crawl) | 1,000 | 1,379 MB | **0.8638** | 157 MB/s | 186 MB/s | 0.9073 | 0.8879 | 0.8887 |
| arxiv | 20 | 94 MB | **0.8346** | 124 MB/s | 142 MB/s | 0.9021 | 0.8831 | 0.8828 |

The speed target binds: everything coded costs more per byte than the budget allows on
average, so the ratio is what fits in it. With no limit (`-s inf`) CC gives **0.7713** at
35 / 57 MB/s. The target is met per file, not just per set: the budget is per file
(below) and planned with a 10% margin for the scatter of the estimates. Files below
100 MB/s hold 2.2% of CC's bytes compressing (75 files, most of them small, where fixed
costs dominate) and 0.01% decompressing, 0.05% of home's and none of arXiv's. Without the
margin (`-s 91`) CC gives 0.8582 with 10.5% of its bytes in slow files.

What each mode does on CC at the default target (ns per byte of stream, compressing /
decompressing; output / input):

| mode | streams | bytes | compress | decompress | ratio |
|---|---|---|---|---|---|
| tok, pixels (images without predictor) | 2,714 | 111 MB | 14.5 | 13.3 | 0.840 |
| tok, content streams | 3,223 | 30 MB | 15.0 | 13.5 | 0.951 |
| png (PNG-predicted images, png-recons) | 51 | 80 MB | 13.1 | 12.8 | 0.787 |
| jpeg (jpeg-recons) | 1,610 | 250 MB | 15.5 | 14.7 | about 0.80 |
| full, fonts (flate-recons fast) | 758 | 11 MB | 35.3 | 25.1 | 0.864 |
| full, content streams | 638 | 1.5 MB | 79.4 | 52.0 | 0.885 |
| dup | 7,627 | 30 MB | 0 | 0 | 0 |
| raw (not worth its time) | 61,582 | 741 MB | 0.1 | 0 | 1 |

(Token mode's times include its share of the per-file final pass and decoder setup.)

## How

### The budget

Everything that pays costs about the budget or more: inflating a DEFLATE stream alone runs
at about 200 MB/s of DEFLATE data (5 ns/B, half the 10 ns/B the target allows), and the
coders measured here take 10-115 ns per byte of stream (below). So the compressor does not
code everything it can; it plans:

- Every stream gets the modes it allows, each with an estimated size and time: a cost
  model fitted to per-item measurements of the corpus (`PDFR_CALIB=FILE pdfr bench`, then
  `scripts/fit-costs.py`; ns per item, per stream byte and per inflated byte, each way;
  for JPEG, per 8×8 block from the frame header instead, which halves the error).
  Inflated sizes come from the dictionary (images' geometry, fonts' `/Length1`), else
  from the first DEFLATE block's code lengths (expected bytes per bit with probabilities
  2^-length: a third of the class prior's error on content streams), else a per-class
  prior; planning inflates nothing.
- The mode of each stream minimises *size + λ·time*, with the smallest λ that keeps the
  file's total time (the larger of compression and decompression per item) within its
  budget, `file size / (1.1 × target speed)`. The estimates are within a few percent of
  measured times over each set and scatter by about 10% per file (content the features
  do not see: flate-recons on some encoders' streams, JPEG entropy), hence the margin.
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

The file is cut into the skeleton (zstd) and *items*:

| mode | what | coder |
|---|---|---|
| raw | stream data kept as it is (incompressible, or not worth its time) | none |
| dup | a stream equal to an earlier one | reference |
| tok | FlateDecode: the DEFLATE tokens recoded (`tok.rs`) | own, rANS |
| full | FlateDecode: inflated data (pooled per class, zstd) + flate-recons' fast metadata | flate-recons |
| png | FlateDecode image with PNG predictors, wrapped as a PNG file | png-recons |
| jpeg | DCTDecode | jpeg-recons |

Uncompressed and ASCII-encoded streams stay in the skeleton. zlib headers are kept, the
Adler-32 recomputed, anything after the DEFLATE data left in the skeleton. Files with an
`/Encrypt` entry keep their streams raw (they look random).

### Token mode (`tok.rs`, `rans.rs`)

The stream's own LZ77 parse is kept, so rebuilding needs no match search, only Huffman
coding (the png-recons approach, for any stream):

- Literals and length codes share one alphabet; the table is chosen by the byte before
  (per class and file, at the resolution that costs least: every byte, 32 classes of
  bytes, 8, or one table). Distance codes have a table per length code; extra bits are raw.
- Images without a predictor are coded as pixels: a literal is the residual against MED
  (left, up, up-left), plus the previous channel's residual for every channel but the
  first, with tables by channel, local activity and whether a match came just before.
- Block layout, stored lengths and padding, and length-258 matches sent as 284+31 go to a
  layout stream; dynamic Huffman headers through flate-recons' header coder against the
  block's frequencies (zlib's own trees cost a bit).
- Static per-file tables (quantised log frequencies), 12-bit probabilities, four
  interleaved rANS states (after png-recons' coder, with alphabets beyond 256).
- All token streams of a file share one rANS stream, decoded in file order.

### Format

```text
"PDFR" version mode
mode 0: the file as it is
mode 1: varint(length) u64-LE(xxh3) | 13 sections: varint(raw length) u8(0 stored | 1 zstd) [varint(coded length)] data
  table (zstd): per item varint(skeleton gap) u8(mode) fields
  skeleton (zstd), raw, flate-recons metadata, jpeg-recons, token streams, png-recons,
  inflated data of full-mode streams per class (zstd)
```

`container.rs` documents the fields of each mode.

### Threads

`-t N` (`Options::threads`, `decompress_threads`, `decompress_with_threads`): the output is
the same bytes whatever the number of threads, and one thread runs the code as before
(jpeg-recons on rayon's global pool; `pdfr c -t 1` and `d -t 1` make that pool one
thread). With more, a pool of N threads codes the items ahead, in
order, at most 4N + 4 of them and about 256 MB of estimated working memory in flight
(always one): JPEG, PNG and flate-recons items whole, and FlateDecode streams for the token
mode parsed; the calling thread appends them to the sections in order and runs the token
coder, whose state carries over from stream to stream. jpeg-recons' own parallelism runs on
the same pool, so it takes N threads in all (flate-recons, one thread per item). The token
coder's final pass runs beside zstd of the table, skeleton and pools. Decompressing, the
sections are decoded on the pool, the table is read first, and the pool restores the
full-mode, PNG and JPEG items ahead while the calling thread decodes the token streams (one
rANS stream) and joins everything in order; the xxh3 check is at the end, as with one thread.

## Measured, not kept

- Everything through flate-recons' fast representation, no budget: 0.810 on CC at 29 / 55
  MB/s. It rebuilds at 60-270 MB/s of *inflated* data, and PDF streams inflate 2-12×.
- Measuring every FlateDecode stream at planning with a counting parse: 5 ns/B, half the
  budget; the dictionary and class priors do nearly as well for free.
- 11- and 10-bit probabilities for the token tables (smaller decoding tables): 0.2-1%
  larger, decoding only a little faster; it is latency-bound (each literal's table depends
  on the byte before), not cache-bound.
- Spending what the budget leaves on stronger zstd levels for the skeleton and pools
  (level 19 makes skeletons 10% smaller than level 9, at 27× the time): a few KB per set.

## Next steps

- JPEG: DCT data is 35% of CC, and only about half of it fits the budget. jpeg-recons takes
  about 15 ns/B plus 250 µs (compressing) / 125 µs (decompressing) per image on PDF
  images, mostly per-file table building; default tables for small images would help.
- Encrypted files (a few percent of CC): with the empty user password their streams can be
  decrypted, recoded and encrypted again exactly (RC4 and AES-CBC with the stored IV).
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
  producer), and `corpus/arxiv/`, 20 arXiv papers (94 MB).
- `scripts/baseline.sh SET...`: zstd and xz on each file, cached in `cache/`.

What CC holds (`examples/survey.rs`): DCT images 35%, FlateDecode images 29% (11.8×
inflated), fonts 12% (2.2×), content streams 10% (4.5×), JPX/CCITT/JBIG2 images 6%,
skeleton 4%.

## Testing

```sh
cargo test --release
target/release/pdfr fuzz -n 5000 $(find corpus -name '*.pdf' -size -600k)
```

- `tests/roundtrip.rs`: FlateDecode streams from every encoder of flate-zoo, through every
  mode; zlib wrapper variants (bare DEFLATE, wrong Adler-32, junk after the data,
  truncated and damaged streams); syntax (indirect and wrong lengths, CR line ends,
  keywords in strings, incremental updates, unterminated streams); images with and without
  predictors, geometry that does not match the data; JPEG; duplicates; encrypted files;
  non-PDF and cut files; 2,400 corrupted containers (errors, never panics).
- Unit tests: lexer, scanner, rANS and bit I/O, token coding.
- `pdfr fuzz`: mutated files (bit flips, cuts, deletions, junk, copied pieces, bytes inside
  streams) must round-trip under every set of modes with the safety nets off; corrupted
  containers must fail cleanly; with 2 to 4 threads, the same container and the same
  outcome.
- With 2, 3 and 8 threads, every test's container is the same bytes, and decompression (of
  corrupted containers too) has the same outcome.
- Every file of the three sets round-trips (`pdfr bench` checks).

## Layout

| File | Contents |
|---|---|
| `src/container.rs` | Format, the cost model and planner, compression, decompression |
| `src/scan.rs`, `src/lex.rs` | Finding streams, PDF objects |
| `src/kind.rs`, `src/objects.rs` | Stream classes and filters, indirect objects |
| `src/flate.rs`, `src/image.rs` | zlib wrapper; image geometry and the PNG wrapper |
| `src/tok.rs`, `src/rans.rs` | Token mode; static rANS (`recons_common::rans32` with 12-bit probabilities) |
| `src/bin/pdfr.rs` | Command-line tool, bench, fuzz |
| `examples/` | Corpus surveys and experiments behind the choices here |
| `scripts/` | Corpus, baselines, cost fitting, profiling (`profile.py` over samply) |
