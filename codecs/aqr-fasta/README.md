# aqr-fasta

FASTA files of nucleotide sequences rebuilt bit for bit (`FORMAT.md`): the bases A, C, G and T
as symbols of an adaptive order-9 context model coded by rANS, and apart from them the headers,
the line layout, the case (runs) and the other letters (N runs and IUPAC codes, runs), which
the archive compresses with zstd.

## How it does

Measured 2026-10-26 on an Apple M4 (10 cores), on hg19 (93 chromosome files of the 10gb corpus,
3.24 GB with the directory's other files).

| | size | compress | decompress |
|---|---|---|---|
| chr1.fa alone (254 MB), `aqr-fasta-bench` | 51.7 MB (0.204) | 170 MB/s | 133 MB/s |
| hg19 through `aqr` (98 files, all threads) | 706.7 MB (0.218) | 108 MB/s | 753 MB/s |
| hg19 through `aqr` before (zstd only) | 971 MB | | |

- Decoding is one serial chain per file (each base's context is the bases before it), so the
  speed through `aqr` comes from the files decoded at once. What bounds it is memory: a model of
  4^10 contexts (8 MiB) codes 1% smaller than 4^9 (2 MiB), but ten of them at once miss the
  caches, 19 Mbase/s a thread against 71 (chr1, ten decoders at once; an M4). The decoder loads
  the contexts two bases ahead as it decodes one, which brings 58 to 71 there: 128 bytes,
  aligned, asked for as two halves of 64 bytes. One hint for all 128 bytes was slower on both
  machines measured (`scripts/cache-ab.sh`, decode MB/s, the faster of three interleaved runs):
  chr1 on the M4 132 -> 162 alone and 91 -> 95 ten at once (54.3 -> 57.7 G instructions, 16.4
  -> 14.9 G cycles); a synthetic 61 MB file on a 4-core Xeon 61 -> 75 alone, 61 -> 80 four at
  once (glibc's large blocks start 16 bytes into a page, so the hint caught at most 6 contexts).
- The model was picked on chr1 (bits per base): order-9 1.806, order-10 1.787, order-11 1.786;
  probabilities moving by 1/64 against 1/16 (1.827) and 1/128 (1.795) at order 10; counts
  halved at 250 (1.790, and a division per base). A match model of 24-base hashes (last
  occurrence, its prediction weighed by an adaptive table of its length) brought 1.787 to
  1.773 on 60 MB at four times the time, not taken. Models started anew every 64 Mbase cost
  1.6%, every 16 Mbase 4.0%: a file is one stream, cut (FORMAT.md §7) only past 256 MiB.
- The module is a part module (version 3): its five inputs are read and its output written in
  order through windows, in pieces of 64 KiB, and the bases are decoded as the file is built.
  Nothing of the file's length is held: an application's memory plan is 3.4 MiB with the module's
  initial memory (the model's 2 MiB, six pieces), 9.5 MiB at most with the windows, whatever the
  length. Version 2 held its output, a copy of
  it, its inputs twice and the model, 2.5 times the file (588 MiB for chr1), and cut a file at
  96 MiB into segments (chr1: three, +0.45%, 51.97 MB against 51.73 MB), so as to fit the
  application bound of 256 MiB (245 MiB at most, random bases). chr1 decodes natively in 18.3
  G instructions against 21.3, resident 115 MB against 368 (Wasm 363 MB against 532, 2.2 s
  of user time against 2.7); the module block grew 687 B. Held, the bases took as much again
  (10gb decompression 8.66 → 7.49 GB of peak footprint, ten chromosomes decoded at once).
- Encoding holds the bases packed four to a byte and codes them 8 Mbase at a time, from the
  last chunk, the model kept at the start of each (2 MiB a chunk): chr1 takes about 0.25 GB
  where it took 1.3 GB (the lines listed, a byte a base, and the start and frequency of every
  base, 4 bytes each). The model runs twice over all but the last chunk, the second time for
  the chunk before on a worker while this one is coded, where one is free: chr1 parses in
  0.32 s (0.41 before) and codes in 0.98 s on two threads (1.38 on one; 1.17 before, one).
- The archive's batches (512 MB) hold few chromosomes each, which keeps compression to two
  or three cores; the module itself encodes at 194 MB/s (chr1 and chr10, two threads where
  free; 170 before, one thread).

## Tests

`cargo rt -p aqr-fasta`; `aqr-fasta-bench FILE...` (with `PHASES` set, the time of
the parse and of the bases too).
