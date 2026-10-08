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
```

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

Decoding runs at about 4.3 instructions per cycle: it is bound by the work per pixel, not by
latency.

Memory (peak RSS less input and output), largest files:

| file | size | compress | decompress |
|---|---|---|---|
| OpenGameArt relic_0.psd | 166 MB | 31 MB | 4 MB |
| OpenGameArt WarMachines_Demo.psd | 134 MB | ~30 MB | 9 MB |

The first version (whole planes in memory) took 7.2 GB / 6.4 GB on the latter.
