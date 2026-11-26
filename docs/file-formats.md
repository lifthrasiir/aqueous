# Aqueous: recompressors

Where each lossless recompressor stands, what it measures, and the formats not taken yet.
Formats are specified in each crate's `FORMAT.md`, archives and formulas in `spec/index.bs`,
and the design is argued in `xform-design.md`. How each recompressor works, its fuller results
and what is left for it are in its crate's README; the rows below are the summary.

## How it was measured

- Ratios: 2026-11-01, through `aqr-show` by `scripts/format-ratio.py` (its `sample` draws the
  samples below, its `measure` runs `aqr-show` as the Ratio column tells), at commit `54cddfe`
  (aqr's code as at `83097ad`), level `mid`. On an Apple M4 on one thread (enwik9 on 10, hg19
  on 4), but for aqr-clip, aqr-tex, aqr-vorbis, aqr-prores and the Hangul text: on a
  Ryzen 7 9700X with `-t 16`. Container formats (ZIP, gzip and the like) were not measured
  again: what they gain is what their contents are.
- Speeds and memory: 2026-10-23 to -25, Apple M4, one thread (`-t 1`), at `2abe944` (the memory
  of aqr-raw, aqr-flac, aqr-pdf, aqr-sqlite, aqr-obj, aqr-zip and aqr-exe again on 2026-10-24
  at `da98d11`), not measured again with the ratios. Through `aqr-show`
  (`crates/aqr/src/bin/show.rs`): each file is archived (recompression, archive coding and the
  round-trip check) at level `mid`, then extracted. Rows measured otherwise (with `aqr`, a
  crate's bench tool, another machine or date) say so in their notes.
- Samples: files of 256 KiB or more from each crate's `corpus/` (aqr-jpeg: `samples/` and the
  JPEG files of codec-corpus), taken in the order of their paths' hashes up to about 400 MB.
- Columns:
  - **Ratio**: Σ archive / Σ input, as *best ~ worst*. An archive carries the decoder modules
    (Wasm) its formulas apply and their resources (built-in tables), each once, shared by its
    files. Worst: one file per archive, its modules and resources in it (and files kept raw
    where they do not pay for them). Best: in the limit of many files per archive,
    Σ (archive − modules − resources) / Σ input, the formulas taken where they pay without
    them (`aqr-show -r -m`). One figure where the two agree to three decimals. Rows of
    container formats keep their earlier figures, of `aqr` on the whole set.
  - **zstd -3**: the same files under `zstd -3`, for reference.
  - **Speed**: Σ input / Σ time, MB/s. Compression covers recompression, archive coding and
    the check; decompression covers archive decoding and rebuilding. Decompression through
    `aqr` is timed by testing, `aqr -K`.
  - **Memory**: peak RSS, for one process per file that compresses, checks and decompresses
    it. It includes the input, the archive and the output, so its floor is about 2× the file.
    Given as a linear fit over the sample's files, fixed MB + k × input; files far over it
    are noted. macOS's allocator keeps freed large blocks resident for reuse, and they count:
    with that cache off (`MallocLargeCache=0`), aqr-exe's sample takes 17 MB + 5.3× instead
    of 26 MB + 7.9×. aqr-compress's `heap` of each format (`src/formats.rs`) is rounded up from
    these. The modules' own memory is a plan, exact to the page; the part modules (aqr-png,
    aqr-mp3, aqr-flac, `pcm-recons`, aqr-bz2, aqr-lz4, aqr-snappy, aqr-git, aqr-exe, aqr-fasta,
    aqr-bundle, aqr-lzma, aqr-lzx, aqr-prores, aqr-div3 and others) hold a window, not the data,
    and the fits, measured before them, are the peak of the process.

## Done

### Images

| format | crate | sample | ratio | zstd -3 | comp. MB/s | decomp. MB/s | memory / input | notes |
|---|---|---|---|---|---|---|---|---|
| PNG, APNG | aqr-png | 379 files, 391 MB | 0.802 ~ 0.855 | 0.997 | 42 | 90 | 7 MB + 10.3× | the data, the raw image and the tokens held a window at a time; the encoder's symbols (2 bytes per byte of data on photos) still follow the pixels |
| GIF | aqr-gif | the 488 GIF files of the 10gb corpus, 9.25 MB (README) | 0.528 ~ 0.570 (0.987 before) | 0.989 | 2.7 (`aqr`, 10 threads) | 4.8 and 3.5 (`-K`, 10 threads; the second as Wasm) | 0.5 GB (`aqr` peak) | speed and memory 2026-10-31 with `aqr` (all in one archive: 0.522). The module (14.5 KB) does not pay alone for most of these small files: 474 of 488 kept raw (worst), 173 without it (best). The model's tables are up to 32 MB a file |
| JPEG | aqr-jpeg | 96 files, 186 MB | 0.778 ~ 0.794 | 0.913 | 39 | 82 | 28 MB + 8.2× | sequential files coded a stripe at a time (a 1.3 MB 7680×4320 file: 54 MB); progressive ones hold their coefficients whole (259 MB for a 1.4 MB one of that size) |
| BMP, PGM, PPM, PAM, TIFF, DICOM (uncompressed) | aqr-raw | 17 files, 51 MB | 0.245 ~ 0.248 | 0.444 | 71 | 303 | 21 MB + 5.5× | 8-bit samples of PNGs converted, not real files (README). 16-bit PNM, TIFF and DICOM by `raster-recons-16`: four 16-bit PPMs 0.756 (zstd -3 0.990). RLE images not taken |
| Lossless JPEG (SOF3) in CR2, DNG | aqr-jpeg (`jpeg-lossless-recons`) | 13 files of raw.pixls.us, 272 MB (README) | 0.83–0.96 a file against its archive (5D IV 0.826, 5D III 0.864, 1D X II 0.886, DNG 0.86–0.95) | | | 19 (one thread), 155 (a stream in bands, 16 threads) | the plan follows the frames' width and a chunk (2–4 MiB for a 62 MB CR2) | measured 2026-11-10. The model gains 5–15% over the Huffman data, noise-bound (Rawzor claimed 20–40% around 2008) |
| PSD, PSB | aqr-psd | 37 files, 424 MB | 0.230 ~ 0.236 | 0.419 | 45 | 127 | 28 MB + 5.4× | pixels coded in 8-bit documents only |
| Clip Studio Paint (`.clip`) | aqr-clip | 13 files, 926 MB | 0.339 ~ 0.340 | 0.760 | 35 | 91 | not measured | 2026-10-28, Ryzen 7 9700X, `-t 16`; aqr without it 0.726. A held-out file 0.454 against 0.763 (README). The database's PNG preview is not recompressed |
| DDS (BC1–BC5) | aqr-tex | 1275 files of The Crew's textures, 400 MB | 0.342 ~ 0.376 | 0.486 | 23 | 20 | not measured | a sample of no least size (`sample --min 0`); the module (19 KB) does not pay alone for small files. Speeds 2026-10-28 on a Ryzen 7 9700X by `aqr-tex-bench` (a thread); on an M4 since 2026-11-01: 31.9 / 27.1. All 52,181 files (16.4 GB): 0.325 against zstd -19's 0.403. Uncompressed DDS is aqr-raw's |
| BC7, BC6H, ETC1, ETC2, EAC, PVRTC1, ASTC (DDS, KTX, KTX 2, PVR, `.astc`; Unity's textures) | aqr-tex (`bc7-recons`, `bc6h-recons`, `etc-recons`, `pvrtc-recons`, `astc-recons`) | BC7: 164 textures of 9 Unity games, 194.5 MB (README) | BC7 0.234 | 0.393 | 19 | 22 | not measured | BC7 2026-11-13 on a Ryzen 7 9700X, `aqr-tex-bench -j 4`; zstd -19 0.347; the module 64 KB. The others on real images by black-box encoders (README, 2026-11-14), against zstd -3: ETC1 0.20 (0.31), ETC2 RGBA 0.13 (0.22), EAC R11 0.63 (0.86), PVRTC 4bpp 0.31 (0.48), ASTC 6×6 0.43 (0.54), BC6H 0.66 (0.98) |

### Audio

| format | crate | sample | ratio | zstd -3 | comp. MB/s | decomp. MB/s | memory / input | notes |
|---|---|---|---|---|---|---|---|---|
| MP3 | aqr-mp3 | 394 files, 400 MB | 0.852 ~ 0.915 | 0.968 | 45 | 116 | 61 MB + 6.4× | the resource (built-in tables, 131 KB raw, about 33 KB archived) and the module (about 28 KB) are paid by a lone 1 MB track (worst): 23 of 394 kept raw. Speeds of the files coded (`-r`); memory a fit over concatenations of the sample (9–120 MB, `-e`), the fixed part mostly the tables |
| Vorbis | aqr-vorbis | 4 Ogg Vorbis files of libvorbis (2008), 29.1 MB; 144 FSB5 sound banks of The Crew, 401 MB | 0.919 ~ 0.925; 0.845 ~ 0.853 | 0.999; 0.955 | 55 (`aqr`, 10 threads) | 158 and 117 (`-K`, 10 threads; the second as Wasm) | 0.66 GB heap peak (`aqr`) | ratios of the files in their containers (aqr-ogg, aqr-fsb); speed and memory 2026-10-31 with `aqr` on 12 Ogg files (49.1 MB) not kept |
| FLAC; WAV, AIFF | aqr-flac; aqr-raw | 25 files, 450 MB | 0.946 ~ 0.948 | 0.999 | 65 | 165 | 9 MB + 11.5× | the module about 31 KB, the resource 2.4 KB. WAV and AIFF are aqr-raw's now, with the same streams |
| AC-3 (Dolby Digital) | aqr-ac3 | the audio of `mari.avi` (5.1, 384 kbps, 206.3 MB; README) | 0.876 | | 27 (4 threads) | | not measured | 2026-11-11. Only in DivX 3 AVI files (aqr-div3) yet; no E-AC-3 |
| IMA ADPCM (Xbox ADPCM in FSB5, Microsoft's in WAV) | aqr-ima | The Crew's FSB5 banks; IMA ADPCM WAV files of ffmpeg and sox (README) | the blocks' record 0.578 (xz -9e about 0.67); WAV 0.58–0.61 stereo, 0.78 mono | | 33 (one thread) | 25 (one thread) | not measured | through aqr-fsb and aqr-raw |
| Microsoft ADPCM (WAV's format 2, XACT's wave banks) | aqr-msadpcm | the ADPCM waves of five XACT wave banks of three games, 581 MB (README) | the record 0.786 of music, 0.527 of effects (zstd -19 0.822); the banks −12.5% through `aqr` | | 24 (one thread) | 19 (one thread) | not measured | 2026-11-13 on Windows; through aqr-raw and aqr-bundle's `xact-xwb` |

### Documents, data, 3D

| format | crate | sample | ratio | zstd -3 | comp. MB/s | decomp. MB/s | memory / input | notes |
|---|---|---|---|---|---|---|---|---|
| PDF | aqr-pdf | 123 files, 456 MB | 0.822 ~ 0.836 | 0.918 | 46 | 198 | 41 MB + 5.7× | files kept as they are: 73 alone (worst), 5 without the modules (best). Speed and memory of an earlier sample (144 files, 402 MB); 9 files take over twice the fit, up to 331 MB for 3.4 MB. Files encrypted with the empty user password (RC4, AESV2, AESV3) are decrypted and encrypted again: 19 such of CC's first 1,000, 70.25 → 39.58 MB |
| SQLite 3 | aqr-sqlite | 88 files, 324 MB | 0.210 ~ 0.219 | 0.366 | 48 | 368 | 24 MB + 6.7× | |
| Text (words, XML markup) | aqr-wrt | enwik9, 1 GB (README) | 0.233 ~ 0.235 | 0.312 | 148 | 291 | not measured | speeds 2026-10-26 with `aqr` (10 threads); text of 256 KiB or more. The 10gb corpus −35.8 MB |
| Text of Hangul (Korean) | aqr-wrt (`wrt-recons-hangul`) | namuwiki, 512 MiB of 8.8 GB (README) | 0.261 ~ 0.267 | 0.349 | 175 | 1280 | 4.2× | the middle 512 MiB; speeds 2026-10-29 with `aqr -5` (10 threads) on a cut of its own; zstd -9 on it 0.307 |
| FASTA (nucleotides) | aqr-fasta | hg19, 93 files, 3.2 GB (README) | 0.209 | 0.309 | 170 | 133 (753 through `aqr`, files at once) | not measured | speeds 2026-10-26 by `aqr-fasta-bench` (chr1) and `aqr`; zstd -3 file by file |
| Delimited tables (CSV, TSV, USV) | aqr-xsv | 10 files, 11.7 MB (README) | 0.792 of `zstd -3` (streams through zstd -3) | | 39–479 | 584–1219 | not measured | 2026-10-28 by `aqr-xsv-bench`: GPS logs 0.41 of zstd -3, `airport-codes.csv` 0.82, `UnicodeData.txt` 0.91. The 10gb corpus −1.71 MB (85 files) |
| Lines of text (logs, tables of numbers) | aqr-log | 8 files of loghub and others, 713 MB (README) | 1.09–19.2% of the input through `aqr` (openssh 1.09%, NASA's access log 5.76%, Binance's klines 19.2%) | | | | not measured | 2026-11-14 on an M4. Chosen for text by coding a sample against aqr-wrt's and aqr-xsv's (aqr-compress's `table`, `src/lib.rs`) |
| JSON lines | aqr-jsonl | 7 files, 2.27 GB (README) | 0.872 of `aqr` before (through `aqr`) | | 179–1012 | 741–3080 (one thread) | segments of 64 MiB: decompressing 2.24 GB, 376 MB of heap | 2026-11-01: GH Archive −8.4%, Amazon reviews −18.1%, OpenAlex −27.8%, GeoJSON −15.1%, event logs −37.6%, C4 −1.5% |
| XML (elements of text and records) | aqr-xml | 28 files, 1.68 GB (README) | 0.891 of `aqr` before (through `aqr`) | | 500–3000 (the module, one thread) | 640–2300 | segments of 48 MiB: decompressing DBLP (268 MB) peaks at 568 MB | 2026-11-26: DBLP −20.8%, PubMed −23.6%, TPC-H lineitem −35.9%, Stack Exchange −3.7 to −18.5%, enwik9 −1.2%. Rows of attributes one to a line (OSM) stay aqr-log's; HTML left out. Compressing takes 1.29 times the instructions in all |
| Binary-to-text in text (base64, hex, quoted-printable: MIME, PEM, data URIs, JSON and XML, RTF's `\pict`, SQL dumps' hex) | aqr-b2t | synthetic files of local corpora (README) | −6% to −19% through `aqr` (mbox −13.5%, ipynb −18.7%, mysqldump `--hex-blob` −16.4%) | | | | not measured | the blocks decoded and their bytes recompressed by their formats |
| FITS (images, Rice tiles, binary tables) | aqr-fits | 68 files, 96.2 MB (README) | 0.584 (0.715 before) | | 7.1 (`aqr -c -T 1`) | 22 | 299 MB peak for an 11.1 MB file | 2026-10-28 with `aqr`, one archive per file: DSS 0.505 → 0.172, AIA's Rice tiles 1.000 → 0.937, Kepler's tables 0.591 → 0.307; `GZIP_*`, `HCOMPRESS_1`, `PLIO_1` tiles kept |
| HDF5 (netCDF-4, NeXus, NWB, AnnData, Keras) | aqr-hdf5, aqr-flate | 60 files, 271.2 MB (README) | 0.728 (default level), 0.704 (`-9`); 0.826 before | | 8.6 (`aqr`, 16 threads) | | not measured | 2026-10-30 on Windows. Deflated chunks by `flate-set`, floats by a linear predictor (GWOSC 1.000 → 0.799); LZF, Blosc, szip and zstd chunks kept |
| GGUF, safetensors, PyTorch `.pth` (model weights) | aqr-tensors | 10 GGUF files, 80.7 GB (README); Segment Anything's `.pth`, 2.56 GB of F32 | 0.640–0.661 (BF16, F16), 0.808–0.941 (Q8_0), 0.892 (Q4_0 QAT), 0.962–0.967 (Q4_K, Q6_K); 0.833 (the `.pth`) | 0.761–0.779, 0.857–0.956, 0.928, 0.982–0.996, 0.926 (`aqr` before) | 297–428 (`aqr`, 16 threads) | 692–1879 (`aqr -d`) | 0.7–2.2 GB heap peak | 2026-11-12 on a Ryzen 7 9700X. The coding runs at 1.6–3.8 GB/s, the rest is the archive's. A file of 1 GiB or more archived alone has its blocks written straight to the archive: a 49.86 GB shard 47.72 GB in 188 s at 1.22 GB resident peak. Weights converted from a lower precision as grids (README, "Conversions"): NVFP4 to BF16 0.530 → 0.434, GPTQ to F16 0.685 → 0.418 |
| GNU gettext catalogs (`.mo`) | aqr-gnu-gettext | the 10gb corpus's 1,171 distinct `.mo` files, 79.66 MB (README) | 10.98 MB archived together (13.70 before, −19.8%) | | 68 (`aqr`, all threads) | | not measured | |
| Wavefront OBJ | aqr-obj | 14 files, 411 MB | 0.080 ~ 0.081 | 0.232 | 121 | 324 | 24 MB + 3.1× | files over 300 MB left out of the sample; a file over 64 MiB is cut into portions coded side by side. Speed and memory by `aqr-obj bench` on 22 files of 403 MB. Taken by its text, not by the name `.obj` |

### Video

| format | crate | sample | ratio | zstd -3 | comp. MB/s | decomp. MB/s | memory / input | notes |
|---|---|---|---|---|---|---|---|---|
| Apple ProRes (QuickTime, MXF) | aqr-prores | 33 files, 16.89 GB (README) | 0.676 (0.990 before) | 0.919 | 46 (`aqr`, 16 threads) | 130 (`-K`, 16 threads) | the archiver's | 2026-11-01 on a Ryzen 7 9700X; the previous frame as context is most of the gain (0.830 without). The largest file (4.58 GB) is found but not recoded. Since `prores-recons` 3 (a part module), 30 files of 6.49 GB at the default bound: 0.694 (README) |
| DivX 3 (MS-MPEG4 v3) in AVI | aqr-div3 | `mari.avi`, 675 MB (README) | the video 0.887, the file 0.878 (with aqr-ac3) | | 38 (`aqr`, all threads) | 15.9 (the frames, one thread) | a part module | 2026-11-11. The VLC tables probed from ffmpeg as a black box; about 0.5% of P frames do not parse and are stored |

### Containers and general-purpose compression

| format | crate | sample | ratio | zstd -3 | comp. MB/s | decomp. MB/s | memory / input | notes |
|---|---|---|---|---|---|---|---|---|
| gzip, zlib | aqr-flate | 4 files, 4 MB | 0.932 | 0.977 | 16 | 58 | 34 MB + 35.6× | a small sample; 153 gzip files of one machine (280 MB): 0.881 at 9.0 MB/s; per-stream figures on 924 streams in aqr-flate's README. Streams that a probe says will not gain are not deconstructed |
| ZIP | aqr-zip | 5 files, 327 MB | 0.954 | 0.998 | 8 | 23 | 48 MB + 13.8× | dominated by enwik9.zip (322 MB); 76 ZIP files of one machine (267 MB) 0.7795 at 12.6 MB/s (README); large bodies rebuilt only where a probe says they may pay |
| mozlz4, LZ4 frames | aqr-lz4 | a Firefox profile (README) | −9 to −18% against `zstd -3` of the files, −57% solid | | | 650–1150 (the compressor) | not measured | liblz4 1.10.0's fast compressor reproduced exactly |
| lzop files (LZO1X) | aqr-lzo | Silesia through `lzop -3` and `-9`, 101.0 and 76.3 MB (README) | 0.622 and 0.821 | 0.840 and 0.923 | 16 and 10 (`aqr`, 10 threads) | 94 and 41 (`-K`; the second as Wasm) | 13.5× (`aqr` peak) | 2026-10-28; the original files archive to 57.8 MB: the records (4.5% and 6.1% of the streams) are most of the rest |
| LZFSE, LZVN (bvx blocks) | aqr-bvx | the 36 asset catalogs over 100 KB of `/Applications`, 272.5 MB (README) | 0.701 (0.809 before) | 0.854 | 4.9 (`aqr`, 10 threads) | 43 and 34 (`-K`; the second as Wasm) | 28.5× (`aqr` peak) | 2026-10-30 at level mid; taken at level 9 only now (170.57 MB). Records 1.5% (LZFSE) and 2.5% (LZVN) of the streams |
| LZMA (`.lzma`), xz (LZMA2) | aqr-lzma | the 10gb corpus's 123 `.tar.lzma` (73.08 MB) and 6 `.tar.xz`; a 7z folder; tars by `xz -6` (README) | records 13.5% of the `.tar.lzma` (10.1% of the `.tar.xz`, 15.5–18.4% of the others) | | 39 of data (a thread) | 34 of data (a thread) | modules of a plan of 118 MiB for an 8 MiB dictionary, 186 MiB for 64 MiB; natively 1.39 GB peak for the 7z folder (README) | 2026-11-02 on an M4. Pays only where the data is stored elsewhere too (a `.tar.lzma` beside its files: 96.5 → about 14 MB); a stream alone gains nothing (Hard, below) |
| 7z (LZMA, LZMA2, Copy, BCJ, BCJ2, Delta, Swap2, Swap4) | aqr-7z, aqr-bcj, aqr-lzma | the 10gb corpus's four 7z archives, 37.25 MB (README) | records 17.5% of the packed streams; `mingw480` (its tree and its 7z) 84.45 → 55.36 MB (the tree alone 47.36) | | 30 of data (`-t 4`) | 24 of data | 4.96 GB peak for `mingw480` | 2026-10-30 on an M4; PPMd, BZip2 and Deflate folders and encrypted archives kept |
| bzip2 | aqr-bz2 | 79 files of one machine, 352.8 MB (README) | 0.726 (0.88 for a source tar, 0.85 for a tar of binaries) | | 3–5 (`aqr`, 16 threads) | 16 (`-K`) | 30 B per symbol of a block, on each thread | 2026-10-31 on Windows; libbzip2's compressor reproduced exactly (149 of 153 files) |
| Ogg | aqr-ogg | | | | | | | pages from the packets, the Vorbis packets by aqr-vorbis. Opus and FLAC in Ogg not taken |
| FSB5 (FMOD sound banks) | aqr-fsb | | | | | | | Vorbis setups found in aqr-vorbis's database, Vorbis by aqr-vorbis, IMA ADPCM by aqr-ima |
| ar, tar, cpio | aqr-bundle | the 10gb corpus (README) | 2810.8 → 2775.0 MB for all of it | | 6600 (the module) | 28000 (the module) | not measured | an archive alone gains nothing: its files are recompressed and shared with the same files archived along (`gimp.tar` and its files −11.3 MB) |
| asar (Electron) | aqr-asar | 27 archives of one machine (README) | 714.42 → 697.11 MB (with aqr-gif) | | 950 | 4100 (the module) | not measured | archives of many small files lose alone (their text a stream apart) |
| Game engines' packs: Godot `.pck`, Unity AssetBundles (`UnityFS`), Unreal `.pak`, Valve VPK | aqr-bundle (`packs`), aqr-disk | Steam games (README of aqr-bundle) | Godot −1.1%, Unity −0.9% (−16.3% with their `.resS` textures rebuilt), Unreal stored 0%, Snake Pass −11.3%, VPK −14.2% | | | | not measured | 2026-11-13 on Windows. No module: the directory read into its files' extents (aqr-disk's formula), what follows from the contents 0.01–0.1% of a pack. Left: Unity's LZ4HC and LZMA blocks, Unreal's zlib blocks as streams of their own, Oodle never |
| Middleware's and engines' other packs: Unity's serialized files and `.resource`, Unreal 3's packages, FMOD, Wwise and XACT banks, CRI's AWB, ACB, CPK and AFS, GameMaker's `data.win`, Chromium's `.pak` (and, unverified on real files, Wwise's `.pck`, Quake's PAK and WADs, Doom's WADs, GMA, Bethesda's BSA and BA2, EA's BIG, Ren'Py's RPA, Kirikiri's XP3, MT Framework's ARC, PSARC) | aqr-bundle (`packs`), aqr-disk | Steam games (README of aqr-bundle) | `.assets` −13.8% (with their `.resS` −15.8%: BC1–BC5 by aqr-tex, uncompressed by aqr-raw), `.resource` −1.3%, FMOD −2.3%, XACT −12.5% (aqr-msadpcm), Unreal 3 −10.3% (its LZO1X chunks by aqr-lzo), GameMaker and CRI 0% | | | | not measured | 2026-11-13 on Windows against `6d9e0db8`. Wwise's Vorbis media are aqr-wwise's (below). Chromium's packs are read but not looked for: their small members archived apart cost more than the pack scanned (60 packs 10.78 → 11.17 MB). Left: Unity's crunched textures |
| Wwise Vorbis (`.wem`, in banks) | aqr-wwise, aqr-vorbis | two games' media and banks (README) | Unpacking −6.6%, Chants of Sennaar −7.4% | | | | not measured | the setup rebuilt from the database of skeletons, the packets by aqr-vorbis |
| ISO 9660, UDF images | aqr-iso, aqr-disk | a Windows Server 2008 R2 DVD, 3.17 GB (README) | 2777.6 → 2776.9 MB | | 177 (`aqr`, all threads) | | 1.82 GB peak | 2026-10-30; 89% of the DVD is `install.wim`, given as a file of its own (WIM row) |
| File system images: FAT12/16/32, exFAT, NTFS, ext2/3/4, APFS, HFS+, HFSX (all of an image, or on a partition) | aqr-disk; aqr-fat, aqr-ntfs, aqr-ext, aqr-apfs, aqr-hfs | a volume of each, made by their own tools (READMEs) | FAT32 26.21 → 23.34 MB; NTFS 132.5 → 123.8; ext4 478.3 → 425.7; APFS 346.3 → 325.3; HFS+ 245.5 → 231.7 (the image compressed as it is → its files taken apart) | 0.231 (HFS+) | 30–105 (`aqr`, all threads) | | resident peak 1.3–3.7 GB | 2026-11-09 on an M4; much of the time is the verify, the image being one leaf (`docs/disk-image-design.md`) |
| NTFS's compressed files: LZNT1, WOF's XPRESS4K/8K/16K and LZX | aqr-lznt1, aqr-xpress, aqr-lzx, aqr-ntfs | System32 compacted on NTFS volumes (README of aqr-disk) | LZNT1 672.8 → 364.2 MB; XPRESS4K 696.2 → 379.8; LZX 531.2 → 393.3 | | 13–20 (`aqr`, all threads) | | heap peak 1.5–1.7 GB | 2026-11-09 on an M4; records 0.35% (LZNT1) and 3.0–6.9% (XPRESS) of the streams |
| Virtual disks (dynamic VHD, VHDX, sparse and streamOptimized VMDK, qcow2, VDI), UDIF (DMG), EnCase E01 | aqr-disk, aqr-flate, aqr-bz2, aqr-bvx | a 2 GB ext4 disk in each container; a 1.33 GB HFS+ disk as UDZO and E01 (README of aqr-disk) | VHDX 510.9 → 461.9 MB, qcow2 515.7 → 467.4 (the disk itself 461.9); UDZO 371.6 → 230.9; E01 383.5 → 231.7 | | 38 (VHDX), 18 (UDZO), 13 (E01) | 78 (UDZO), 83 (E01) | resident peak 4.4 GB (VHDX) | 2026-11-09/10 on an M4; compressed chunks rebuilt from the disk, zlib at profile 10 where its emulation predicts a sample exactly |
| Zstandard (`.zst`, `.tar.zst`) | aqr-zstd | libarchive's source tree (20.98 MB) beside its `.tar.zst` (5.89 MB; README) | 11.44 → 5.95 MB for both (the tree alone 5.56) | | | | resident peak 182 → 83 MB | 2026-11-09; no frame rebuilt: the files a `.zst` holds are taken from its frames by Patch instructions. A frame alone gains nothing (Hard, below). zstd frames in other data (SQLite blobs) are made again by `zstd-frames` (level 3) |
| WIM (LZX, stored) | aqr-wim, aqr-lzx | the same DVD's `install.wim` (2.81 GB) and `boot.wim` (README) | the DVD: 3.17 GB → 2483.1 MB | | 19 (`aqr`, 16 threads) | 45 (extracting) | 22.9 GB peak commit compressing | 2026-10-30; records 6.74% of the LZX chunks |
| CAB (MSZIP, LZX, stored) | aqr-mszip, aqr-flate, aqr-lzx | 441 CAB files of Windows, 1.80 GB (README) | 1662.7 → 1331.3 MB | | 23 (`aqr`, all threads) | 84 (`-K`) | 3.92 GB peak footprint | 2026-10-31; MSZIP records 4.6% (profile 6) / 1.2% (10); LZX (records 7.9%) from level 9 only: at mid −1.1% more for 73.7 → 290 s |
| OLE compound files (MSI, MSP; Office 97–2003, HWP 5, `.msg`) | aqr-ole, aqr-disk | `C:\Windows\Installer` (README) | 19.60 → 16.57 MB (0.846) at `-9` | | | | not measured | 2026-11-11; the streams its files (a package's cabinet put together for aqr-mszip and aqr-lzx). Office streams only separated; HWP's raw deflate not found |
| Inno Setup installers (LZMA) | aqr-inno, aqr-lzma | `ALZip1237.exe` (Inno Setup 6.4.2, 45.27 MB; README) | 44.79 → 35.84 MB | | 2.4 (`aqr`, 16 threads) | 6.7 | not measured | 2026-11-11; the solid LZMA stream split (record 6.7%) from level 4 |
| InstallShield cabinets (`data1.cab`) | aqr-isc, aqr-flate | six `data1.cab`, 0.4–54.7 MB (README) | 0.24–0.71 (0.39–0.93 before) | | 28 | 54 | not measured | 2026-11-11; raw DEFLATE chunks by `flate-set` (records 0.9–6.3%) |
| CHM | aqr-mszip, aqr-lzx | 139 help files, 90.9 MB (README) | 88.36 → 69.23 MB at level 9 | | 7.0 (`aqr -9`) | 19 (`-K`) | 1.44 GB peak footprint | 2026-10-31; from level 9 only; records 4.9% |
| ALZ (ALZip) | aqr-alz, aqr-flate, aqr-bz2 | 16 archives, 7.31 MB (README) | 7.19 → 5.54 MB at mid, 5.05 at level 9 | | 5.5 (mid), 1.8 (9) | 23 (`-K`) | 0.35 GB peak at mid | 2026-11-01; DEFLATE is zlib's (records 0.01% at profile 10), bzip2 libbzip2's in ALZip's framing |
| EGG (ALZip) | aqr-egg, aqr-flate, aqr-bz2, aqr-lzma, aqr-azo | 327 test archives, 49.5 MB (README) | 33.19 → 16.66 MB at mid, 16.42 at level 9 (31.87 before) | | 19 (mid), 13 (9) | 79 (`-K`) | 0.96 GB peak at mid | 2026-11-01; much of the gain is the same files stored once |
| AZO (ALZip, in EGG) | aqr-azo | the 17 AZO streams of the EGG test archives, 3.12 MB (README) | records 1.03% of the streams | | 5.5 (one thread) | 6.4 | about 14 MB for a block of 512 KiB as Wasm | 2026-11-01; the x86 filter found from the CRC-32 alone (`docs/azo-x86-filter-search.md`) |
| LHA (`.lzh`, `.lha`) | aqr-lha | 119 Aminet archives, 25.2 MB (README) | 24.95 → 22.85 MB at mid, 22.04 at level 9 | | 6.0 (mid), 4.1 (9) | 22 (`-K`) | 0.29 GB peak at mid | 2026-11-01; records 0.38% of the streams. Much of Aminet is already compressed |
| RAR (RAR5, RAR 1.5–4.x of RAR 2.9's algorithm) | aqr-rar | the 10gb corpus's 83 RAR archives, 22.25 MB, mostly PPMd (README) | 13.44 MB at mid (PPMd from level 9 only), 13.25 at level 9 (−1.4%) | | 4.2 (mid), 3.8 (9) | | 0.32 GB peak at mid, 1.71 at 9 | 2026-11-01; records 0.717% of the corpus's streams (3.12% of rar 7.23's RAR5). High-order PPMd archives do not pay apart: aqr's coders are no better on their data |
| NSIS installers (stored data) | aqr-compress (`nsis`) | the 10gb corpus's 11 installers | 2483.0 → 2464.9 MB for all of it | | | | not measured | 2026-11-02; the files stored in the installer's data cut out; NSIS's zlib, bzip2 and LZMA not reproduced |
| PPMd var. J (`.pmd`) | aqr-ppmd | the 10gb corpus's two files, 205.4 MB (README) | 2579.2 → 2557.8 MB for all of it | | | | not measured | 2026-10-30; no module: `enwik8.pmd`'s data is stored within `enwik9.pmd`'s. PPMd itself is not recoded |
| Matroska, WebM | aqr-mkv | 11 WebM files from YouTube, 738.1 MB (README) | 0.531 (still pictures 0.32–0.67; moving videos not taken) | 0.532 (`--long=27`) | 408 (`aqr`) | 2600 (`-K`) | 1.94 GB peak | 2026-10-31; keyframes the same where the picture is still held once (the frames not decoded). What is left is mostly Opus audio |
| LevelDB tables (Snappy) | aqr-snappy | 33 tables of a Chrome profile, 13.3 MB (README) | −30.5% against `zstd -3` | | | 315 | not measured | both hashes of Snappy 1.2.2 |
| git checkouts | aqr-git (packs), aqr-flate (loose objects), the archive (the rest) | 15 repositories, 1196 MB (README) | 0.454 (0.430 from level 7) | 0.651 (`tar \| zstd -19 --long=27`) | 23 (`aqr`, 10 threads) | 142 (`-K`) | peak RSS 0.2–7.1 GB (pyre, 665 MB, 10.7×) | 2026-10-28 |
| Minecraft regions (`.mca`), Distant Horizons databases | aqr-minecraft, aqr-zstd | an 18 GB world (README) | the regions of three dimensions 8.79 GB: 5.31 → 2.42 GB; DH's databases The End 420.61 → 361.15 MB, the Overworld 5.98 → 5.92 GB (`dh-lods` on `zstd-frames`) | | | | heap peak 1.80 GB | on a Ryzen 7 9700X; the chunks' zlib (level 6) made again of their NBT, the NBT's block states and light by a model |

### Executables

| format | crate | sample | ratio | zstd -3 | comp. MB/s | decomp. MB/s | memory / input | notes |
|---|---|---|---|---|---|---|---|---|
| PE, Mach-O, ELF (x86, x86-64, ARM64, CIL) | aqr-exe | 259 files, 391 MB | 0.295 ~ 0.303 | 0.381 | 28 | 414 | 26 MB + 7.9× | `Program Files` and `System32` of a Windows machine; CIL 0.306 → 0.267 for the 84 IL assemblies; 39 files not coded. VS Code's 34 universal Mach-O binaries −16.7%; 22 Linux ELF binaries −16.2% (README) |
| DWARF (dSYM, ELF) | aqr-dwarf | the dSYM of `aqr` (rustc, DWARF 4), 214 MB; 7-Zip's `7zr` by clang and GCC, DWARF 5 (README) | 0.107 (rustc), 0.276 (clang), 0.239 (gcc) | 0.227 (rustc) | | | peak RSS 0.99 GB (rustc) | 2026-11-02; `.debug_info` by its DIEs and Apple's accelerator tables |
| PDB (MSF) | aqr-pdb | a Rust program's PDB by lld-link (54 MB), Rust's `std`, 12 of Windows' PDBs (README) | `uniform.pdb` 14.95 → 6.38 MB; `std` 0.729; `combase.pdb` 9.42 MB (xz -9 9.63) | | | | peak RSS 473 MB (`uniform.pdb`) | the modules' streams and the TPI and IPI records by field; Windows' public PDBs kept as they are |
| Android DEX | aqr-dex | the 21 DEX files of 7 F-Droid APKs, 132.0 MB (README) | 46.23 → 30.87 MB through `aqr` (−33.2%); the APKs −6.8% | 0.631 of zstd -9 (xz -9e 0.758) | 130 (the module) | 290 (the module) | not measured | 2026-11-02 on an M4 |
| Android resource tables (`resources.arsc`) | aqr-arsc | the same 7 APKs' tables, 44.7 MB (README) | 9.41 → 6.78 MB (−27.9%); the APKs −1.3% more | 0.638 of zstd -9 | | 2100 tested | not measured | 2026-11-02 on an M4; binary XML too small a file for a formula each |
| WebAssembly | aqr-wasm | the 56 modules of aqr-modules, 3.60 MB (README) | −20.5% through `aqr -c` at mid; −4.3% each file alone | −15.8% at zstd -22, each alone | 169 (one thread) | 185 (one thread) | not measured | the module (7.9 KB) costs an archive of one module nearly what it saves |

### Data of no known format

What aqr-compress tries on data in which no file of a known format is found.

| what | crate | sample | ratio | zstd -3 | comp. MB/s | decomp. MB/s | memory / input | notes |
|---|---|---|---|---|---|---|---|---|
| records of fixed size, entries with a length field, floats of whole multiples | aqr-infer-recs | 11 files, 114.6 MB (README) | 0.974 of zstd -3 (one thread) | | 65 (one thread) | | 527 MB peak RSS | tried where a 2–50 ms estimate tells 5% at least. The SAO star catalogue 0.432 of zstd -3 |
| directories of unknown archive formats (game paks) | aqr-infer-pak | The Crew: 14 files, 766.21 MB (`.fat`/`.dat`, LZO1X members) | 442.72 MB archived, 617.14 MB without (−28.3%) | | 32.3 (all of `aqr -c`) | | 1.93 GB heap peak | **taken out of aqr-compress again (2026-11-13): below** |

aqr-infer-pak on other games (2026-11-13), on the formats one or two games use only (those many
games share have recompressors of their own, aqr-bundle's `packs`). Four were parsed by hand for
a ground truth and archived three ways (`crates/aqr-infer-pak/scripts/gt`, at `4a93bf8`):

| sample | input | no paks | inferred | ground truth | members inferred right | of them the scan finds |
|---|---|---|---|---|---|---|
| Baba Is You, `Assets.dat` | 21.8 MB | 18.92 MB, 1.8 s | 18.95 MB, 0.6 s | 18.95 MB, 0.5 s | 3571 of 3572 | none |
| Waveform, 10 `.fc` | 665 MB | 528.58 MB, 14.9 s | as no paks (not looked at) | 528.59 MB | none | 31.5% (PNG) |
| Sakuna, 16 `.arc` | 245 MB | 241.40 MB | 241.40 MB | 241.40 MB | 49 of 6689 | none |
| The Tiny Bang Story, `data.pfp` | 175 MB | 134.230 MB, 9.4 s | as no paks | 134.230 MB | none | 39.0% (PNG, JPEG) |

Even the ground truth gains nothing (within 0.1%): the members are PNG, JPEG, Ogg and WAV, which
the scan finds, or game data stored. The gain is only where the members are coded by a codec aqr
rebuilds and the scan cannot find (The Crew's LZO1X, No Man's Sky's zstd chunks); many
single-game formats have records of names, out of the inference's design. A search of three
Steam libraries' files of no engine's format (4534 of 1 MB or more) for zlib streams, zstd
frames and gzip members found no other such pak. For the cost of a plan on every archive (up to
45 s for a game of 10 GB) the paks' plan is out of aqr-compress; aqr-infer-pak stays, with its
probe and batch. Left: an inference anchored on the members a scan finds; a measure of members
repeated across one game's paks.

### What is left, by crate

Each crate's README has what is left for it (its "Status", "Next steps", "Open issues and
limitations", "Limitations" or "Left"). Those whose README lists none:

| crate | next |
|---|---|
| aqr-bundle | members sliced from the archive's bytes rather than copied (the memory of `.tar.gz`); COFF objects of ar archives (aqr-exe); old GNU sparse files; tar archives found in other data |
| aqr-asar | contents of many small files by kind cost alone and pay in a batch (README: left to decide) |
| aqr-wasm | a dictionary for the streams (−16% more in a trial); the module's cost against archives of one module |

## Worth doing

The criteria: a weak entropy coder (prefix codes, Rice codes, fixed widths, none at all) or a
deterministic encoder that can be reproduced, and a field where the format makes most of the
bytes. Gains are estimates unless a tool is cited.

### Video production and broadcast

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| Avid DNxHD, DNxHR | broadcast, Avid workflows | intra DCT, VLC; constant bit rate, so every frame is padded | the padding, then the VLC recoded | aqr-prores's model and chunks |
| DV, HDV, XDCAM (MPEG-2), MPEG-4 Part 2 (Xvid, DivX 4 on), H.263, VC-1 (WMV9, Blu-ray) | archived broadcast and camcorder footage, DVD VOB, old downloads | DCT, VLC tables fixed by the standard | coefficients recoded; inter frames' motion vectors and macroblock modes modelled, nothing reconstructed (as aqr-div3 does DivX 3: 0.887) | aqr-div3's walk and model |
| Motion JPEG (AVI, MOV) | dashcams, webcams, older cameras, security cameras | a JPEG per frame, often without its Huffman tables (the standard ones implied) | each frame through aqr-jpeg; the frames' headers predicted from each other | aqr-jpeg |
| HuffYUV, FFVHuff, UT Video, MagicYUV | lossless capture, intermediates | spatial prediction, then static Huffman tables per stream or slice | the residuals context-modelled | aqr-raw's pixel model |
| H.264 CAVLC (baseline profile) | older phones, video calls, some security cameras | VLC tables chosen by the neighbours' coefficient counts | parsing needs no reconstruction, so the syntax elements can be recoded under richer contexts | aqr-mp3's structure |
| Theora | old web video, game cutscenes | VP3: DCT tokens with Huffman tables set in the stream header | as Vorbis | the Vorbis work |
| CineForm, ProRes RAW | high-end cameras, post-production | wavelets, or a RAW-specific coding | after ProRes (aqr-prores) | |
| MPEG-TS, M2TS | AVCHD camcorders, DVB and IPTV recordings, Blu-ray | 188-byte packets (192 in M2TS) of PES around the streams; null packets padding a constant rate | the packets' headers (continuity counters, PCR, M2TS's timestamps) derived and the null packets dropped; the streams handed to their recompressors (MPEG-2, MP2, AC-3; H.264 CABAC is Hard) | aqr-mkv's handling |
| AVI (other than aqr-div3's), ASF (WMV, WMA), FLV | old downloads, camera and screen recordings, Flash video | chunks or packets interleaved, with an index (`idx1`, ASF's simple index, FLV's keyframes) | the indexes derived from the chunks; the streams handed to their recompressors (Motion JPEG, MP3, VC-1, WMA). aqr-div3 takes AVIs of DivX 3, and of other video with AC-3 or MP3 audio (the frames kept) | aqr-div3's layout, aqr-mkv's handling |

### Audio

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| AAC (M4A, MP4), WMA | music stores, soundtracks of every MP4 | MDCT, then Huffman codebooks (AAC) or run-level VLC (WMA) | as packMP3; needs MP4 or ASF parsing | aqr-mp3's structure |
| MPEG-1 Layer II (MP2) | DAB radio, DVB and DVD soundtracks, broadcast archives | subband samples quantised at a fixed allocation, in grouped fixed-width codes: no entropy coding | the samples and scalefactors modelled: a large gain | aqr-mp3's frame handling |
| E-AC-3; AC-3 outside AVI | Blu-ray, broadcast, streaming 5.1; DVD VOB, MKV, MP4 | as AC-3 (aqr-ac3: 0.876 in DivX 3 AVIs), E-AC-3 with more coding tools | the frames found in their containers; E-AC-3's syntax | aqr-ac3 |
| DTS core, ATRAC3 | DVD and Blu-ray tracks; MiniDisc, PSP and PS3 games | ADPCM subbands with optional Huffman (DTS); fixed-width or VLC spectra (ATRAC3) | as above | |
| AMR-NB, AMR-WB | voice memos, 3GP, call recordings | CELP parameters at fixed widths per mode | the parameters modelled across frames | |
| ALAC | Apple music libraries | adaptive Rice | the residual model of FLAC | aqr-raw's audio model |
| Shorten, TTA | etree live-music archives, older lossless rips | Rice (Shorten), adaptive Rice with a filter (TTA) | the residual model of FLAC; most for Shorten | aqr-raw's audio model |
| CAF, W64, RF64 | audio production | uncompressed PCM | aqr-raw's `pcm-recons`, with these containers (BWF is a WAV, taken already) | aqr-raw |
| DSF, DSDIFF | DSD (SACD) downloads | 1-bit samples uncompressed (DST in DSDIFF is already arithmetic) | a DST-like model of the bit stream | |
| SoundFont (SF2), DLS, tracker modules (MOD, XM, IT, S3M) | sample libraries, game and demoscene music | uncompressed PCM samples (XM delta-coded; IT optionally its own compression) | `pcm-recons` on the samples found | aqr-raw |

### Camera RAW

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| Nikon NEF | photographers' originals (terabytes each) | Huffman-coded differences (also uncompressed and lossy variants) | as CR2's lossless JPEG (Done) | aqr-jpeg's lossless model; the JPEG previews |
| Sony ARW2 | the same | 128-bit blocks of min, max and 7-bit deltas: no entropy coding | the largest gain of the group | |
| Panasonic RW2, Olympus ORF, Pentax PEF, Hasselblad 3FR, Phase One IIQ | the same | fixed-width packing or simple Huffman or differential schemes per vendor | as NEF | |
| Fujifilm RAF, Canon CR3 | the same | RAF uncompressed or its own coding; CR3's CRX is adaptive Golomb | CR3 is already adaptive: less to gain | |

Nearly every RAW file holds one to three JPEG previews, which aqr-jpeg takes as they are. The
container is TIFF/EXIF.

### Images and graphics

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| TIFF (compressed), GeoTIFF, COG, OME-TIFF | scanning, print, GIS, microscopy | PackBits, LZW or deflate, with an optional horizontal predictor (uncompressed TIFF is aqr-raw's) | the existing recompressors per strip or tile | aqr-psd (PackBits), aqr-flate, aqr-gif (LZW, of MSB-first codes) |
| CCITT Group 3 and 4 | fax, scanned documents in TIFF and PDF (`CCITTFaxDecode`) | modified Huffman and READ codes with fixed tables | the bilevel pixels context-modelled (JBIG's context model beats G4 clearly) | |
| ICO, CUR, TGA, PCX, ILBM; RLE BMP | icons, old games, screenshots, textures | uncompressed or RLE | the samples through `raster-recons`; the RLE runs reproduced (TGA has no reliable signature) | aqr-raw |
| Radiance HDR | HDR environment maps | RGBE, per-scanline RLE | the RLE reproduced, the RGBE pixels modelled | |
| OpenEXR | VFX, rendering | per block: ZIP (zlib after a predictor and byte split), PIZ (wavelet, then Huffman), RLE, and lossy ones | ZIP blocks through aqr-flate; PIZ's Huffman recoded | aqr-flate |
| GIMP XCF | image editing | tiles in RLE or zlib | the zlib tiles; the RLE reproduced | aqr-flate |
| Krita (`.kra`), Procreate | digital painting | ZIP of layer tiles compressed with LZF (Krita) or LZO (Procreate) | LZF reproduced, LZO as aqr-lzo's record, then a pixel model | aqr-zip, aqr-lzo |
| ICNS, MNG, JNG | macOS icons, animations | PNG, JPEG or JPEG 2000 inside; a PackBits variant (old ICNS) | the embedded files through their recompressors | aqr-png, aqr-jpeg |

### 3D and CAD

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| FBX (binary) | 3D asset exchange, game pipelines | property arrays (vertices, indices, normals) optionally zlib-compressed one by one | the zlib arrays, then the arrays modelled as meshes | aqr-flate, aqr-obj's predictors |
| glTF and GLB (without Draco or meshopt), STL, PLY | the web, 3D printing, scans | raw little-endian buffers of floats and indices | a mesh model of the buffers (as aqr-obj's, without the text) | aqr-obj's predictors |
| USDZ, USDC | Apple AR, Pixar pipelines | USDZ is a stored ZIP; USDC compresses its arrays with LZ4 and integer coding | the LZ4 blocks reproduced | aqr-zip, aqr-lz4 |
| Blender (`.blend`) | 3D art | a dump of in-memory structs described by its SDNA; optionally gzip (before 3.0) or zstd | the structs modelled by their SDNA types | aqr-flate |
| LAS, E57 | lidar, surveying | uncompressed point records (LAZ is LAS's arithmetic-coded form) | the points predicted (LAZ is typically a tenth to a fifth of LAS) | |
| STEP, IGES, IFC | mechanical CAD exchange, BIM | text: entity instances of numbers and references | numbers and references (`#123`, mostly the previous instance's plus a little) modelled by entity type | aqr-obj's predictors, aqr-xsv's fields |
| DXF (ASCII) | AutoCAD exchange, CNC, laser cutting | text of group codes and values, one a line | the group codes predicted from the entity, the coordinates as in aqr-obj | aqr-obj |
| GDSII, OASIS | chip layouts (often GBs, then gzip'd) | GDSII: uncompressed records of 32-bit coordinates; OASIS: varints, optionally DEFLATE blocks (`CBLOCK`) | the polygons' coordinates as deltas; OASIS's raw deflate | aqr-flate |
| Alembic, OpenVDB | VFX caches and volumes | Alembic (Ogawa): uncompressed arrays of samples; OpenVDB: grids of half or full floats, in zlib or Blosc | the arrays against the frame before; the zlib grids | aqr-flate, aqr-hdf5's float model |

### Documents and office

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| HWP 5.0 | Korean government, schools and businesses (Hancom Office) | an OLE compound file whose streams are raw deflate | the deflate streams (aqr-ole takes the compound file apart already) | aqr-ole, aqr-flate |
| DOC, XLS, PPT (OLE) | legacy Microsoft Office | a compound file of uncompressed streams, embedded pictures (PNG, JPEG, EMF, often zlib-wrapped in PPT) | the pictures inside the streams (aqr-ole separates the streams) | aqr-ole, aqr-png, aqr-jpeg, aqr-flate |
| Outlook PST, OST | mail archives | "compressible encryption": a fixed byte substitution, then the messages and attachments as they are | the substitution undone (it is a table), then the contents recompressed | everything |
| MOBI, AZW, PalmDOC | e-books | PalmDOC's simple LZ77, or HUFF/CDIC | the PalmDOC encoder reproduced (small and deterministic) | |
| SWF | Flash archives (Flashpoint and others) | `CWS` is the whole file in zlib; `ZWS` is LZMA | the tags, JPEG and lossless bitmaps (zlib) inside | aqr-flate, aqr-jpeg |
| PostScript, EPS | print, old figures | images in ASCII85, RunLength, LZW, Flate or DCT filters (hex `image` data is aqr-b2t's) | the filters undone as in PDF | aqr-pdf's stream coding |
| Keynote, Pages, Numbers (iWork) | Apple's office documents | ZIP of `.iwa`: protobuf in Snappy chunks without the framing format's CRCs | the Snappy chunks reproduced (if Apple's compressor is Google's; else a record of the parse as aqr-lzo), then the protobuf by field | aqr-zip, aqr-snappy |
| OneNote (`.one`) | notes | a revision store (MS-ONESTORE) of uncompressed objects; embedded files and pictures as they are | the pictures and files recompressed; the store's references derived | everything |

MIME mail (base64, quoted-printable), RTF's `\pict`, Jupyter's base64 outputs and the like are
aqr-b2t's (Done).

### Fonts

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| TrueType, OpenType (CFF), TTC | every OS and application; CJK fonts take 5–20 MB each | uncompressed tables: `glyf` outlines (flags and coordinates as deltas of 1 or 2 bytes), CFF charstrings, `hmtx`, `loca` | WOFF2's transforms show the way (`loca` derived, `glyf` split into streams of flags, coordinates and instructions): about 30% under the same coder; a TTC's shared tables once | aqr-infer-recs's fields (it takes their tables now as data of no format) |
| WOFF | web fonts (before WOFF2) | each table in zlib, where that is smaller | the zlib streams, then the tables as above | aqr-flate |

### Data and machine learning

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| ONNX, `.npy`, `.npz`, PyTorch's legacy `.pt` | model weights | raw tensors (`.npz` is a stored ZIP) | as aqr-tensors does for GGUF, safetensors and `torch.save`'s ZIP format | aqr-zip, aqr-tensors |
| Parquet, ORC, Avro | data lakes, analytics | pages or blocks in Snappy (Parquet's usual default), gzip, LZ4 or zstd | the Snappy and gzip pages, then the column values | aqr-snappy, aqr-flate |
| Arrow IPC, Feather, Zarr (Blosc) | dataframes, arrays | LZ4 frames or zstd buffers; Blosc's shuffle and LZ4 | the LZ4 ones | aqr-lz4 |
| TFRecord | ML datasets | protobuf records, optionally gzip or zlib | the records | aqr-flate |
| MATLAB `.mat` (v5–v7) | engineering, science | each variable zlib-compressed (v7.3 is HDF5) | the zlib streams | aqr-flate |

### Databases

Backups come as text dumps (SQL or delimited rows), logical binary dumps, or the engine's own
files (pages, logs) copied by a backup tool. What they share:

- The layer of compression on top decides first: zlib, Snappy, LZ4 and LZF are reproduced;
  the closed ones (MSSQL's, RMAN's, Data Pump's, DB2's) belong under Hard; encryption (TDE,
  encrypted backups and binlogs) is detected and stored as it is.
- Pages carry fields derived from their place: checksums (CRC-32C in InnoDB, PostgreSQL's
  FNV-based 16 bits, Oracle's 16-bit XOR, MSSQL's), page numbers, space ids, LSNs and SCNs
  rising; logs carry file offsets (`log_pos`) and lengths. aqr-sqlite derives the same.
- Rows by column: SQL dumps are tables printed as literals, so aqr-xsv's columns and aqr-num's
  numbers apply once the literals are parsed; hex-printed binaries (`\x…`, `0x…`, `X'…'`) are
  aqr-b2t's already.
- Logs repeat the data: PostgreSQL's full-page images, binlog's before and after images,
  table maps in every transaction; recycled log files keep old contents in their tails.
- Daily full backups differ little from one another: where several generations are archived
  together, long-range matches (pages by number, dumps in a stable row order) gain the most.

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| mysqldump, MariaDB's `mariadb-dump` (`.sql`, `.sql.gz`) | MySQL backups, migrations | text: `--opt`'s extended `INSERT … VALUES (…),(…);`, lines up to `net_buffer_length` (about 1 MB) of thousands of rows in column order; strings with `\0 \n \r \\ \' \" \Z`; binaries escaped as they are, or `0x…` with `--hex-blob`; `--tab` writes TSV with `\N` | the rows parsed into columns (lines of a megabyte); the DDL and `/*!40101 … */` boilerplate by the archive | aqr-xsv, aqr-num, aqr-flate |
| mydumper, MySQL Shell's `util.dumpInstance` | large MySQL dumps | chunks of a table per file; mydumper's in gzip or zstd, Shell's TSV in zstd by default with JSON metadata | as above; zstd as under Hard | aqr-flate, aqr-xsv |
| MySQL binlog (`binlog.000123`) | replication, point-in-time recovery; up to `max_binlog_size` (1 GB) each | events of a 19-byte header (time, type, server id, size, `log_pos` = the next event's offset) and a CRC-32 (since 5.6); ROW format: `TABLE_MAP` then `WRITE/UPDATE/DELETE_ROWS`, each row a null bitmap and values by column type; `binlog_row_image=FULL` keeps both images of an update; optionally zstd (`TRANSACTION_PAYLOAD`, 8.0.20) or encrypted | the header fields and CRCs derived; rows by column from the table map; an update's after image against its before image | aqr-infer-recs's fields |
| MySQL InnoDB (`.ibd`, `ibdata1`, redo), Percona XtraBackup | database servers and their physical backups | 16 KiB pages: a FIL header of 38 bytes (checksum, page number, neighbours, LSN, type, space id) and an 8-byte trailer, CRC-32C by default; records with a 5-byte header and relative next-record offsets; `ROW_FORMAT=COMPRESSED` and page compression in zlib (or LZ4, with holes punched); redo in 512-byte blocks, reused in a circle; XtraBackup's xbstream, `--compress` with qpress (QuickLZ), LZ4 or zstd, incrementals as `.delta` pages by LSN | the checksums, page numbers and space ids derived; the records by column; the zlib pages; free space as it is | aqr-flate, aqr-lz4, aqr-sqlite's approach |
| pg_dump plain (`-Fp`), `pg_dumpall` | PostgreSQL backups, migrations | text: `COPY t (…) FROM stdin;` of TSV rows with `\N` and backslash escapes, ended by `\.` (or `INSERT`s with `--inserts`); `bytea` in hex (`\\x…`), timestamps with zones, arrays and JSON as text | the COPY rows as aqr-xsv takes TSV | aqr-xsv, aqr-num |
| pg_dump custom and directory (`-Fc`, `-Fd`) | PostgreSQL backups (`pg_restore`) | `PGDMP`, a table of contents, each table's COPY text as length-prefixed chunks of one zlib stream (lz4 or zstd from 16); `-Fd` writes `NNNN.dat.gz` per table; `-Ft` is a plain tar | the zlib streams, then as plain | aqr-flate, aqr-xsv |
| PostgreSQL data files, `pg_basebackup`, WAL | database servers, physical backups and their archived WAL | 8 KiB pages: a 24-byte header (LSN, a 16-bit checksum, on by default from 18), line pointers, tuples from the end with 23-byte headers (`xmin` alike in a page, `ctid` mostly pointing at itself); 1 GiB segment files; TOAST values in pglz or LZ4; WAL in 16 MiB segments with CRC-32C records and full-page images after checkpoints (`wal_compression`: pglz, LZ4, zstd); a segment switched early by `archive_timeout` stays 16 MiB; base backups as tar (gzip, LZ4 or zstd on the server from 15) with a JSON manifest; pgBackRest and WAL-G compress with gzip, LZ4, zstd, bzip2 or Brotli | the pages' derived fields and tuples by column; pglz (simple and open); full-page images against the base backup's pages | aqr-flate, aqr-lz4, aqr-sqlite's approach |
| Oracle Data Pump (`expdp`, `.dmp`), `exp` | Oracle backups and migrations | closed: metadata as XML-like DDL, rows in the direct-path stream, the job's master table included; uncompressed unless `COMPRESSION=ALL` (Advanced Compression's licence), so plain dumps are common; split by `%U` and `FILESIZE` | the row pieces by column, once the format is known (`expdp` and `impdp` the black boxes); the old `exp` format is simpler | aqr-infer-recs |
| Oracle datafiles, RMAN image copies, archived redo (`.arc`) | Oracle servers, physical backups | blocks of 8 KiB (2–32 KiB): a header with the block address and SCN, a 16-bit XOR checksum (`DB_BLOCK_CHECKSUM`), a 4-byte tail check from the SCN; redo in 512-byte (or 4 KiB) blocks with headers of sequence, block number and checksum | the derived fields and the rows by column, as InnoDB's; RMAN's backup sets as under Hard | aqr-sqlite's approach |
| Microsoft SQL Server `.bak`, `.trn` (uncompressed), `.mdf`, `.ldf` | SQL Server backups and detached databases | Microsoft Tape Format: descriptor blocks (`TAPE`, `SSET`, `VOLB`, `MSCI`, `MSDA`) and padding around 8 KiB pages almost as they are (96-byte headers, checksums by default, slot arrays; ROW and PAGE compression within them); differentials hold changed extents; the log in 512-byte sectors each stamped with parity bits (64 and 128 by turns as a VLF is reused); stripes over several files | MTF's blocks parsed, the pages' derived fields and rows by column, as InnoDB's; compressed backups (the default in many installations) as under Hard | aqr-sqlite's approach |
| SSMS "Generate Scripts", `.bacpac` | SQL Server exports | scripts in UTF-16LE by default: one `INSERT [dbo].[t] ([a], [b]) VALUES (…, CAST(N'…' AS DateTime))` per row, the column list repeated, `0x…` binaries; `.bacpac` is a ZIP of `model.xml` and BCP native data | UTF-16 to UTF-8 reversibly, the statements as a template and columns; the bacpac's entries | aqr-zip, aqr-xsv |
| SQLite `.dump`, WAL, rollback journals | SQLite backups | `.dump`: `INSERT INTO t VALUES(…)` per row, blobs as `X'…'`; WAL frames of a page and a 24-byte header with checksums | the dump as SQL text above; the WAL's frames as aqr-sqlite's pages (its README: left) | aqr-sqlite |
| MongoDB `mongodump` (`.bson`, `--archive`, `--gzip`), WiredTiger | MongoDB backups, servers | BSON documents with every field name repeated and int32 lengths, ObjectIds rising (time, machine, counter); `mongoexport` writes Extended JSON lines (aqr-jsonl); WiredTiger's blocks in Snappy by default (zstd or zlib optional) | the lengths derived, fields transposed by path; Snappy blocks | aqr-jsonl, aqr-flate, aqr-snappy |
| Redis RDB, AOF | cache snapshots and logs | RDB: LZF-compressed strings, a CRC-64 footer; AOF: RESP text (`*3\r\n$3\r\nSET\r\n…`), a base RDB and increments from 7 | LZF reproduced; RESP's lengths derived | |
| RocksDB and Cassandra SSTables, Kafka segments | databases, logs | Snappy or LZ4 blocks (zstd elsewhere); Cassandra's in LZ4 chunks of 16 KiB with CRCs | as LevelDB's tables | aqr-snappy, aqr-lz4 |
| ClickHouse parts and `BACKUP` | analytics | a file per column of LZ4 blocks (the default) with CityHash128 checksums; backups as directories, ZIP or tar | the LZ4 blocks, then the columns | aqr-lz4, aqr-zip |
| Firefox IndexedDB | browser storage | SQLite rows whose values are Snappy-compressed | the values through aqr-snappy inside aqr-sqlite | aqr-sqlite, aqr-snappy |
| Lucene, Elasticsearch and OpenSearch snapshots | search indices | stored fields in Lucene's own LZ4; snapshots reuse segments across generations | Lucene's LZ4 reproduced | aqr-lz4 |
| Microsoft Access (`.mdb`, `.accdb`) | small businesses, laboratories, government | Jet/ACE pages of rows; text optionally "Unicode compressed" (one byte a character) | the pages' rows by column, the free space dropped | aqr-sqlite's approach |
| Windows ESE (`Windows.edb`, Exchange, SRUM), registry hives | Windows' search index (often GBs), mail servers, the registry | ESE: pages of B-trees, some columns 7-bit or XPRESS-compressed; hives: `hbin` blocks of cells, uncompressed | the pages' and cells' records by field; ESE's XPRESS (Hard: Microsoft's encoder) | aqr-infer-recs |
| Firebird `gbak` (`.fbk`), etcd and BoltDB snapshots | smaller servers, Kubernetes | gbak: a logical dump of tagged attributes and records; BoltDB: 4 KiB pages of a B+tree, protobuf values, uncompressed | records by field | aqr-infer-recs |

### Science, medicine, geography

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| BAM, bgzip (VCF.gz, FASTQ.gz) | genomics, petabytes of it | BGZF: gzip members of 64 KiB one after the other (htslib's zlib or libdeflate) | aqr-compress takes runs of gzip members (`src/flate.rs`); not measured on BGZF, and libdeflate is not emulated (aqr-flate's README); fields split into streams (as CRAM) later | aqr-flate |
| DICOM (compressed) | medical imaging (PACS) | JPEG Lossless, JPEG-LS, RLE (uncompressed images are aqr-raw's) | lossless JPEG as in CR2 (`jpeg-lossless-recons` looks in TIFF files only) | aqr-jpeg's lossless model |
| NIfTI (`.nii.gz`) | neuroimaging | a gzip of the whole volume | a 3D model of the voxels | aqr-flate |
| mzML, mzXML | mass spectrometry | XML with base64 arrays of floats, optionally zlib | the m/z and intensity arrays modelled (base64 is aqr-b2t's) | aqr-b2t, aqr-flate |
| ROOT | particle physics (CERN) | baskets in zlib, LZ4, LZMA or zstd | the zlib and LZ4 baskets | aqr-flate, aqr-lz4 |
| GRIB2, simple packing | weather | integers packed at a fixed width | unpacked, then a predictive model | |
| SEG-Y | seismic surveys | uncompressed traces of IBM or IEEE floats | a trace model | |
| OSM PBF | OpenStreetMap extracts | zlib-compressed protobuf blobs | the zlib blobs | aqr-flate |
| MBTiles, PMTiles | map tiles | SQLite or a flat archive of PNG, JPEG or gzip'd vector tiles | the tiles through their recompressors | aqr-sqlite, aqr-png, aqr-jpeg, aqr-flate |
| FASTQ, SAM (uncompressed) | sequencers' output, alignments | text: reads' names, nucleotides, quality strings (FASTQ); tab-separated fields (SAM) | names by field against the read before, quality strings by a context model (fqzcomp's), nucleotides as aqr-fasta | aqr-fasta, aqr-xsv |
| MRC, cryo-EM movies | structural biology (TBs per session) | raw 8- or 16-bit counts or floats a frame (TIFF of LZW too) | a model of sparse counts against the frames before | aqr-fits's pixel model |
| SRTM `.hgt`, Esri shapefiles (`.shp`, `.dbf`) | elevation data, GIS | `.hgt`: 16-bit big-endian heights uncompressed; `.shp`: records of doubles; `.dbf`: fixed-width text records | the heights through `raster-recons-16`; the coordinates as deltas, the `.dbf` by column | aqr-raw, aqr-infer-recs, aqr-xsv |

### Web, logs and capture

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| WARC (`.warc.gz`) | web archives (Common Crawl, Internet Archive) | a gzip member per record | the HTTP bodies (gzip again, images) | aqr-flate, everything |
| uuencode, yEnc | Usenet binaries | text of 6 or 8 bits a byte, in lines | as aqr-b2t does base64 (its README: left) | aqr-b2t |
| Browser HTTP caches (Chrome, Firefox, Safari) | browser profiles | bodies as sent: gzip, Brotli, zstd; images | the gzip bodies and images (Brotli and zstd are hard, below) | aqr-flate, aqr-png, aqr-jpeg |
| systemd journal | Linux logs | field data over a size threshold in xz, LZ4 or zstd | the LZ4 objects; the journal's hash tables and offsets derived | aqr-lz4 |
| pcap, pcapng | network captures | uncompressed packets | packets modelled by flow and protocol; payloads recompressed | |
| Windows EVTX | event logs | chunks of binary XML with templates | the templates and records modelled | |
| Chromium `.pak` | every Electron app and Chromium browser | an index and resources, gzip or Brotli where compressed, PNGs | read by aqr-bundle's `chromium-pak` but not looked for, as it lost (Done table); the Brotli entries are Hard | aqr-bundle |

Rotated logs (`*.log.N.gz`, zlib's levels 6 or 9) are aqr-flate's gzip (Done).

### Version control

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| Mercurial revlogs | Mercurial repositories | zlib (or zstd) per revision, deltas against earlier revisions | as aqr-git: the zlib emulated, the revisions related | aqr-git, aqr-flate |
| Fossil | Fossil repositories (SQLite's own) | zlib-compressed artifacts in an SQLite database | the zlib, inside aqr-sqlite | aqr-sqlite, aqr-flate |
| Subversion FSFS | Subversion servers | svndiff deltas, zlib (svndiff1) or LZ4 (svndiff2) | the zlib and LZ4 | aqr-flate, aqr-lz4 |
| OSTree, Flatpak | immutable OS images, desktop apps | objects addressed by SHA-256, raw deflate in archive repositories | as aqr-git | aqr-git |

### Operating systems and distribution

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| LZFSE, LZVN containers | APFS and HFS+ transparent compression (decmpfs), DMG (ULFO), `.aar`, `.xip` | chunks of LZFSE or LZVN behind a table of their own | aqr-bvx takes whole streams (asset catalogs, `.lzfse`) and DMG's LZFSE chunks; left: decmpfs (raw LZVN chunks or streams behind its tables: aqr-hfs's and aqr-apfs's READMEs), `.aar`, `.xip` | aqr-bvx, aqr-hfs, aqr-apfs |
| xar (`.pkg`) | macOS installers | a zlib table of contents; a payload in gzip'd cpio or pbzx (xz chunks) | the zlib and gzip parts | aqr-flate |
| AppX, MSIX, NuGet | Windows apps, .NET packages | ZIP; AppX adds a block map of SHA-256 hashes | the block map derived (the ZIP is aqr-zip's) | aqr-zip |
| squashfs, initramfs, Android sparse images | firmware, Linux distributions | gzip, xz, zstd, lz4 or LZO blocks | the gzip, lz4 and LZO blocks | aqr-flate, aqr-lz4, aqr-lzo |
| OCI and Docker layers | container registries | tar.gz by Go's `compress/flate` | Go's encoder emulated in aqr-flate (flate-zoo has it, as an oracle); the tar is aqr-bundle's | aqr-flate, aqr-bundle |
| UEFI firmware volumes | PC firmware | EFI/Tiano compression (LZ77 with Huffman) or LZMA | Tiano's reference encoder (EDK II) reproduced | |
| Intel HEX, Motorola S-record | microcontroller firmware | hex text with per-line checksums | the bytes, with addresses and checksums derived | aqr-b2t |

### Executables and bytecode

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| ELF (x86-64, ARM64) | Linux and Android binaries | uncompressed (debug sections optionally zlib or zstd) | the tables (relocations, symbols, `.eh_frame`); compressed debug sections (aqr-dwarf takes `.debug_info` and Apple's tables) | aqr-exe, aqr-flate, aqr-dwarf |
| Java class files | JARs | uncompressed constant pools and bytecode | as DEX | aqr-dex's approach |
| Python bytecode (`.pyc`) | every Python installation | uncompressed marshal of code objects | the constants and names against the source beside it; the bytecode by field | aqr-wasm's approach |
| SPIR-V, DXBC/DXIL shaders | games' shader caches and packages | uncompressed 32-bit words (SPIR-V); LLVM bitcode in a container (DXIL) | SPIR-V's ids and opcodes by field | aqr-wasm's approach |

### Games

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| PVRTC2, ASTC of 3D footprints; textures of real mobile games | mobile games | fixed-rate block compression, no entropy coding | as aqr-tex does the other block formats (its README's "Next": the sample of other formats is made by black-box encoders) | aqr-tex |
| Unity AssetBundle (LZ4HC, LZMA blocks) | Unity games | LZ4 chunks (LZ4HC as Unity writes it) or LZMA | the LZ4HC chunks reproduced, which may not pay: 941.2 MB of LZ4HC blocks of 184 bundles of 10 games hold 12.09 GB of data, whose zstd -3 is 6.0% larger than the blocks (847.2 against 799.3 MB, 2026-11-13) | aqr-lz4 |
| Unreal `.pak` with zlib | Unreal Engine 4 games (before Oodle) | zlib per compression block | the zlib blocks as streams of their own, rather than found by the scan | aqr-flate |
| Bethesda BSA, BA2 | Skyrim, Fallout | zlib or LZ4 per file | the packs are read (`bethesda-bsa`, `bethesda-ba2`, unverified on real files); their zlib and LZ4 files | aqr-flate, aqr-lz4 |
| Ren'Py `.rpyc` | visual novels | compiled scripts in zlib (`.rpa` is read: `renpy-rpa`) | the zlib | aqr-flate |
| RPG Maker MV and MZ assets | indie games | PNG and OGG with their first 16 bytes XORed by a key in `System.json` | the header restored, then aqr-png or the Vorbis work | aqr-png |
| Minecraft: Bedrock worlds; DH's LODs | server backups | LevelDB with raw deflate (Bedrock) | Bedrock; the coarser LODs of children after their parents (aqr-minecraft's README) | aqr-minecraft, aqr-snappy's LevelDB tables |
| CD images (BIN/CUE, IMG) | PlayStation, Saturn and other disc images | none; each 2352-byte sector carries EDC and ECC computed from its data | sync, EDC and ECC regenerated (as ECM does): about 12% of a data sector | |
| CSO, ZSO | PSP and PS2 ISOs | deflate (CSO) or LZ4 (ZSO) per block | the blocks | aqr-flate, aqr-lz4 |
| CRI ADX, HCA, USM; CPK's CRILAYLA | Japanese games | ADX: 4-bit ADPCM at fixed width; HCA: MDCT with static tables; USM: MPEG-1, VP9 or H.264 in its own container; CRILAYLA an LZ of its own (AWB, ACB, CPK, AFS are read: aqr-bundle's `cri-*`) | ADX's samples modelled (a large gain); CRILAYLA reproduced | aqr-ima's approach |
| Valve VTF | Source engine games | DXT1/DXT5 textures with mipmaps | the textures as aqr-tex does DDS | aqr-tex |
| XNA, MonoGame `.xnb` (compressed) | indie games | XMemCompress's LZX (XNA) or LZ4 (MonoGame) | the LZ4 reproduced (stored files are read: `xna-xnb`). The LZX does not pay: of 389 files of A Virus Named TOM (48.1 MB), their data (855.8 MB) archives to 44.48 MB and the record is 7.39 MB, against 47.33 MB for the files as they are (2026-11-13) | aqr-lz4 |
| GameCube and Wii disc images | emulation | none; padding of junk from a known generator; Wii's partitions encrypted with known keys and hashed | the junk regenerated (as RVZ does), Wii's data decrypted and its hashes derived | |

### Older archives

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| Unix `compress` (`.Z`, `.tar.Z`) | old source and FTP archives, some scientific data | LZW of 9 to 16-bit codes, reset when the ratio drops; one encoder (ncompress) | the encoder reproduced (its reset rule) | aqr-gif's LZW |
| ARJ | DOS-era archives, BBS and Korean PC-communication files | LZSS with Huffman, close to LHA's (methods 1–3), and a simpler method 4 | a record of the parse, as aqr-lha | aqr-lha |
| StuffIt, Compact Pro, ZOO, ARC | classic Mac and DOS archives | LZW, Huffman and LZH methods (StuffIt 5's Arsenic is arithmetic: Hard) | the LZW reproduced, the rest as records of the parse | aqr-gif's LZW, aqr-lha |
| AppleDouble (`._*`, `__MACOSX` in ZIPs), MacBinary, BinHex | files from Macs | headers around resource forks and Finder information; BinHex as 6-bit text | the forks' resources (icons, `snd `) recompressed; BinHex's text undone | |

## Hard

### Strong entropy coders

They already code arithmetically or adapt to context, so remodelling leaves little, and
reproducing their coding is expensive.

| format | used in | why it is hard |
|---|---|---|
| H.264 (CABAC), HEVC, AV1, VVC | nearly all distributed video | context-adaptive arithmetic coding is already close to the entropy; gaining more needs prediction and motion compensation redone, a whole decoder. H.264 CAVLC is the exception (above) |
| VP8, VP9, WebP (lossy) | YouTube, WebRTC, web images | a boolean arithmetic coder with coefficient contexts and probabilities updated per frame; reproducible, but little left. Videos of a still picture repeat their keyframes byte for byte, which aqr-mkv holds once without decoding |
| FFV1, Lagarith | archival video (FFV1 is a museum and library standard) | lossless coders with adaptive range coding and context modelling already |
| JPEG 2000 (DCP, DICOM, PDF's JPX) | digital cinema, medicine | EBCOT with the MQ coder (adaptive arithmetic), and many encoder choices (code-block bit-plane order, layers, truncation) |
| JBIG2, DjVu | scanned documents, PDF | adaptive arithmetic coders (MQ, ZP) over context models; JBIG2's symbol dictionaries are encoder choices too |
| JPEG XL, WebP lossless, AVIF, HEIC | recent images | ANS or arithmetic coding with context modelling already; HEIC brings HEVC's problems |
| JPEG-LS | DICOM, some RAW | context-adaptive Golomb with bias correction: already near what a model gains |
| LAZ, Draco, meshopt | lidar, web 3D | already predicted and entropy coded |
| CRAM, Monkey's Audio, OptimFROG, WavPack | genomics, audiophiles | lossless coders with strong models already: little is left |

### Encoders to reproduce

The bitstream is simple, but the same bytes need the encoder's decisions, and the encoder is
not available or comes in too many variants.

| format | used in | why it is hard |
|---|---|---|
| LZMS (ESD, WIM); XPRESS Huffman alone (Windows prefetch, `hiberfil.sys`) | Windows install images | Microsoft's encoders are closed and their parsers optimal-style; predicting and correcting (as aqr-flate does) leaves large corrections. LZX, LZNT1 and WOF's XPRESS are records of the parse (Done), which pay where the data gains or is shared |
| Oodle (Kraken, Mermaid, Leviathan) | AAA game paks | a closed commercial library with many versions and levels |
| xz, LZMA, 7z alone (also `.deb`, `.rpm`, Arch packages, pbzx) | source and package distribution | optimal parsing over many versions and options, and little to gain over an already strong coder: aqr-lzma's records (10–18% of the streams) pay only where the data is stored elsewhere too (Done) |
| zstd, Brotli (also WOFF2, HTTP bodies, Chromium `.pak` entries) | HTTP, packages, file systems | output changes between versions; levels, windows and dictionaries make a wide space; the entropy stage is already good. aqr-zstd keeps a `.zst`'s frames and gives their content to the files of the same, and emulates level 3 for frames in other data (Done) |
| CHD, RVZ, WIA, NSZ | disc and cartridge images for emulators | per-hunk choices among zlib, LZMA, zstd, FLAC and Huffman; the LZMA and zstd ones as above |
| SQL Server's compressed backups, Oracle RMAN backup sets and compressed Data Pump, DB2's `COMPRESS` | database backups | SQL Server's default `MS_XPRESS` is Microsoft's closed encoder; 2022's `QAT_DEFLATE` is DEFLATE by QATzip and ISA-L, whose encoder would have to be matched; RMAN keeps only used blocks in a closed container, its BASIC, LOW, MEDIUM and HIGH said to be bzip2, LZO, zlib and bzip2; DB2's is closed |
| Android OTA payloads | system updates | bsdiff and puffin operations over xz, bzip2, zstd or Brotli blobs |

### Other reasons

| format | used in | why it is hard |
|---|---|---|
| Opus | streaming, voice (Discord, WhatsApp, YouTube's WebM audio) | reproducible (a range coder over mostly static tables, and parsing needs no synthesis), but the PVQ pulses are coded uniformly and fine energy as raw bits, so little is left (Balrogg: 3–8%), for a whole CELT and SILK parser with its bit allocation |
| DRM-protected media, encrypted archives and backups | commercial content; iOS, WhatsApp and Telegram backups; databases under TDE and their encrypted backups | random data without the key; only files that carry their key can be done (PDF with an empty user password: aqr-pdf, Done) |
| Console packages (Switch NCA, PlayStation PKG), Unreal `.pak` with encryption | games | platform encryption and signatures on top |
| AutoCAD DWG, Native Instruments Kontakt (NCW), Bink video, ECW, MrSID | CAD, sample libraries, game cutscenes, aerial imagery | closed formats that change with versions; need reverse engineering first |
| Blockchains (Bitcoin's `blk*.dat`) | full nodes (hundreds of GB) | mostly hashes, public keys and signatures, which are random; the scripts' templates and amounts are a few percent |
