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
  caches, 19 Mbase/s a thread against 71 (chr1, ten decoders at once). The decoder loads the
  line of the contexts two bases ahead as it decodes one, which brings 58 to 71.
- The model was picked on chr1 (bits per base): order-9 1.806, order-10 1.787, order-11 1.786;
  probabilities moving by 1/64 against 1/16 (1.827) and 1/128 (1.795) at order 10; counts
  halved at 250 (1.790, and a division per base). A match model of 24-base hashes (last
  occurrence, its prediction weighed by an adaptive table of its length) brought 1.787 to
  1.773 on 60 MB at four times the time, not taken. Models started anew every 64 Mbase cost
  1.6%, every 16 Mbase 4.0%: a file is one stream.
- The bases are decoded as the file is built, never held: a file decodes in its own size and
  the coded bases. Held, they took as much again (10gb decompression 8.66 → 7.49 GB of peak
  footprint, ten chromosomes decoded at once).
- The archive's batches (512 MB) hold few chromosomes each, which keeps compression to two
  or three cores; the module itself encodes at 170 MB/s a thread.

## Tests

`cargo test --release -p aqr-fasta`; `aqr-fasta-bench FILE...` (with `PHASES` set, the time of
the parse and of the bases too).
