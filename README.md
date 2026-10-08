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
  a pixel model (MED prediction, contexts, run mode, static rANS tables from png-recons).
- PackBits is rebuilt bit for bit: Photoshop's encoder is identified per channel (it cuts rows
  at its image cache tiles, then into 128-byte chunks); rows no known encoder reproduces are
  stored verbatim.
- The composite is predicted by rendering the layers (blend modes, masks, clipping, groups).
- Channels repeating an earlier one are copies; files embedded in smart objects are
  recompressed recursively (PSD/PSB), or with png-recons and jpeg-recons.
- The rest of the file is zstd'd.

## Results (baseline, 2026-10-08)

Output size / input size over whole sets (every file round-trips):

| set | files | size | psdr | zstd -3 | zstd -19 | xz -9 |
|---|---|---|---|---|---|---|
| tori29umai (illustrations) | 32 | 379 MB | **0.222** | 0.433 | 0.358 | 0.327 |
| GenPoster (poster templates) | 52 | 2.0 GB | 0.324 | 0.564 | 0.439 | **0.322** |
| OpenGameArt (game art) | 828 | 3.8 GB | **0.333** | 0.502 | 0.394 | 0.336 |
| fixtures (parser tests) | 651 | 205 MB | 0.244 | 0.297 | 0.251 | **0.219** |

One thread on an Apple M4, ~110-120 MB subsets (zstd and xz on the subset concatenated):

| subset | psdr | c / d MB/s | zstd -19 | c / d | xz -9 | c / d |
|---|---|---|---|---|---|---|
| tori29umai | 0.235 | 25.7 / 32.9 | 0.361 | 9.5 / 1044 | 0.326 | 4.5 / 90 |
| GenPoster | 0.220 | 21.8 / 30.2 | 0.279 | 10.2 / 1309 | 0.231 | 6.9 / 128 |
| OpenGameArt | 0.204 | 35.4 / 52.7 | 0.483 | 5.2 / 784 | 0.292 | 4.4 / 100 |

## Corpus

`corpus/` is not tracked. Sets used so far:

| set | source | files | size |
|---|---|---|---|
| genposter | [GenPoster-100K](https://huggingface.co/datasets/BruceW91/GenPoster-100K) `meta_psd/`, files < 100 MB (Freepik templates, CC-BY-NC) | 52 | 2.0 GB |
| oga | [OpenGameArt](https://opengameart.org) search "psd": .psd files and .psd inside .zip | 828 | 3.6 GB |
| tori29umai | [tori29umai/train_psd](https://huggingface.co/datasets/tori29umai/train_psd) (illustrations, Apache-2.0) | 32 | 379 MB |
| fixtures | test files of psd-tools, ag-psd, psd.js, webtoon/psd, Krita, Pillow, chinedufn/psd, psd.rb | 651 | 219 MB |
