# psd-recons

Lossless PSD/PSB recompression: a Photoshop file is stored smaller, and restored bit for
bit.

```sh
cargo build --release
psdr c in.psd out.psdr                  # compress
psdr d out.psdr back.psd                # decompress
psdr bench [-n ITERS] [-v] FILE...      # round trip check, ratio, throughput
psdstat [-v] FILE...                    # byte breakdown by section, summed over files
psdx check|predict|rle FILE...          # research tools: decoding, composite prediction, PackBits encoders
cargo test                              # round trips of synthetic PSDs (no corpus needed)
```

`PSDR_STATS=1` prints phase times and sizes; `PSDR_DEBUG=1` where decoding found corrupt
input. `examples/` holds profiling aids (`modelbench`, `renderbench`, `pbbench`, `recbench`) and
debugging ones (`chan`, `diff`, `rlediff`), run with `cargo run --release --example NAME -- FILE`.

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
- Channels repeating an earlier one are copies; files embedded in smart objects are
  recompressed recursively (PSD/PSB), or with png-recons and jpeg-recons.
- The rest of the file is zstd'd, at the strongest level the time budget allows.
- Both directions stream: pixel data is coded row by row from the input and decoded row by
  row into the output (allocated at its final size), so the memory besides input and output
  stays small (about 30 MB compressing, under 10 MB decompressing) whatever the file size
  (embedded PNG and JPEG files take what their recompressors need).

## Results (2026-10-08)

Output size / input size over whole sets (every file round-trips):

| set | files | size | psdr | zstd -3 | zstd -19 | xz -9 |
|---|---|---|---|---|---|---|
| tori29umai (illustrations) | 32 | 379 MB | **0.227** | 0.433 | 0.358 | 0.327 |
| GenPoster (poster templates) | 52 | 2.0 GB | 0.332 | 0.564 | 0.439 | **0.322** |
| OpenGameArt (game art) | 828 | 3.8 GB | **0.328** | 0.502 | 0.394 | 0.336 |
| fixtures (parser tests) | 651 | 205 MB | 0.265 | 0.297 | 0.251 | **0.219** |

One thread on an Apple M4, ~110-120 MB subsets. psdr's speeds are its cycles (from
`/usr/bin/time -l`, summed over one process per file and direction) at 4.4 GHz, which other
load on the machine hardly affects; zstd and xz were timed (on the subset concatenated):

| subset | psdr | c / d MB/s | zstd -3 | c / d | zstd -19 | c / d | xz -9 | c / d |
|---|---|---|---|---|---|---|---|---|
| tori29umai | 0.243 | 89 / 105 | 0.435 | 329 / 1202 | 0.361 | 9.5 / 1044 | 0.326 | 4.5 / 90 |
| GenPoster | 0.203 | 83 / 103 | 0.424 | 461 / 1556 | 0.279 | 10.2 / 1309 | 0.231 | 6.9 / 128 |
| OpenGameArt | 0.200 | 123 / 142 | 0.573 | 336 / 1250 | 0.483 | 5.2 / 784 | 0.292 | 4.4 / 100 |

Decoding runs at about 4.3 instructions per cycle.

Memory (peak RSS less input and output), largest files:

| file | size | compress | decompress |
|---|---|---|---|
| OpenGameArt relic_0.psd | 166 MB | 31 MB | 4 MB |
| OpenGameArt WarMachines_Demo.psd | 134 MB | ~30 MB | 9 MB |

The first version (whole planes in memory) took 7.2 GB / 6.4 GB on the latter.

## Limitations

- Pixel data is coded only in 8-bit documents (RLE or raw channels); 16- and 32-bit data,
  ZIP channels and 1-bit images stay in the skeleton (zstd). Color groups are RGB(A) only:
  other modes code each plane on its own.
- Embedded PDF and other non-PSD/PNG/JPEG smart object files stay in the skeleton.
- Rendering supports normal and the common blend modes, opacity, fill, user masks, clipping and
  groups; layer effects, adjustment layers and dissolve are not rendered (the hint is then
  weaker, or dropped when a sample of rows predicts less than half the pixels).
- A file whose structure cannot be parsed, or with an RLE row that does not decode, is stored
  (zstd) as a whole.
- Embedded PNG and JPEG files are recompressed in memory by their own recompressors.

## Speed work (2026-10-08)

From the first streaming version to the results above (subsets, single thread): compression
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
- zstd fitted to the skeleton's size and level to the time budget; 8-byte compares in
  PackBits encoders; 11-bit rANS probabilities with per-chunk compacted decoding tables.

Tried and dropped (no gain, or a loss): contexts from the row above only; dropping
alpha-aware prediction or the cross-plane context (ratio loss, same speed); zero-residual and
copy-from-above runs; one rANS stream per color plane; ring-indexed rANS states; coding two
rows in turns (2-15% slower at every interleave step); forced inlining of the pixel code;
unchecked indexing in the pair coder and PGO (both cut instructions by 6-10% but not
cycles); `target-cpu=native` (no change).

How it was measured: `samply record --save-only`, then `dsymutil target/release/psdr` and
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
