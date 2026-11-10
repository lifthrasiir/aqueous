# aqr-prores

Apple ProRes (SMPTE RDD 36; 422 Proxy to 4444 XQ, not ProRes RAW) recompression: a frame is
parsed into its headers and its slices' quantized coefficients, which a context model codes
again with the previous frame as context, and the frame is written again from them bit for
bit. The module `prores-recons` (`FORMAT.md`) rebuilds a chunk of a QuickTime or MXF file.

Frames delimit themselves (a big-endian size, then `icpf`), so `find` takes them out of any
container without reading it; aqr-compress takes `.mov` and `.mxf` files, and others that start
as QuickTime or MXF with a frame in their first 64 MiB.

Written from the specification's description of the bitstream; no decoder's code was read.
The test fixtures (`tests/data`, `scripts/gen-fixtures.sh`) are ffmpeg's test patterns through
its two encoders.

## What a frame holds

- The frame header, the pictures' headers and the slices' headers are kept as stored; the
  sizes in them (picture, slice table, components) are derived from the coded data.
- A slice's Y, Cb and Cr: DC coefficients (differences, the codebook picked by the previous
  one, the sign carried over), then AC coefficients as runs and levels interleaved across the
  slice's blocks; the codewords are Rice codes escaping to exp-Golomb. The coefficients
  determine the bits, but for whole zero bytes after the last codeword (recorded, none seen so
  far) and empty components (chroma of gray encodes).
- Alpha (4444 with alpha) is kept as stored, for now.
- Bytes after the pictures within the frame's size: zero stuffing in Apple's frames
  (64–128 bytes), kept.

## The formula (`src/formula.rs`, `src/codec.rs`, `src/module.rs`)

The frames are cut into chunks of about 64 MiB (a chunk ends also where more than 4 MiB lie
between frames). A chunk's span, from its first frame to its last, is an application of
`prores-recons`, whose inputs are the side stream (`prores/side`), the stripes' pieces
(`prores/coded`) and the bytes between its frames (audio, the container's headers); the bytes
outside the spans and between frames are one stream, `prores/rest`, in the file's order.

The coded chunk is pieces, a few for each frame, of adaptive binary arithmetic coding
(aqr-common's `arith`) and raw bits. The side stream has the frame's piece: the gaps' lengths,
the frames kept as they are (those that do not parse), the headers as differences from the
previous ones, the slices' headers, tails and alpha. The stripes' stream has, for each frame,
a piece for each stripe: the stripe of rows of every picture, with a model of its own and the
same stripe of the frame before as context. Every piece restarts the arithmetic coder (the
model's state carries on), which costs about four bytes a piece: on 120 frames of 1080p in 16
stripes, 106,220,346 bytes against 106,219,711 of the streams before, and 122,123,352 against
122,129,519 on 24 frames of UHD 4:4:4.

The module is a part module (`prores-recons` 3, as flac-recons): its instance reads the three
inputs in order through windows and writes the output through its window, and a gang of tasks,
one for each stripe, lives for the whole chunk, holding its stripe's model and planes of the
frame before. For each modelled frame the instance sends each task its message (the side piece
and the stripe's piece), the task decodes the headers of its slices from the side piece itself
and writes them (the slices' bytes, with their lengths), and the instance puts the frame
together from what the tasks send back and writes it with the gap before it. Three frames are
in flight (`module::DEPTH`: a channel holds as many messages, so no end waits for another),
(depths from 2 to 6 decoded alike on ten cores). A frame kept as it is waits for those before it and
goes through. Natively the same code runs through the emulation of aqr-api, the tasks on
threads (`Run::Part` of aqr-decompress): the memory that grows with the length is only the
application's result.

A chunk has a stripe for each 4 MiB of its frames, rounded up (at most 16), and no more than a
stripe for each 16 KiB of a frame (a frame of 4 KB, in the fixtures, takes one: the messages
cost more than the work otherwise; on 300 MB of those, 16 stripes took 3.8 MB/s to decode
and one 18). Splitting rows costs little spatial context (SD at 32 stripes, a slice row each,
lost 0.1%); what costs is each stripe's model learning anew, which shows where little is
coded: of 150 frames of each file in one chunk, by 1, 8, 16 and 32 stripes, Apple's animated
film 0.7347, 0.7414, 0.7457, 0.7516, while camera footage gained a little (ARRI 2K 0.5517,
0.5498, 0.5493, 0.5491) and decoding on 16 threads went 13, 71, 100, 108 MB/s. Stripes by size
keep their cost a small part of any chunk, and a large file's 64 MiB chunks decode on 16
threads. On noisy ffmpeg footage, 1080p 4:2:2 HQ and UHD 4:4:4 (120 and 24 frames, 115 MB and 138 MB, one chunk), by 1, 2, 4, 8
and 16 stripes the streams came to 106.23, 106.17, 106.15, 106.11 and 106.14 MB (UHD 121.91,
121.94, 121.94, 121.96, 122.01) and decoded at 12, 24, 45, 72 and 82 MB/s (UHD 13, 25, 48, 75,
88) on 10 cores.

The parameters give what decoding holds: the most of any frame, stripe or piece (blocks and
bytes of a frame, of the headers, slices of a stripe, bytes of a piece and of what a stripe
writes), exactly as the encoder found it; the decoder holds to them. The module lays its memory
out as a stack (`aqr_api::stack`), so its limit is its plan (`module::plan`): the instance's
memory (the windows' reads, a few frames' headers, a message, a frame and a result for each
stripe) and the gang's (a task for each stripe: the model, the planes of two frames of the
stripe, the slices' records, a message and a result; with the messages the channels hold).
None of it follows the chunk's or the file's length (`prores_plan_is_independent_of_length`,
`prores_plans_hold`).

## The model (`src/model.rs`)

One code path for both directions (`Coder`). Each picture's planes (`src/plane.rs`: blocks put
in place, in a macroblock TL, TR, BL, BR) are coded block by block in raster order:

- DC: predicted by the previous frame's DC, else the median of the left, above and gradient;
  the residual's size in unary, sign modelled, the rest raw.
- The number of nonzero AC coefficients (six bits, a tree), in the context of the
  neighbours' counts and the previous frame's.
- AC in ProRes's scan order until that many are found: a zero flag and the size (bit length,
  unary) in the context of the expected magnitude (twice the previous frame's coefficient;
  else the blocks above, left, above left and above right, and the coefficients above and left
  in the block itself), the scan band and the nonzeros left. The top two mantissa bits and the
  sign are modelled with the previous frame's coefficient; signs without one, and lower
  mantissa bits, are raw.
- A frame that does not parse is kept as it is. One that parses needs no check: every
  coefficient set has one coding, padding is zero and recorded, sizes are derived.

The contexts were chosen with `examples/model.rs` (static entropy of sampled frames by
context; environment variables pick the variants). Sampled from the corpus, against the coded
bytes: neighbours alone 0.850; median DC, frequency neighbours, mantissa bits 0.830; the
previous frame for AC sizes 0.793, for DC 0.775, for signs and mantissa 0.738. Coding the
difference from the previous frame instead helps static footage and hurts noisy footage, about
even in all. The other block placement and a raster scan cost more, which confirms both.

## Status

Tools (`cargo run -r -p aqr-prores --example ...`):

- `aqr-prores-check [--zstd] FILE_OR_DIR...`: finds the frames and checks that each is written
  again bit for bit (with `--zstd`, zstd -3 of the frames and of their parse).
- `aqr-prores-bench [-c CHUNK_MB] [-n FRAMES] [-k STRIPES] [-r DECODINGS] FILE_OR_DIR...`: codes the frames
  in chunks and decodes them (through the module, the stripes on threads).
- `aqr-prores-model [-n FRAMES] FILE_OR_DIR...`: the context estimator above.
- `aqr-prores-decisions [-n FRAMES] FILE_OR_DIR...`: the model's binary decisions by kind (zero
  flags, the size's first, second and later decisions, mantissa bits, signs, counts, DC) with
  the bytes each would cost, over the first frames.

The local corpus is `F:\Data\aqr-prores-corpus` (see its README: Apple, ARRI, FFmpeg's
`prores_ks` and `prores_aw`, 2K to UHD, MOV and MXF, interlaced, alpha; 16.4 GB). Every frame
of it is written again bit for bit; zstd -3 takes the frames to 0.9902.

Measured 2026-11-01 on a Ryzen 7 9700X (8 cores, 16 threads), `cargo aqr -c --stats` of the
corpus directory (33 files, 16.89 GB) at level mid, against the build before aqr-prores
(`cache/aqr-corpus.txt`):

| | before | after |
|---|---|---|
| archived | 16.72 GB (0.990) | 11.42 GB (0.676) |
| compressed in | 39 s | 371 s (46 MB/s; the largest file, 5.8 GB, 113 s on 12 threads) |
| tested (`aqr -K`, then `-t`) | | 130 s (130 MB/s; the largest file 61 s on 10.4 threads) |
| heap peak | 17.5 GB | 20.5 GB compressing, 11.9 GB testing |

Compressing, aqr-compress decodes every application to check it (prevalidate): of its 3,250
thread-seconds, 1,720 decode and 1,230 encode (`cache/aqr-corpus-stripes.txt`). With 8 stripes
and 32 MiB chunks it was 11.46 GB, tested in 153 s.

The peaks are the archiver's: it holds the largest files whole (17.5 GB before as well), and a
leaf's output is its file. By file, from the bench (`examples/bench.rs`, 32 MiB chunks): tripod
ARRI 2K 422 HQ 0.54–0.55, ARRI 4444 0.67–0.83, Apple's animated film 0.63, the SD tape capture
0.82, ffmpeg's encodes 0.35–0.73 of their frames. A chunk codes at about 15 MB/s on a thread
and decodes at about 13 (about 100 MB/s with 16 stripes at once).

aqr-compress does not weigh a ProRes formula against zstd of the file when the file is 16 MiB
or more (`PRORES_UNCOMPARED`). A smaller one still is: with it skipped, FATE's 8 files (1.2 MB,
nearly blank frames) archived to 377.6 KB in place of 319.5 KB, zstd taking them under the
formula's modules. Measured 2026-11-10 on the same machine, `cargo aqr -c --stats
--application-bound 512M` of `fate`, `ffmpeg`, `arri-alexa` and `arri-amira` (30 files, 6.49 GB),
before and after: 4,503,182,837 bytes both, the archives identical byte for byte; 148.6 s and
146.8 s wall (the weighing of the largest file, one thread at the end, is within the noise).

The module as a part module (version 3) against version 2, measured 2026-11-10 on an M4 (10
cores), `cargo aqr -c` and `-K` (the files are ffmpeg's: `hd` 120 frames of noisy 1080p 4:2:2
HQ, 115 MB; `uhd` 24 frames of noisy UHD 4:4:4, 138 MB; `rep` the fixture `aw-422hq.mov` 2,500
times, 298 MB of 4 KB frames, which the stripe rule gives one stripe), with the application bound
raised (`--application-bound 512M`, `1G` for `rep`) so that version 2 is used at all:

| | hd 2 → 3 | uhd 2 → 3 | rep 2 → 3 |
|---|---|---|---|
| archive, bytes | 106,234,567 → 106,214,021 | 122,143,709 → 122,123,577 | 24,872,647 → 11,948,327 |
| application's memory (`-I -v`, the largest) | 404 MB → 70 MB | 410 MB → 220 MB | 582 MB → 6.9 MB |
| compressing, wall | 10.5 s → 6.1 s | 10.7 s → 6.6 s | 6.3 s → 6.9 s |
| compressing, peak resident | 748 → 764 MB | 1036 → 1018 MB | 1057 → 1019 MB |
| `-K`, wall (native; threads busy) | 5.9 s (1.7) → 1.6 s (8.7) | 5.7 s (1.9) → 1.8 s (8.9) | 3.4 s (2.5) → 3.9 s (2.5) |
| `-K`, peak resident | 412 → 352 MB | 479 → 648 MB | 617 → 437 MB |
| `-K --wasm`, wall | 1.85 s → 1.83 s | 2.1 s → 2.1 s | 2.2 s → 4.1 s |
| `-K --wasm`, peak resident | 555 → 334 MB | 579 → 320 MB | 930 → 474 MB |

(The archive of `rep` is smaller because pieces of repeated frames repeat bytes, which the
archive's deduplication finds; it is no measure of coding. Version 2's `rep` used 16 stripes of
4 KB frames; at 16 version 3 decodes it at a fifth of the speed of one stripe, hence the rule.)
The module is 44,009 bytes (14,610 of its block) against 40,306 (13,292).

The plan follows the frames, not the chunk: a chunk of 64 MiB took up to 389 MiB of the
application's memory as version 2, over the compressing bound of 256 MiB, so the corpus's files
over 64 MiB (14 of 30) were kept as they were. Measured 2026-11-10 on the Ryzen 7 9700X (16
threads), `cargo aqr -c --stats` of the same 30 files (6.49 GB) at `1866ff4f`:

| | version 2 | version 3 |
|---|---|---|
| default bound (256 MiB) | 6.14 GB (94.6%) | 4,505,195,603 bytes (69.4%), 137.2 s |
| `--application-bound 512M` | 4,503,182,837 bytes, 146.8 s | the same as at the default, byte for byte; 137.7 s |
| `-K` of the archive (native / `--wasm`) | | 58.8 s (110 MB/s) / 80.9 s (80 MB/s) |
| heap peak, resident peak (compressing) | | 3.04 GB, 5.16 GB |

No file drops prores-recons for the bound. The pool looks idle for long stretches when testing
(one of 16 workers busy for 22-49 s natively, 30-69 s in Wasm), and two ARRI files (A006, A007)
seemed to take 1.0 thread on average as they were tested. That was `--stats` counting the pool's
threads alone: the part module and its gang run on threads outside it. A006 tested alone natively
took 11.2 s at about 13.4 threads by process CPU time, A007 16.1 s at about 13.6 (`--stats` now
counts those threads too). `archive-org` (2 files, 10.41 GB)
at the default bound: 7,691,455,732 bytes (73.9%) in 235.6 s, the 5.82 GB file in 99.3 s on 9.1
threads.

Speed of the model, measured 2026-11-10 on an M4 (10 cores) with a local set made with ffmpeg
(10 files, 1.2 GB: `prores_ks` and `prores_aw`, 422 proxy to 4444 with alpha, 1080p and UHD,
test patterns, fractals and noise; synthetic, no camera footage), against the commit before
(`1866ff4f`'s model). Decisions per input byte of the first 20 frames, 313 MB: 7.67 -> 7.50
(zero flags 1.66, the size's 3.05, mantissa 1.35, signs 0.85, counts 0.46 -> 0.29, DC 0.30); the
estimated ratio 0.7902 -> 0.7900 and the bench's, whole files, 0.7785 -> 0.7782, no source worse
(one thread, one stripe, `aqr-prores-bench -k 1 -n 20 -r 3`, decoding MB/s of the module's code
-> after): mandel_aw_hq (HQ noise) 13.3 -> 17.5, life_aw_lt (LT) 14.5 -> 20.1, pan_ks_hq (sparse)
12.7 -> 16.5, testsrc_ks_hq 18.1 -> 28.4; cycles of the run (an encoding and 3 decodings) -19% to -30%. The whole set at the bench's
default (chunks of 32 MiB, the stripes on threads): decoding 22.0 -> 29.8 MB/s of a chunk's wall
time; encoding unchanged (15.6), the same model code but the parse of ProRes bits (a fifth of
it) and the arithmetic coder's own work not touched. `cargo aqr -c`/`-K` of mandel_aw_hq (121
MB) and testsrc_uhd_hq (84 MB): compressing 5.7 -> 5.4 s and 4.3 -> 3.8 s, `-K` natively 1.43 ->
1.27 s and 1.10 -> 0.87 s (threads' CPU time 12.3 -> 9.9 and 7.0 -> 4.7 s), `-K --wasm` 1.65 ->
1.62 s and 1.17 -> 1.13 s (CPU 14.0 -> 13.0 and 7.5 -> 6.4 s). The module is 45,159 bytes
(15,222 of its block) against 44,009 (14,610).

What the time was: decoding is bound by the latency chain of each decision (the adaptive bit's
load, the multiplication of the range, the comparison, whose result is a branch) and its
mispredictions, not by cache (a model cut to 16k contexts ran as fast) or by instructions
(unchecked indexing took 8% of them for 2% of the cycles). So what helped was fewer branches on
hard-to-predict bits (mantissa and sign bits by selects), the slices written from the nonzero
positions (their writing was a quarter of decoding, now 13%) and a cheaper bit update. Tried
and dropped, on the set above (decisions or ratio, one thread):

- Zero flags in spans of positions with nothing around them coded as one decision per span:
  decisions -3% (proxy) to -8% (pan), ratio +0.65% and +0.2%; the spans are short.
- Raw second mantissa bit where the previous frame gives no hint (or always): decisions -5% (-7%),
  ratio +0.15% (+0.22%, +1.5% on the static pan), and only 1-3% of the time (those bits are
  branchless already).
- All zero flags of a block first, then the sizes, signs and mantissas, the flags coded with no
  branch on a result (their context the neighbours' flags, not magnitudes: ratio the same, 0.7900):
  decoding 6-13% slower (the chain of 25 cycles a flag against 22 with branches), encoding the same.
- One window for a coefficient's run, level and sign in the ProRes parse, by selects: 2-3% of
  encoding for more code.

Left:

- Speed, decoding first (it is more than half of compressing too): writing the slices is about
  a quarter of decoding; about 8–9 binary decisions per input byte, mostly zero flags (3.8 a byte) and sizes
  (1.8); the arithmetic coder itself is about 15% of the encoding time, the rest the decisions'
  contexts and branches (those figures are from the corpus; the local set has more sizes and
  fewer zero flags). Sizes binarised around their expected value would save decisions on dense
  footage (not tried: it widens the size table); the corpus is to say whether the changes of the
  commits after `1866ff4f` keep the ratio, and where the time goes there.
- Memory: 8K 4:4:4 frames would pass the compressing bound of 256 MiB (a stripe's planes are
  258 bytes a block of its picture, in two frames; UHD 4:4:4 takes 220 MB). Native decoding runs
  all the gang's tasks at once, so it holds the plan (UHD 4:4:4: 648 MB resident, against 479
  for version 2), and the chunk's span as its result.
- The chunk's size (`aqr-prores-bench -c`, 120 frames of 1080p repeated nine times, 1.04 GB,
  10 cores; the chunk is the unit of the encoder's work, so coding takes one thread a chunk):

  | chunk | streams | encoding and decoding, wall | decoding one chunk |
  |---|---|---|---|
  | 64 MiB | 955,914,677 | 24.8 s | |
  | 256 MiB | 954,581,967 (−0.14%) | 32.8 s | |
  | 1 GiB (the file) | 954,124,586 (−0.19%) | 78.7 s | 78 MB/s |

  The default stays at 64 MiB. A bigger chunk gains little on noise (the model learns in the first
  frames, and there is more to learn in the corpus's footage: to be measured there) and loses the
  chunks' parallelism in encoding, which is a third of compressing, and its memory (the encoder
  holds the chunk's frames); decoding does not need it any more.
- Alpha: its run-length coding parsed and modelled (0.3 MB of the corpus).
- Apple's own encoders as a sample: AVFoundation (`avconvert`, `prores_videotoolbox`) and iPhone
  recordings; the corpus has one Apple-encoded film.
- MP4 or Matroska holding ProRes are not taken (`.mp4` stays compressed data; Matroska goes to
  aqr-mkv).
