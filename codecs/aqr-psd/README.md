# aqr-psd

Lossless PSD/PSB recompression: a Photoshop file is coded as a formula (the module
`psd-recons` of aqr-formula) of a smaller representation, and restored bit for bit. `FORMAT.md`
specifies the module and how it works: the pixel model, PackBits rebuilt by the encoder
identified per channel, the composite predicted by rendering the layers, and the threads and
memory plan.

```sh
cargo build --release --bins --example aqr-psd   # aqr-psd in target/release/examples
aqr-psd bench [-n ITERS] [-t N] [-v] [-c|-d] FILE...  # .aqr round trip check, ratio, throughput (-t default 1)
aqr-psd-stat [-v] FILE...                    # byte breakdown by section, summed over files
aqr-psd-research check|predict|rle FILE...          # research tools: decoding, composite prediction, PackBits encoders
cargo test                              # formula round trips of synthetic PSDs (no corpus needed)
```

`-Z psd:stats` (of `aqr`, or `aqr-psd bench -Z psd:stats`) prints phase times and sizes; `-Z psd:debug` tells where decoding found corrupt
input. `examples/` holds profiling aids (`modelbench`, `renderbench`, `pbbench`, `recbench`) and
debugging ones (`chan`, `diff`, `rlediff`), run with `cargo run --release --example aqr-psd-NAME -- FILE`.

## What is taken

- 8-bit documents' layer channels and composite (RLE/PackBits or raw), in RGB(A) groups; other
  modes code each plane on its own. Channels repeating an earlier one are copies.
- Smart objects' embedded PSD/PSB (recursively), PNG (aqr-png) and JPEG (aqr-jpeg) files.
- The rest, the skeleton, is kept as it is: whatever holds the formula compresses it.
- A file whose structure cannot be parsed, or with an RLE row that does not decode, gets no
  formula (`formula::encode` gives `None`).
- Not taken, left in the skeleton: 16- and 32-bit data, ZIP channels, 1-bit images, embedded PDF
  and other files.
- Memory is about 28 MB + 5.4x the file for `aqr` (372 MB for a 22.7 MB file with embedded
  JPEGs); aqr-psd's own is the streams and a few MB, the output at its final size, plus the
  embedded files' recompressors (up to 96 MB, for progressive JPEGs).
- `-t N` (`threads`): the output is the same bytes whatever the number of threads.

## Results

Apple M4, 2026-10-24, one thread, through `aqr-show` (see the
[file formats overview](../../docs/file-formats.md#how-it-was-measured)): each file archived at
level `mid` with its round-trip check, then extracted. Ratios are tentative, since the decoder
modules were not in the archives then. Measured again with the modules in the archives on
2026-11-01 (`docs/file-formats.md`, 37 files, 424 MB): 0.230-0.236 of the input (zstd -3 0.419).

| set | files | MB | ratio | zstd -3 | compress | decompress |
|---|---|---|---|---|---|---|
| genposter | 5 | 198.1 | 0.393 | 0.693 | 37.0 MB/s | 97.9 MB/s |
| oga | 37 | 214.7 | 0.284 | 0.457 | 58.5 MB/s | 176.8 MB/s |
| fixtures | 13 | 16.1 | 0.230 | 0.284 | 40.3 MB/s | 148.6 MB/s |
| tori29umai | 1 | 10.3 | 0.215 | 0.383 | 40.2 MB/s | 87.5 MB/s |
| all | 56 | 439 | **0.330** | 0.555 | **45 MB/s** | **127 MB/s** |

8 files are kept as they are. The composite hint saves 19% (GenPoster) and 29% (OpenGameArt)
of the size.

## Tests

- `cargo test`: formula round trips of synthetic PSDs (no corpus needed).
- `aqr-psd bench` checks the `.aqr` round trip of the files given.

## Corpus

`corpus/` is not tracked. Sets used:

| set | source | files | size |
|---|---|---|---|
| genposter | [GenPoster-100K](https://huggingface.co/datasets/BruceW91/GenPoster-100K) `meta_psd/`, files < 100 MB (Freepik templates, CC-BY-NC) | 52 | 2.0 GB |
| oga | [OpenGameArt](https://opengameart.org) search "psd": .psd files and .psd inside .zip | 828 | 3.8 GB |
| tori29umai | [tori29umai/train_psd](https://huggingface.co/datasets/tori29umai/train_psd) (illustrations, Apache-2.0) | 32 | 379 MB |
| fixtures | test files of psd-tools, ag-psd, psd.js, webtoon/psd, Krita, Pillow, chinedufn/psd, psd.rb | 651 | 205 MB |

The speed subsets are ~110-120 MB of each of the first three (10, 7 and 45 files).

## Left

- **Deeper pixel data.** 16- and 32-bit documents, ZIP channels and 1-bit images stay in the
  skeleton; non-RGB modes code each plane on its own.
- **Embedded PDF and other smart objects.** They stay in the skeleton.
- **Layer effects, adjustment layers and dissolve** are not rendered, so the composite hint is
  weaker, or dropped when a sample of rows predicts less than half the pixels.
- **Speed.** Compression of tori29umai and GenPoster is short of 100 MB/s; cutting instructions
  has not cut cycles, so the bound is likely the front end or memory (Instruments' CPU counters
  would tell). A row-wise SIMD model would need a predictor without the left neighbour, at a
  ratio cost.
