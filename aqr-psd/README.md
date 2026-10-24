# aqr-psd

Lossless PSD/PSB recompression: a Photoshop file is coded as a formula (the module
`psd-recons` of aqr-formula) of a smaller representation, and restored bit for bit.

```sh
cargo build --release
aqr-psd bench [-n ITERS] [-t N] [-v] [-c|-d] FILE...  # .aqr round trip check, ratio, throughput (-t default 1)
aqr-psd-stat [-v] FILE...                    # byte breakdown by section, summed over files
aqr-psd-research check|predict|rle FILE...          # research tools: decoding, composite prediction, PackBits encoders
cargo test                              # formula round trips of synthetic PSDs (no corpus needed)
```

`PSDR_STATS=1` prints phase times and sizes; `PSDR_DEBUG=1` where decoding found corrupt
input. `examples/` holds profiling aids (`modelbench`, `renderbench`, `pbbench`, `recbench`) and
debugging ones (`chan`, `diff`, `rlediff`), run with `cargo run --release --example aqr-psd-NAME -- FILE`.

## How

- Layer channels and the composite image (8-bit, PackBits or raw) are decoded and coded with
  a pixel model (MED prediction with alpha-aware neighbors, contexts, runs) and static-table
  rANS. Color planes are coded pixel by pixel so that their dependency chains overlap.
- PackBits is rebuilt bit for bit: Photoshop's encoder is identified per channel (it cuts rows
  at its image cache tiles, then into 128-byte chunks); rows no known encoder reproduces are
  stored verbatim.
- The composite is predicted by rendering the layers (blend modes, masks, clipping, groups),
  when a sample of rows shows that it pays: where pixels match, runs of them cost next to
  nothing.
- Channels repeating an earlier one are copies; files embedded in smart objects are coded as
  formulas of their own, recursively (PSD/PSB), or by aqr-png and aqr-jpeg.
- The rest of the file, the skeleton, is kept as it is: whatever holds the formula compresses
  it.
- The format is specified in [FORMAT.md](FORMAT.md).
- Both directions stream: pixel data is coded row by row from the input and decoded row by
  row into the output (allocated at its final size), so the memory besides input, output and
  the streams stays small whatever the file size (embedded PNG and JPEG files take what their
  recompressors need).

## Threads

`-t N` (the `threads` of `formula::encode` and `formula::decode`): the output is the same
bytes whatever the number of threads, and one thread runs the serial code unchanged. The model
state is per group but all groups share one rANS stream, so the model coding and decoding stay
on one thread, in order; around it:

- Encoding: spans are planned (RLE checks, tile inference, encoder ranking) in parallel and
  then added in order; embedded files are recompressed, and the composite is planned and its
  hint sampled, at the same time; for groups of at least 256K samples a thread reads the rows,
  checks them against the encoders and renders the hint ahead (in batches of about 1 MB)
  while the first one codes them; encoders are settled per span in parallel.
- Decoding: in groups of at least 256K samples one thread reads the known planes and renders
  the hint ahead, and another re-encodes the PackBits rows behind the model decoder.
- aqr-jpeg uses rayon: for more than one thread aqr-psd asks it for that many, for one it
  keeps the global pool. aqr-png gets the threads too. A nested PSD gets one thread when
  several embedded files run at once.
- `PSDR_STATS` times are summed over threads.

Memory: batches in flight add at most about 10 MB per helper pair (5 batches of about 1 MB,
twice), and fewer rows than the group has.

## Results

Measured on 2026-10-24 on an Apple M4, one thread, through `aqr-show` (see the
[top-level README](../README.md#how-it-was-measured)): each file archived at level `mid` with
its round-trip check, then extracted. Ratios are tentative, since the decoder modules are not
in the archives yet. Memory is the peak RSS of that process (input, archive and output
included) over the input size.

| set | files | MB | ratio | zstd -3 | compress | decompress |
|---|---|---|---|---|---|---|
| genposter | 5 | 198.1 | 0.393 | 0.693 | 37.0 MB/s | 97.9 MB/s |
| oga | 37 | 214.7 | 0.284 | 0.457 | 58.5 MB/s | 176.8 MB/s |
| fixtures | 13 | 16.1 | 0.230 | 0.284 | 40.3 MB/s | 148.6 MB/s |
| tori29umai | 1 | 10.3 | 0.215 | 0.383 | 40.2 MB/s | 87.5 MB/s |
| all | 56 | 439 | **0.330** | 0.555 | **45 MB/s** | **127 MB/s** |

8 files are kept as they are. Memory is about 28 MB + 5.4× the file (a least-squares fit; the
largest above it, 372 MB for a 22.7 MB file with embedded JPEGs, recompressed by aqr-jpeg),
against 32 MB + 5.2× and up to 98 MB for a 0.58 MB file before files kept raw were scanned by
their parts. Little of it is aqr-psd's: on its own (`formula::encode` and evaluating the
formula) it takes the streams and at most a few MB besides, plus what aqr-png and aqr-jpeg take
for embedded files (up to 96 MB, for progressive JPEGs), and decoding holds the embedded files'
bytes twice (the module's input and its output). The rest is the archive's: its verification,
and for files kept raw, the files found in them. These are looked for by the parts
`formula::regions` tells (pixel data, structure, text and embedded files), not from every byte,
where MP3 frames were found in pixel data.

## Limitations

- Pixel data is coded only in 8-bit documents (RLE or raw channels); 16- and 32-bit data,
  ZIP channels and 1-bit images stay in the skeleton. Color groups are RGB(A) only:
  other modes code each plane on its own.
- Embedded PDF and other non-PSD/PNG/JPEG smart object files stay in the skeleton.
- Rendering supports normal and the common blend modes, opacity, fill, user masks, clipping and
  groups; layer effects, adjustment layers and dissolve are not rendered (the hint is then
  weaker, or dropped when a sample of rows predicts less than half the pixels).
- A file whose structure cannot be parsed, or with an RLE row that does not decode, gets no
  formula (`formula::encode` gives `None`).
- Embedded PNG and JPEG files are recompressed in memory by their own recompressors.

## Speed work (2026-10-08)

From the first streaming version to the last results measured with the former file format
(subsets, single thread): compression
22-35 -> 83-123 MB/s, decompression 30-53 -> 103-142 MB/s. What helped, roughly by gain:

- Planning on samples: PackBits rows are checked and the tile cut inferred on every 8th row,
  candidate encoders ranked on 32 rows; checking a row against an encoder compares token
  headers only (nothing is written).
- Runs: shorter (4 equal samples above) with the stretch above as context; runs of pixels
  equal to the composite hint; the run check reads the (known) row above first.
- Color planes coded pixel by pixel; second and third planes jointly when small.
- The composite hint only when a sample of rows shows that it pays (it does on GenPoster and
  OpenGameArt, -19% and -29% in size, not on tori29umai, where it cost a third of the
  decompression time for -4%).
- Renderer: 8-bit integer canvases row by row, blend functions chosen per layer, rows without
  coverage skipped, clipping bases only where clipped layers follow.
- 8-byte compares in
  PackBits encoders; 11-bit rANS probabilities with per-chunk compacted decoding tables.

Tried and dropped (no gain, or a loss): contexts from the row above only; dropping
alpha-aware prediction or the cross-plane context (ratio loss, same speed); zero-residual and
copy-from-above runs; one rANS stream per color plane; ring-indexed rANS states; coding two
rows in turns (2-15% slower at every interleave step); forced inlining of the pixel code;
unchecked indexing in the pair coder and PGO (both cut instructions by 6-10% but not
cycles); `target-cpu=native` (no change).

How it was measured: `samply record --save-only`, then `dsymutil ../target/release/aqr-psd` and
`llvm-symbolizer --inlining` on the sampled addresses (plus 0x100000000) for inlined source
lines and annotated disassembly; A/B comparisons by the cycles and instructions of
`/usr/bin/time -l`, since wall-clock times moved by ±5% with other load on the machine.

Open: cutting instructions has not cut cycles, so the bound is likely elsewhere (front end or
memory); Instruments' CPU counters (full Xcode) would tell. Compression of tori29umai and
GenPoster is short of 100 MB/s; a row-wise SIMD model would need a predictor without the left
neighbor, at a ratio cost.

## Corpus

`corpus/` is not tracked. Sets used:

| set | source | files | size |
|---|---|---|---|
| genposter | [GenPoster-100K](https://huggingface.co/datasets/BruceW91/GenPoster-100K) `meta_psd/`, files < 100 MB (Freepik templates, CC-BY-NC) | 52 | 2.0 GB |
| oga | [OpenGameArt](https://opengameart.org) search "psd": .psd files and .psd inside .zip | 828 | 3.8 GB |
| tori29umai | [tori29umai/train_psd](https://huggingface.co/datasets/tori29umai/train_psd) (illustrations, Apache-2.0) | 32 | 379 MB |
| fixtures | test files of psd-tools, ag-psd, psd.js, webtoon/psd, Krita, Pillow, chinedufn/psd, psd.rb | 651 | 205 MB |

The speed subsets are ~110-120 MB of each of the first three (10, 7 and 45 files).
