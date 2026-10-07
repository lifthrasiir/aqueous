# psd-recons

Lossless PSD/PSB recompression (planned): a Photoshop file is stored smaller, and restored bit
for bit. At this stage only the analysis tool exists.

```sh
cargo build --release
psdstat [-v] FILE...     # byte breakdown by section, summed over files
```

## Corpus

`corpus/` is not tracked. Sets used so far:

| set | source | files | size |
|---|---|---|---|
| genposter | [GenPoster-100K](https://huggingface.co/datasets/BruceW91/GenPoster-100K) `meta_psd/`, files < 100 MB (Freepik templates, CC-BY-NC) | 52 | 2.0 GB |
| oga | [OpenGameArt](https://opengameart.org) search "psd": .psd files and .psd inside .zip | 828 | 3.6 GB |
| tori29umai | [tori29umai/train_psd](https://huggingface.co/datasets/tori29umai/train_psd) (illustrations, Apache-2.0) | 32 | 379 MB |
| fixtures | test files of psd-tools, ag-psd, psd.js, webtoon/psd, Krita, Pillow, chinedufn/psd, psd.rb | 651 | 219 MB |
