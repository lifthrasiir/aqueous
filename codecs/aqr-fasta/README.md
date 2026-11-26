# aqr-fasta

FASTA files of nucleotide sequences rebuilt bit for bit: the bases A, C, G and T as symbols of
an adaptive order-9 context model coded by rANS, and apart from them the headers, the line
layout, the case (runs) and the other letters (N runs and IUPAC codes), which the archive
compresses with zstd. `FORMAT.md` specifies the module `fasta-recons` and how it works.

`aqr-fasta-bench FILE...` measures a file alone (with `PHASES` set, the time of the parse and of
the bases too).

## What is taken

- Files that start with `>`, have no CR, whose sequence lines are of letters, with runs of other
  letters at most one per 64 letters. Module: `fasta-recons` (version 3, a part module: its
  memory does not grow with the file, 9.5 MiB at most).
- Not taken: protein sequences, files with CR (left as they are).

## Results

Apple M4 (10 cores), 2026-10-26, hg19 (93 chromosome files of the 10gb corpus):

| | size | compress | decompress |
|---|---|---|---|
| chr1.fa alone (254 MB), `aqr-fasta-bench` | 51.7 MB (0.204) | 170 MB/s | 133 MB/s |
| hg19 through `aqr` (98 files, all threads) | 706.7 MB (0.218) | 108 MB/s | 753 MB/s |
| hg19 through `aqr`, zstd only | 971 MB | | |

Measured again on 2026-11-01 (`docs/file-formats.md`, 93 files, 3.2 GB): 0.209 of the input
(zstd -3 file by file 0.309).

Decoding is one serial chain per file, so the speed through `aqr` comes from the files decoded
at once. Compressing holds about 0.25 GB for chr1.

## Tests

`cargo rt -p aqr-fasta`.

## Left

- Nothing known.
