# Aqueous: recompressors

Where each lossless recompressor stands, what it measures, and the formats not taken yet.
Formats are specified in each crate's `FORMAT.md`, archives and formulas in `spec/index.bs`,
and the design is argued in `xform-design.md`.

## How it was measured

- Ratios: 2026-11-01, through `aqr-show` by `scripts/format-ratio.py` (its `sample` draws the
  samples below, its `measure` runs `aqr-show` as the Ratio column tells), at commit `54cddfe`
  (aqr's code as at `83097ad`), level `mid`. On an Apple M4 on one thread (enwik9 on 10, hg19
  on 4), but for aqr-clip, aqr-tex, aqr-vorbis, aqr-prores and the Hangul text: on a
  Ryzen 7 9700X with `-t 16`. Container formats (ZIP, gzip and the like) were not measured
  again: what they gain is what their contents are.
- Speeds and memory, as follows (not measured again with the ratios):
- 2026-10-23, Apple M4, one thread (`-t 1`), at commit `2abe944`.
- Through `aqr-show` (`crates/aqr/src/bin/show.rs`): each file is archived (recompression, archive
  coding and the round-trip check) at level `mid`, then extracted.
  - aqr-obj was measured with `aqr-obj bench` before it was wired into aqr-compress.
  - aqr-git, aqr-lz4, aqr-lzo and aqr-snappy: see their rows.
- aqr-jpeg, aqr-png, aqr-psd and aqr-mp3 were re-measured on 2026-10-24 (otherwise as above;
  aqr-mp3's ratio and speed again on 2026-10-25; aqr-flate's speed on 2026-10-25, its memory
  not re-measured),
  and aqr-raw's images measured then; aqr-exe too, on copies of files from a Windows
  machine.
- The memory of aqr-raw, aqr-flac, aqr-pdf, aqr-sqlite, aqr-obj, aqr-zip and aqr-exe was
  re-measured on 2026-10-24 at `da98d11`. aqr-sqlite (whose system databases have changed
  since) and aqr-obj were re-measured in full then, on samples taken anew, and so were
  aqr-raw's images, made again as its README tells.
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
    `aqr` is timed by testing, `aqr -K` (`-t` before 2026-11-04).
  - **Memory**: peak RSS, for one process per file that compresses, checks and decompresses
    it. It includes the input, the archive and the output, so its floor is about 2× the file.
    Given as a linear fit over the sample's files, fixed MB + k × input; files far over it
    are noted. macOS's allocator keeps freed large blocks resident for reuse,
    and they count: with that cache off (`MallocLargeCache=0`), aqr-exe's sample takes
    17 MB + 5.3× instead of 26 MB + 7.9×, and the largest PDF of aqr-pdf's corpus (179 MB)
    954 MB instead of 1.38 GB.
    The modules' own memory is a plan, exact to the page, since the part modules of 2026-11 (aqr-png, aqr-mp3,
    aqr-flac, aqr-raw's `pcm-recons`, aqr-bz2, aqr-lz4, aqr-snappy, aqr-git, aqr-exe, aqr-fasta, aqr-bundle,
    aqr-lzma and others hold a window, not the data): the fits above, measured before, are the peak of the process.

## Done

### Images

| format | crate | sample | ratio | zstd -3 | comp. MB/s | decomp. MB/s | memory / input | notes |
|---|---|---|---|---|---|---|---|---|
| PNG, APNG | aqr-png | 379 files, 391 MB | 0.802 ~ 0.855 | 0.997 | 42 | 90 | 7 MB + 10.3× | the data, the raw image and the tokens are held a window at a time; the encoder's symbols (2 bytes per byte of data on photos) are what still follows the pixels |
| GIF | aqr-gif | the 488 GIF files of the 10gb corpus, 9.25 MB (README) | 0.528 ~ 0.570 (0.987 before) | 0.989 | 2.7 (`aqr`, 10 threads) | 4.8 and 3.5 (`-K`, 10 threads; the second as Wasm) | 0.5 GB (`aqr` peak) | speed and memory measured 2026-10-31 with `aqr`, not `aqr-show` (all of them in one archive: 0.522). The module (14.5 KB) does not pay alone for most of these small files: 474 of 488 kept raw (worst), 173 without it (best). No encoder reproduced: the codes against the longest match (a few bytes a file), the pixels by a context-mixing model of palette indices at 3.5 (photographs) to 15 (animations) megapixels a second. The model's tables are up to 32 MB a file |
| JPEG | aqr-jpeg | 96 files, 186 MB | 0.778 ~ 0.794 | 0.913 | 39 | 82 | 28 MB + 8.2× | sequential files are coded a stripe at a time: a 1.3 MB 7680×4320 file takes 54 MB. Progressive ones still hold their coefficients whole: 259 MB for a 1.4 MB one of that size |
| BMP, PGM, PPM, PAM | aqr-raw | 17 files, 51 MB | 0.245 ~ 0.248 | 0.444 | 71 | 303 | 21 MB + 5.5× | 8-bit samples; not a sample of real files (PNGs of aqr-png's corpus converted, README of aqr-raw; made again for the ratio, from a corpus grown since). 16-bit PNM and TIFF files are taken by `raster-recons-16` since 2026-11-10 (README of aqr-raw); RLE images are not |
| PSD, PSB | aqr-psd | 37 files, 424 MB | 0.230 ~ 0.236 | 0.419 | 45 | 127 | 28 MB + 5.4× | pixels coded in 8-bit documents only; aqr-psd itself takes a few MB besides its streams, the rest is the archive's (a file kept raw is scanned by the parts aqr-psd tells) |
| Clip Studio Paint (`.clip`) | aqr-clip | 13 files, 926 MB | 0.339 ~ 0.340 | 0.760 | 35 | 91 | not measured | measured 2026-10-28 on a Ryzen 7 9700X with `aqr-show -t 16` (16 threads, not one); the tiles (zlib level 1, memLevel 9, all made again exactly) by a model of whole pixels in batches of 64 decoded as tasks (a tile equal to an earlier one a reference), the database by aqr-sqlite, the rest (13 MB of one file's 3D data) by the archive; aqr as it was: 0.726. A held-out file came to 0.454 against 0.763 (0.499 before the references; README of aqr-clip). The database's PNG preview is not recompressed |
| DDS (BC1–BC5) | aqr-tex | 1275 files of The Crew's textures, 400 MB | 0.342 ~ 0.376 | 0.486 | 23 | 20 | not measured | the ratio of a sample drawn with no least size (`sample --min 0`; of 256 KiB or more, 423 files: 0.345 ~ 0.366); the module (19 KB) does not pay alone for small files. Speeds measured 2026-10-28 on a Ryzen 7 9700X with `aqr-tex-bench` (the module a thread, not `aqr-show`) on a sample of 1274 files, 402 MB (0.347 then); all 52,181 of them (16.4 GB): 0.325 against zstd -19's 0.403 (README of aqr-tex). Large surfaces are coded in stripes decoded at once: the largest files (2.8–5.6 MB), one at a time, 23 → 97 MB/s on 16 threads. On an M4 after the coding was made faster (2026-11-01): 31.9 / 27.1 MB/s on a thread (26.6 / 20.9 before). Uncompressed DDS goes to aqr-raw's `raster-recons`; BC6H, BC7 and KTX are not taken yet |

### Audio

| format | crate | sample | ratio | zstd -3 | comp. MB/s | decomp. MB/s | memory / input | notes |
|---|---|---|---|---|---|---|---|---|
| MP3 | aqr-mp3 | 394 files, 400 MB | 0.852 ~ 0.915 | 0.968 | 45 | 116 | 61 MB + 6.4× | see below |
| Vorbis | aqr-vorbis | 4 Ogg Vorbis files of libvorbis (2008), 29.1 MB; 144 FSB5 sound banks of The Crew, 401 MB | 0.919 ~ 0.925; 0.845 ~ 0.853 | 0.999; 0.955 | 55 (`aqr`, 10 threads) | 158 and 117 (`-K`, 10 threads; the second as Wasm) | 0.66 GB heap peak (`aqr`) | the ratios measured 2026-11-01 on a Ryzen 7 9700X with `aqr-show -t 16`, of the files in their containers (aqr-ogg, aqr-fsb); speed and memory measured 2026-10-31 with `aqr` on 12 Ogg files made from aqr-flac's corpus by libvorbisenc with libogg and by ffmpeg (49.1 MB, FORMAT.md of aqr-ogg; 0.932 then), which were not kept. The audio packets coded in pieces of 2 MiB |
| FLAC; WAV, AIFF | aqr-flac; aqr-raw | 25 files, 450 MB | 0.946 ~ 0.948 | 0.999 | 65 | 165 | 9 MB + 11.5× | the module is about 31 KB per archive, the resource 2.4 KB. Measured when aqr-flac took WAV and AIFF too: aqr-raw has them now, with the same streams |
| IMA ADPCM (Xbox ADPCM in FSB5, Microsoft's in WAV) | aqr-ima | The Crew's FSB5 banks; IMA ADPCM WAV files of ffmpeg and sox (README) | the record 0.578 of the blocks (xz -9e about 0.67); WAV 0.58–0.61 stereo, 0.78 mono | | 33 (one thread) | 25 (one thread) | not measured | the headers and nibbles by binary arithmetic coding in contexts of the decoder's state (step index, the nibble before, the other channel's nibble); a channel equal to the one before is one decision. Through aqr-fsb and aqr-raw |

The MP3 resource is the built-in tables (131 KB raw, about 33 KB in every archive), and the
module about 28 KB more: a typical 1 MB track alone (worst) pays for both, and 23 of the
sample's 394 tracks are kept raw. The speeds are those of the files coded
(`-r`); the memory is a fit over concatenations of the sample (9 to 120 MB, each coded as one
file with `-e`), as the sample's files are all about 1 MB (32–59 MB each, median 41 MB). The
fixed part is mostly the tables. Before the contexts shared 500 tables, the resource was
1.5 MB raw and 340 KB archived, no file of the sample was coded alone (0.975), and the same
measurements gave 20 / 104 MB/s and 28–52 MB more memory. It was about 140 MB + 14.7× before
the rANS symbols were held a run of 1024 frames at a time and the output assembled as the
reservoir is rebuilt.

### Documents, data, 3D

| format | crate | sample | ratio | zstd -3 | comp. MB/s | decomp. MB/s | memory / input | notes |
|---|---|---|---|---|---|---|---|---|
| PDF | aqr-pdf | 123 files, 456 MB | 0.822 ~ 0.836 | 0.918 | 46 | 198 | 41 MB + 5.7× | files kept as they are (no formula paid): 73 alone (worst), 5 without the modules (best). Speed and memory of an earlier sample (144 files, 402 MB). 9 files take over twice the memory fit, up to 331 MB for 3.4 MB (`cc/0000192.pdf`) and 202 MB for 1.8 MB |
| SQLite 3 | aqr-sqlite | 88 files, 324 MB | 0.210 ~ 0.219 | 0.366 | 48 | 368 | 24 MB + 6.7× | |
| Text (words, XML markup) | aqr-wrt | enwik9, 1 GB (README) | 0.233 ~ 0.235 | 0.312 | 148 | 291 | not measured | speeds measured 2026-10-26 with `aqr` (10 threads) on enwik9 alone, not `aqr-show` (0.253 then); text of 256 KiB or more. The 10gb corpus −35.8 MB (README) |
| Text of Hangul (Korean) | aqr-wrt (`wrt-recons-hangul`) | namuwiki, 512 MiB of 8.8 GB (README) | 0.261 ~ 0.267 | 0.349 | 175 | 1280 | 4.2× | the ratio of the middle 512 MiB cut again at level mid; speeds measured 2026-10-29 with `aqr -5` (10 threads), not `aqr-show`, on a cut of its own (0.272 then, zstd -3 0.358); 0.307 before (zstd -9 on it raw). Syllables in two bytes, words of Hangul by codes |
| FASTA (nucleotides) | aqr-fasta | hg19, 93 files, 3.2 GB (README) | 0.209 | 0.309 | 170 | 133 (753 through `aqr`, files at once) | not measured | speeds measured 2026-10-26 with `aqr-fasta-bench` (chr1) and `aqr` (hg19 of the 10gb corpus), not `aqr-show`; zstd -3 file by file |
| Delimited tables (CSV, TSV, USV) | aqr-xsv | 10 files, 11.7 MB (README) | 0.792 of `zstd -3` (streams through zstd -3) | | 39–479 | 584–1219 | not measured | measured 2026-10-28 with `aqr-xsv-bench`, not `aqr-show`: GPS logs 0.41 of zstd -3, cmz's `airport-codes.csv` 0.82 (2.74 → 2.32 MB through `aqr`), perl's `UnicodeData.txt` 0.91. Taken by its text before aqr-wrt. The 10gb corpus −1.71 MB (85 files; README) |
| JSON lines | aqr-jsonl | 7 files, 2.27 GB (README) | 0.872 of `aqr` before (through `aqr`) | | 179–1012 | 741–3080 (one thread) | segments of 64 MiB: decompressing 2.24 GB, 376 MB of heap | measured 2026-11-01 through `aqr` (10 threads) and `aqr-jsonl-bench`, not `aqr-show`: GH Archive −8.4%, Amazon reviews −18.1%, OpenAlex −27.8%, GeoJSON −15.1%, event logs −37.6%, C4 −1.5%. Taken by its text before aqr-xsv and aqr-wrt |
| FITS (images, Rice tiles, binary tables) | aqr-fits | 68 files, 96.2 MB: SDO/AIA (Rice), Digitized Sky Survey (16-bit, and through `fpack`), Kepler tables, NASA's and SDSS's samples, a Legacy Survey image (README) | 0.584 (0.715 before) | | 7.1 (`aqr -c -T 1`) | 22 | 299 MB peak for the 11.1 MB Legacy Survey file | measured 2026-10-28 with `aqr`, not `aqr-show`, one archive per file. Integer images by prediction or an H-transform pyramid (DSS 0.505 → 0.172), Rice tiles rebuilt from their pixels and their blocks' codes (AIA 1.000 → 0.937), tables as byte planes of their columns (Kepler 0.591 → 0.307); `GZIP_*`, `HCOMPRESS_1` and `PLIO_1` tiles kept |
| HDF5 (netCDF-4, NeXus, NWB, AnnData, Keras) | aqr-hdf5, aqr-flate | 60 files, 271.2 MB: GWOSC strain, GOES-16, NOAA OISST, Keras weights, 10x and AnnData, NWB, NeXus, test files (README) | 0.728 (default level), 0.704 (`-9`); 0.826 before | | 8.6 (`aqr`, 16 threads, a file at a time) | | not measured | measured 2026-10-30 on Windows with `aqr`. The deflated chunks rebuilt by `flate-set` (zlib's `compress2`, emulated exactly from level 7), floats by a linear predictor of order 32 (`hdf5-recons-float`, GWOSC 1.000 → 0.799); LZF, Blosc, szip and zstd chunks kept |
| GNU gettext catalogs (`.mo`) | aqr-gnu-gettext | the 10gb corpus's 1,171 distinct `.mo` files, 79.66 MB (README) | 10.98 MB archived together (13.70 before, −19.8%) | | 68 (`aqr`, all threads; 1.82 s → 1.17 s) | | not measured | measured with `aqr -c` at level mid; the offsets, lengths and hash table derived from the strings, the rest kept for the archive |
| Wavefront OBJ | aqr-obj | 14 files, 411 MB | 0.080 ~ 0.081 | 0.232 | 121 | 324 | 24 MB + 3.1× | files over 300 MB left out. Speed and memory by `aqr-obj bench`, before the wiring, on 22 files of 403 MB (0.097 then). Through `aqr` (all threads, 2026-10-28): c3d 67.5 → 9.99 MB (19.94 before the wiring), threejs 6.0 → 0.83 MB (1.50). Taken by its text, not by the name `.obj` (object files) |

### Video

| format | crate | sample | ratio | zstd -3 | comp. MB/s | decomp. MB/s | memory / input | notes |
|---|---|---|---|---|---|---|---|---|
| Apple ProRes (QuickTime, MXF) | aqr-prores | 33 files, 16.89 GB: ARRI cameras (MOV and MXF), an Apple-encoded film, an SD capture, ffmpeg's encoders, FFmpeg's samples (README) | 0.676 (0.990 before) | 0.919 | 46 (`aqr`, 16 threads) | 130 (`-K`, 16 threads) | the archiver's (it holds the largest file whole) | measured 2026-11-01 with `aqr` on a Ryzen 7 9700X. The coefficients under a context model with the previous frame as context (most of the gain: 0.830 without it), in chunks of 64 MiB whose stripes (one for each 4 MiB) decode as tasks; nothing reproduced but the coding. A chunk codes at about 15 MB/s on a thread and decodes at 13. The largest file (`NineEnProres01.mov`, 4.58 GB, 27% of the set) is found to be ProRes but not recoded: the archive keeps it as one stream (0.461) |

### Containers and general-purpose compression

| format | crate | sample | ratio | zstd -3 | comp. MB/s | decomp. MB/s | memory / input | notes |
|---|---|---|---|---|---|---|---|---|
| gzip, zlib | aqr-flate | 4 files, 4 MB | 0.932 | 0.977 | 16 | 58 | 34 MB + 35.6× | a small sample; 2 of 4 not coded, and since 2026-10-25 not deconstructed either (a probe of what the stream may gain): 6.7 and 8.0 to 50 and 33 MB/s. 153 gzip files of this machine (280 MB): 0.881 at 9.0 MB/s (README of aqr-flate). Per-stream figures on 924 streams are in aqr-flate's README |
| ZIP | aqr-zip | 5 files, 327 MB | 0.954 | 0.998 | 8 | 23 | 48 MB + 13.8× | dominated by enwik9.zip (322 MB, 1 GB of text: 4.5 GB peak when measured; `flate-set` 3 since holds no stream whole, 108 MiB per application); two Python wheels come to 0.76. Since 2026-10-25 large bodies are rebuilt only where a probe says they may pay: 76 ZIP files of this machine (267 MB: wheels, JARs, Office files, Go modules, test corpora) 0.7799 -> 0.7795 at 10.8 -> 12.6 MB/s (README of aqr-zip) |
| mozlz4, LZ4 frames | aqr-lz4 | a Firefox profile (README) | −9 to −18% against `zstd -3` of the files, −57% solid | | | 650–1150 (the compressor) | not measured | no sample on this machine; liblz4 1.10.0's fast compressor reproduced exactly |
| lzop files (LZO1X) | aqr-lzo | Silesia (12 files) through `lzop -3` and `-9`, 101.0 and 76.3 MB (README) | 0.622 and 0.821 | 0.840 and 0.923 | 16 and 10 (`aqr`, 10 threads) | 94 and 41 (`-K`, 10 threads; the second as Wasm) | 13.5× (`aqr` peak) | measured 2026-10-28 with `aqr`, not `aqr-show`; the original files archive to 57.8 MB: the records (4.5% and 6.1% of the streams) are most of the rest. No compressor reproduced: data + record of the parse |
| LZFSE, LZVN (bvx blocks) | aqr-bvx | the 36 asset catalogs over 100 KB of `/Applications`, 272.5 MB (README) | 0.701 (0.809 before) | 0.854 | 4.9 (`aqr`, 10 threads) | 43 and 34 (`-K`, 10 threads; the second as Wasm) | 28.5× (`aqr` peak, 7.8 GB) | measured 2026-10-30 with `aqr`, not `aqr-show`, with the streams taken at level mid (now level 9 only: 170.57 MB at `-l 9`, 146 s); 4,781 LZFSE and 10,200 LZVN streams, 3.7 GB of data. No compressor reproduced: data + record of the parse (1.5% and 2.5% of the streams), with the LZFSE compressor's table of candidates as a context. The data (pixels) is stored by zstd; some catalogs' streams are kept |
| LZMA (`.lzma`), xz (LZMA2) | aqr-lzma | the 10gb corpus's 123 `.tar.lzma` (73.08 MB, XZ Utils `-6`) and 6 `.tar.xz`; a 7z folder of 7-Zip; a tar through `xz -6 -T1` and `-T4` (README) | records 13.5% of the `.tar.lzma` (10.1% of the `.tar.xz`, 15.5–18.4% of the others) | | 39 of data (`aqr-lzma bench`, a thread) | 34 of data (a thread) | natively the data, the stream, 12 × the dictionary of chains and 132.5 MiB of tables (1.39 GB peak for the 7z folder, measured so); the modules are part modules of a plan of 118 MiB for an 8 MiB dictionary, 186 MiB for 7-Zip's 64 MiB (README of aqr-lzma) | measured 2026-11-02 on an M4. No encoder reproduced: data + record of the parse against a binary-tree match finder; streams over 64 MiB of data in parts coded side by side. For streams whose data is stored elsewhere too (a `.tar.lzma` beside its files: 96.5 → about 14 MB); a stream alone gains nothing (Hard, below). Blocks of filters before LZMA2 by aqr-bcj |
| 7z (LZMA, LZMA2, Copy, BCJ, BCJ2, Delta) | aqr-7z, aqr-bcj, aqr-lzma | the 10gb corpus's four 7z archives, 37.25 MB (README) | records 17.5% of the packed streams; `mingw480` (its tree and its 7z) 84.45 → 55.36 MB (the tree alone 47.36) | | 30 of data (`aqr-7z-check -t 4`) | 24 of data | 4.96 GB peak for `mingw480` (1.44 before) | measured 2026-10-30 on an M4; no module of its own: the folders' streams rebuilt from the files' contents by `lzma-recons` and `lzma2-recons`, the filters by aqr-bcj's modules (ported from the LZMA SDK; BCJ2's bits of conversion kept from its range coder's stream). PPMd, BZip2 and Deflate folders and encrypted archives kept |
| bzip2 | aqr-bz2 | 79 files of this machine, 352.8 MB (README) | 0.726 (0.88 for a source tar, 0.85 for a tar of binaries); text alone kept | | 3–5 (`aqr`, 16 threads) | 16 (`-K`, 16 threads) | 30 B per symbol of a block, on each thread | measured 2026-10-31 on Windows with `aqr`, not `aqr-show`; libbzip2's compressor reproduced exactly (149 of 153 files); the gain is what the data holds, BWT already models text well |
| Ogg | aqr-ogg | | | | | | | the pages from the packets (layout about 5 bytes a page before compression), the Vorbis packets by aqr-vorbis (Audio). Opus and FLAC in Ogg are not taken |
| FSB5 (FMOD sound banks) | aqr-fsb | | | | | | | the Vorbis samples' setup headers found by their CRC-32 in aqr-fsb's database, the samples recoded by aqr-vorbis (Audio), IMA ADPCM by aqr-ima |
| ar, tar, cpio | aqr-bundle | the 10gb corpus (README) | 2810.8 → 2775.0 MB for all of it | | 6600 (the module) | 28000 (the module) | not measured | measured with `aqr` and `aqr-bundle-check`, not `aqr-show`: an archive alone gains nothing, its files are recompressed by their formats and shared with the same files archived along (`gimp.tar` and its files −11.3 MB); ar archives of objects +1.0 MB |
| asar (Electron) | aqr-asar | 27 archives of one machine (README) | 714.42 → 697.11 MB for all of them (with aqr-gif) | | 950 | 4100 (the module) | not measured | an archive alone gains little (headers at zstd -3 2.13 → 0.84 MB); its files are recompressed by their formats; archives of many small files (`node_modules`) lose alone, as their text is a stream apart (aqr-compress's kinds) |
| ISO 9660, UDF images (all of an image, or on a partition of MBR, GPT, APM) | aqr-iso, aqr-disk | a Windows Server 2008 R2 DVD, 3.17 GB (README) | 2777.6 → 2776.9 MB | | 177 (`aqr`, all threads) | | 1.82 GB peak for 3.17 GB (2.04 before) | measured 2026-10-30 with `aqr`; no module (the image is its files' contents and the rest); 89% of the DVD is `install.wim` (LZX), which it gives as a file of its own |
| WIM (LZX, stored) | aqr-wim, aqr-lzx | the same DVD's `install.wim` (2.81 GB) and `boot.wim` (README) | the DVD: 3.17 GB → 2483.1 MB | | 19 (`aqr`, 16 threads, the whole DVD) | 45 (extracting) | 22.9 GB peak commit compressing, 1.72 GB extracting | measured 2026-10-30 with `aqr`; no module of its own (applications of `lzx-recons` and slices); the LZX chunks' data (6.86 GB for `install.wim`) with records of 6.74% of the chunks, recompressed together rather than 32 KiB at a time |
| CAB (MSZIP, LZX, stored) | aqr-mszip, aqr-flate, aqr-lzx | 441 CAB files of Windows, 1.80 GB (README) | 1662.7 → 1331.3 MB for all of them | | 23 (`aqr`, all threads) | 84 (`-K`) | 3.92 GB peak footprint (1.29 before) | measured 2026-10-31 with `aqr`; MSZIP folders as one DEFLATE stream each (the blocks joined), rebuilt by aqr-flate (records 4.6% of the streams at profile 6, 1.2% at 10); LZX folders by aqr-lzx (records 7.9%) from level 9 only: at level mid −1.1% more for 73.7 → 290 s, as an LZX CAB alone gains little (40 of them with the files they hold −33%) |
| CHM | aqr-mszip, aqr-lzx | 139 help files, 90.9 MB (README) | 88.36 → 69.23 MB at level 9 | | 7.0 (`aqr -9`, all threads) | 19 (`-K`) | 1.44 GB peak footprint | measured 2026-10-31 with `aqr`; from level 9 only (LZX); each LZX reset interval a stream, records 4.9% of the content |
| ALZ (ALZip) | aqr-alz, aqr-flate, aqr-bz2 | 16 archives, 7.31 MB: 15 test archives and one of ALZip 11.25 (README) | 7.19 → 5.54 MB at level mid, 5.05 at level 9 | | 5.5 (`aqr`, level mid, all threads); 1.8 at level 9 | 23 (`-K`) | 0.35 GB peak RSS at mid, 0.50 at 9 | measured 2026-11-01 with `aqr`; no module of its own (the headers kept, the bodies rebuilt between them). ALZip's DEFLATE is zlib's (records 0.01% of the streams at profile 10, 1-10% at 6); its bzip2 is libbzip2's in its own framing (`bz2-alz-recons`). Mostly one PE file (6.46 MB), recompressed by aqr-exe in turn |
| EGG (ALZip) | aqr-egg, aqr-flate, aqr-bz2, aqr-lzma, aqr-azo | 327 test archives, 49.5 MB: EggDotNet's, alzgg's and unegg's, mostly the same few files by every setting (README) | 33.19 → 16.66 MB at level mid, 16.42 at level 9 (31.87 before) | | 19 (`aqr`, level mid, all threads); 13 at level 9 | 79 (`-K`) | 0.96 GB peak RSS at mid, 1.17 at 9 | measured 2026-11-01 with `aqr`; no module of its own (the headers kept, the blocks rebuilt between them). ALZip's DEFLATE is zlib's (records 0.08% of the blocks at profile 10, 0.79% at 6); its LZMA the LZMA SDK's (records 1.2%); bzip2 libbzip2's; AZO by aqr-azo (records 1.0%; −6.2% of the whole). Much of the gain is the same files stored once |
| AZO (ALZip, in EGG) | aqr-azo | the 17 AZO streams of the EGG test archives, 3.12 MB (README) | records 1.03% of the streams (2.1% of the coded blocks) | | 5.5 (`aqr-azo-check`, split, one thread) | 6.4 (join) | about 14 MB for a block of 512 KiB as Wasm | measured 2026-11-01; ALZip's parse recorded against hash chains (as aqr-lzo); the x86 filter not known, filtered streams rebuilt with their data before it |
| LHA (`.lzh`, `.lha`) | aqr-lha | 119 Aminet archives drawn at random, 25.2 MB, LhA for AmigaOS's mostly (README) | 24.95 → 22.85 MB at level mid, 22.04 at level 9 | | 6.0 (`aqr`, level mid, all threads); 4.1 at level 9 | 22 (`-K`) | 0.29 GB peak RSS at mid, 0.61 at 9 | measured 2026-11-01 with `aqr`; the streams of `-lh4-` to `-lh7-` split into data and a record (0.38% of the streams; 0.2–1.9% by compressor on lhasa's test archives), every block's header as LHA's Huffman coder makes it; the module `lha-recons`. Much of Aminet is already compressed |
| RAR (RAR5, RAR 1.5–4.x of RAR 2.9's algorithm) | aqr-rar | the 10gb corpus's 83 RAR archives, 22.25 MB (13.44 MB stored: many are the same), RAR 3.x by older WinRARs, mostly PPMd (README) | 13.44 MB at level mid (none taken: PPMd from level 9 only), 13.25 at level 9 (−1.4%: three taken) | | 4.2 (`aqr`, level mid, all threads: one LZ stream split and turned down); 3.8 at level 9 | | 0.32 GB peak RSS at mid, 1.71 at 9 | measured 2026-11-01 with `aqr`; streams split into data and a record (0.717% of the corpus's streams, 3.12% of rar 7.23's RAR5, 2.23% of rar 6.24's RAR 3.x), every block cut and table as RAR's; the modules `rar5-recons` and `rar3-recons` (PPMd var.H with RAR's range coder). High-order PPMd archives do not pay apart: aqr's coders are no better on their data |
| NSIS installers (stored data) | aqr-compress (`nsis`) | the 10gb corpus's 11 installers, two of TDM-GCC (27 MB each) holding tarballs of the files installed | 2483.0 → 2464.9 MB for all of it | | | | not measured | measured 2026-11-02 with `aqr`; no module: the files stored in the installer's data (not compressed by NSIS) cut out as files of their formats, their tarballs' members shared with the same files archived along. NSIS's own zlib, bzip2 and LZMA are not reproduced |
| PPMd var. J (`.pmd`) | aqr-ppmd | the 10gb corpus's two files, 205.4 MB (README) | 2579.2 → 2557.8 MB for all of it | | | | not measured | measured 2026-10-30 with `aqr`, not `aqr-show`; no module (the file is its header and its data): `enwik8.pmd`'s data is stored within `enwik9.pmd`'s, as their texts start alike. PPMd itself is not recoded |
| Matroska, WebM | aqr-mkv | 11 WebM files from YouTube, 738.1 MB (README) | 0.531 (still pictures 0.32–0.67; moving videos not taken) | 0.532 (`--long=27`) | 408 (`aqr`, all threads) | 2600 (`-K`) | 1.94 GB peak for all at once (1.47 stored) | measured 2026-10-31 with `aqr`; no module: a video's keyframes, byte for byte the same where the picture is still, held once and apart from the rest (the frames not decoded). What is left is mostly Opus audio |
| LevelDB tables (Snappy) | aqr-snappy | 33 tables of a Chrome profile, 13.3 MB (README) | −30.5% against `zstd -3` | | | 315 (tables rebuilt) | not measured | both hashes of Snappy 1.2.2 |
| git checkouts | aqr-git (packs), aqr-flate (loose objects), the archive (the rest) | 15 repositories, 1196 MB (README) | 0.454 (0.430 from level 7: aqr-flate's adaptive profile) | 0.651 (`tar \| zstd -19 --long=27`) | 23 (`aqr`, 10 threads) | 142 (`aqr -K`, 10 threads) | peak RSS 0.2–7.1 GB (pyre, 665 MB, 10.7×) | directories, measured with `aqr` 2026-10-28, not `aqr-show`; the former aqr-git (its own container) came to 0.452, 0.255 with every zlib stream rebuilt and all the packs' data in one stream (here in solid blocks of 16 MiB) |

### Executables

| format | crate | sample | ratio | zstd -3 | comp. MB/s | decomp. MB/s | memory / input | notes |
|---|---|---|---|---|---|---|---|---|
| PE, Mach-O, ELF (x86, x86-64, ARM64, CIL) | aqr-exe | 259 files, 391 MB | 0.295 ~ 0.303 | 0.381 | 28 | 414 | 26 MB + 7.9× | samples from `Program Files` and `System32` of a Windows machine; .NET metadata and IL (CIL) too: 0.306 → 0.267 for the 84 assemblies of IL alone; the rest of an image scanned (PNG resources); 39 files not coded (27 of them .NET, mostly satellite assemblies of resources). 11 large binaries (1529 MB): 0.306 at 58 and 348 MB/s, 0.76–1.01 of the sample's memory fit; two small files kept raw take over twice it (106 MB for 0.7 MB, 136 MB for 5.3 MB). The disfilter alone: 292 and 336 MB/s, −20% under zstd -9. Mach-O, thin or universal, and ARM64 code (in Mach-O and PE): VS Code's 34 universal binaries (493 MB) −16.7% against the files as they are; the ARM64 filter alone −17.6% under zstd -9 (xz's BCJ −12.1%). ELF (code alone): 22 Linux binaries of ARM64 and x86-64 (499 MB) −16.2% against the files as data of no format (README) |
| DWARF (dSYM, ELF) | aqr-dwarf | the dSYM of a release build of `aqr` (rustc, DWARF 4), 214 MB; 7-Zip's `7zr` by clang (dSYM) and GCC (ELF), DWARF 5 (README) | 0.107 (rustc), 0.276 (clang), 0.239 (gcc) | 0.227 (rustc) | | | peak RSS 0.99 GB (rustc) | measured 2026-11-02 with `aqr`: `.debug_info` by its DIEs and Apple's accelerator tables: rustc 41.77 → 22.85 MB (−45.3%; xz -9 31.71 MB), clang −24.8%, gcc −37.9% |
| PDB (MSF) | aqr-pdb | a Rust program's PDB by lld-link (54 MB), Rust's `std`, 12 of Windows' PDBs (README) | 0.121 (`uniform.pdb`), 0.145 (`std`) | | | | peak RSS 473 MB (`uniform.pdb`) | measured 2026-11-02 with `aqr`: the container and the modules' streams (inline sites' scopes derived, annotations by operation, C13 lines): `uniform.pdb` 14.95 → 6.57 MB (−56%; xz -9 9.73) at 56 MB/s, `std` −23.8%, `combase.pdb` −6.6%; Windows' public PDBs kept as they were |
| Android DEX | aqr-dex | the 21 DEX files of 7 APKs from F-Droid, 132.0 MB (README) | the files 46.23 → 30.87 MB through `aqr` (−33.2%); the 7 APKs −6.8% | 0.631 of zstd -9 (`aqr-dex-check`; xz -9e 0.758) | 130 (the module, a thread) | 290 (the module, a thread); 827 tested through `aqr` | not measured | measured 2026-11-02 on an M4. Strings front-coded, id tables by column in deltas, instructions split by field, indices through caches of the recent ones, the checksum and SHA-1 derived. Left: a context model of opcodes and registers (3.6 MB each) |
| Android resource tables (`resources.arsc`) | aqr-arsc | the `resources.arsc` of the same 7 APKs, 44.7 MB (README) | 9.41 → 6.78 MB through `aqr` (−27.9%); the APKs −1.3% more | 0.638 of zstd -9 (xz -9e 0.731) | | 2100 tested through `aqr` | not measured | measured 2026-11-02 on an M4. The string pools' strings without offsets and lengths, the type chunks' entries by field, keys and values predicted by the configurations before. Left: the string indices (0.94 MB, the pool sorted by configuration and value); binary XML, too small a file for a formula each |
| WebAssembly | aqr-wasm | the 56 modules of aqr-modules, 3.60 MB (README) | −20.5% through `aqr -c` at mid (−21.7% at high); −4.3% each file alone | −15.8% at zstd -22, each alone | 169 (one thread) | 185 (one thread) | not measured | the instructions split by field, local indices by their ranks among those used last, sizes derived; data segments and custom sections handed on as content. The module (7.9 KB) costs an archive of one module nearly what it saves |

### Data of no known format

What aqr-compress tries on data in which no file of a known format is found.

| what | crate | sample | ratio | zstd -3 | comp. MB/s | decomp. MB/s | memory / input | notes |
|---|---|---|---|---|---|---|---|---|
| records of fixed size, entries with a length field, floats of whole multiples | aqr-infer-recs | 11 files, 114.6 MB: ELF and Mach-O binaries, a font, a pcap, git's index and pack index, `ld.so.cache` (README) | 0.974 of zstd -3 (one thread) | | 65 (one thread) | | 527 MB peak RSS | tables of fixed-size records, entries with a length field and floats of whole multiples, their fields transposed (`recs-fields`, `recs-entries`, `recs-floats`); tried by aqr-compress on data in which no file is found, where a 2–50 ms estimate tells 5% at least. The SAO star catalogue 0.432 of zstd -3 |

### In progress or planned

| crate | next |
|---|---|
| aqr-flate | zlib block layouts predicted; zlib-ng, chromium, libdeflate, miniz and stb emulated; faster splitting at profile 10 |
| aqr-png | only matches kept when decoding; a pixel-domain mode for streams zlib reproduces; 16-bit and palette models |
| aqr-jpeg | progressive re-encoding and compression in parallel; memory of progressive files (compact coefficients); why fuzzing leaves many inputs uncoded |
| aqr-mp3 | the 3 pp gap to packMP3; other encoders (FhG, iTunes, Xing); MP3 in MP4 or MKV |
| aqr-flac | warm-up samples predicted; FLAC in Ogg, more than 8 channels |
| aqr-raw | 16-bit images (PNM, PAM, then TIFF and DICOM); float audio, CAF, W64, RF64; a layout with the pixel data other recompressors hand over, so that aqr-compress sends it to `raster-recons` |
| aqr-pdf | default JPEG tables for small images; files encrypted with an empty user password; ASCII85 and DCT chains; cross-reference tables rebuilt |
| aqr-psd | 16- and 32-bit data, ZIP channels, other colour modes; effects and adjustment layers rendered |
| aqr-clip | speed of one thread (the model of busy tiles); neighbouring tiles and mipmaps as context; the database's PNG preview through aqr-png; the 3D data |
| aqr-sqlite | memory (chunked processing); WAL and rollback journals; WITHOUT ROWID secondary indexes |
| aqr-xsv | numbers within text fields (coordinates, dates, times); hexadecimal fields; numbers coded better than varints |
| aqr-jsonl | decoding segments on more threads; numbers within strings; coordinates' floats; pretty-printed JSON |
| aqr-obj | compression speed of `%.17g` files; connectivity of faces in random order; `.mtl` |
| aqr-zip | DEFLATE only; other methods (bzip2, LZMA, AES) are kept |
| aqr-bundle | members sliced from the archive's bytes rather than copied (the memory of `.tar.gz`); COFF objects of ar archives (aqr-exe); old GNU sparse files; tar archives found in other data |
| aqr-asar | contents of many small files by kind cost alone and pay in a batch (etcher +1.96 MB alone; of no kind the 27 archives +9.2 MB) |
| aqr-ppmd | the data decoded and coded again (a few MB/s, `docs/10gb-todo.md` item 5); files of several files past the first; other variants |
| aqr-prores | decoding speed (fewer decisions a byte); alpha's run-lengths; Apple's own encoders as a sample (README) |
| aqr-mkv | Opus audio; MP4 (`stsz`/`stco`) by the same idea; why the formula taken unweighed archives larger (README) |
| aqr-mszip | speed of LZX (22 MB/s of data a stream), to take it below level 9; the data blocks' checksums derived; MSI |
| aqr-alz | split archives' volumes joined; encrypted entries; the entries' CRC-32 and sizes for the probe of what is archived along (`entry_sums`); bzip2 of several blocks checked against ALZip |
| aqr-egg | split archives' volumes joined; encrypted files; the files' CRC-32 and sizes for the probe of what is archived along (`entry_sums`) |
| aqr-azo | the x86 filter (an oracle of ALZip wanted); ALZip's choice between a literal and a match, most of the record; speed |
| aqr-lha | `-lh1-`, `-lh2-`, `-lh3-`, `-lhx-`, LArc's and PMarc's methods; archives in other data (self-extracting programs); LHA 2.x's choice among matches of the longest length; speed; the module's size against small archives |
| aqr-rar | volumes (a stream over several archives); encryption; RAR 1.5 and 2.0's algorithms; older WinRARs' code lengths (not RAR 6.24's Huffman: 8.6% of the corpus's record); RAR's parse of RAR5 (literals where matches are, most of a 3% record); speed (PPMd rebuilt to split, and archives that do not pay split all the same); the modules' sizes against small archives |
| aqr-iso | UDF's metadata partition (2.50, Blu-ray); raw 2352-byte sectors (EDC/ECC); the files' contents sliced from the image rather than copied |
| aqr-wim | XPRESS and LZMS (and ESD files); the lookup table's SHA-1 and the chunk tables derived; the resources cut into the images' files by their metadata; spanned WIMs |
| aqr-lzx | speed (a large CAB folder is one stream on one thread); LZX DELTA |
| aqr-lzma | speed of the match finder (most of the time); a stream alone does not pay for its record (14%); LZMA in ZIP (method 14) |
| aqr-7z | PPMd, BZip2 and Deflate folders; Swap2/Swap4; a large folder rebuilt on one thread (10.7 s for the 10gb corpus's) |
| aqr-bcj | Swap2/Swap4; BCJ2 under other filters |
| aqr-dex | a context model of opcodes and registers; `registers_size` and `ins_size` from the prototypes; debug info by its state machine; DEX 041's containers, DEX in OAT and VDEX |
| aqr-arsc | the string indices (the pool's order derived from the order of first reference); binary XML as one application for all of an archive's; compact and sparse entries |
| aqr-fits | a blend of predictors (AIA's tiles at 0.95 of their Rice streams); hcompress's quantisation; `GZIP_*`, `HCOMPRESS_1`, `PLIO_1` tiles and compressed tables; the modules' sizes against small files |
| aqr-hdf5 | a module of zlib's deflate alone (`flate-set` is 78.7 KB); integer images by a 2D predictor; LZF and Blosc chunks emulated; layout 4's indexes |
| aqr-wasm | a dictionary for the streams (−16% more in a trial); the module's cost against archives of one module |

## Worth doing

The criteria: a weak entropy coder (prefix codes, Rice codes, fixed widths, none at all) or a
deterministic encoder that can be reproduced, and a field where the format makes most of the
bytes. Gains are estimates unless a tool is cited.

### Video production and broadcast

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| Avid DNxHD, DNxHR | broadcast, Avid workflows | intra DCT, VLC; constant bit rate, so every frame is padded | the padding, then the VLC recoded | aqr-prores's model and chunks |
| DV, HDV, XDCAM (MPEG-2), MPEG-4 Part 2 (Xvid, DivX), H.263, VC-1 (WMV9, Blu-ray) | archived broadcast and camcorder footage, DVD VOB, old downloads | DCT, VLC tables fixed by the standard | coefficients recoded (as packMP3). Inter frames also need motion vectors and macroblock modes modelled, but nothing reconstructed | aqr-mp3's structure |
| Motion JPEG (AVI, MOV) | dashcams, webcams, older cameras' video, security cameras | a JPEG per frame, often without its Huffman tables (the standard ones implied) | each frame through aqr-jpeg; the frames' headers predicted from each other | aqr-jpeg |
| HuffYUV, FFVHuff, UT Video, MagicYUV | lossless capture, intermediates | spatial prediction, then static Huffman tables per stream or slice | the residuals context-modelled | aqr-raw's pixel model |
| H.264 CAVLC (baseline profile) | older phones, video calls, some security cameras | VLC tables chosen by the neighbours' coefficient counts | parsing needs no reconstruction, so the syntax elements can be recoded under richer contexts | aqr-mp3's structure |
| Theora | old web video, game cutscenes | VP3: DCT tokens with Huffman tables set in the stream header | as Vorbis | the Vorbis work |
| CineForm, ProRes RAW | high-end cameras, post-production | wavelets, or a RAW-specific coding | after ProRes (aqr-prores) | |
| MPEG-TS, M2TS | AVCHD camcorders, DVB and IPTV recordings, Blu-ray | 188-byte packets (192 in M2TS) of PES around the streams; null packets padding a constant rate | the packets' headers (continuity counters, PCR, M2TS's timestamps) derived and the null packets dropped; the streams handed to their recompressors (MPEG-2, MP2, AC-3; H.264 CABAC is Hard) | aqr-mkv's handling |
| AVI, ASF (WMV, WMA), FLV | old downloads, camera and screen recordings, Flash video | chunks or packets interleaved, with an index (`idx1`, ASF's simple index, FLV's keyframes) | the indexes derived from the chunks; the streams handed to their recompressors (Motion JPEG, MP3, VC-1, WMA) | aqr-mkv's handling |

### Audio

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| AAC (M4A, MP4), WMA | music stores, soundtracks of every MP4 | MDCT, then Huffman codebooks (AAC) or run-level VLC (WMA) | as packMP3; needs MP4 or ASF parsing | aqr-mp3's structure |
| MPEG-1 Layer II (MP2) | DAB radio, DVB and DVD soundtracks, broadcast archives | subband samples quantised at a fixed allocation, in grouped fixed-width codes: no entropy coding | the samples and scalefactors modelled: a large gain | aqr-mp3's frame handling |
| AC-3, E-AC-3 (Dolby Digital) | DVD, Blu-ray, broadcast, streaming 5.1 | differential exponents in groups, mantissas at fixed widths from a computed bit allocation: no entropy coding | exponents and mantissas modelled | |
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
| Canon CR2, DNG | photographers' originals (terabytes each) | lossless JPEG (ITU T.81 SOF3): prediction, then Huffman | 2D context modelling: 15–30%. Rawzor claimed 20–40% around 2008 | aqr-jpeg's Huffman decoder; the JPEG previews |
| Nikon NEF | the same | Huffman-coded differences (also uncompressed and lossy variants) | the same | the same |
| Sony ARW2 | the same | 128-bit blocks of min, max and 7-bit deltas: no entropy coding | the largest gain of the group | |
| Panasonic RW2, Olympus ORF, Pentax PEF, Hasselblad 3FR, Phase One IIQ | the same | fixed-width packing or simple Huffman or differential schemes per vendor | as CR2 | |
| Fujifilm RAF, Canon CR3 | the same | RAF uncompressed or its own coding; CR3's CRX is adaptive Golomb | CR3 is already adaptive: less to gain | |

Nearly every RAW file holds one to three JPEG previews, which aqr-jpeg takes as they are. The
container is TIFF/EXIF.

### Images and graphics

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| TIFF, GeoTIFF, COG, OME-TIFF | scanning, print, GIS, microscopy | uncompressed, PackBits, LZW or deflate, with an optional horizontal predictor | the existing recompressors per strip or tile | aqr-psd (PackBits), aqr-flate, aqr-gif (LZW, of MSB-first codes) |
| CCITT Group 3 and 4 | fax, scanned documents in TIFF and PDF (`CCITTFaxDecode`) | modified Huffman and READ codes with fixed tables | the bilevel pixels context-modelled (JBIG's context model, the reference point, beats G4 clearly) | |
| ICO, CUR, TGA, PCX, ILBM; RLE BMP | icons, old games, screenshots, textures | uncompressed or RLE | the samples through `raster-recons`; the RLE runs reproduced (uncompressed BMP and PNM are done) | aqr-raw |
| Radiance HDR | HDR environment maps | RGBE, per-scanline RLE | the RLE reproduced, the RGBE pixels modelled | |
| OpenEXR | VFX, rendering | per block: ZIP (zlib after a predictor and byte split), PIZ (wavelet, then Huffman), RLE, and lossy ones | ZIP blocks through aqr-flate; PIZ's Huffman recoded | aqr-flate |
| GIMP XCF | image editing | tiles in RLE or zlib | the zlib tiles; the RLE reproduced | aqr-flate |
| Krita (`.kra`), Procreate | digital painting | ZIP of layer tiles compressed with LZF (Krita) or LZO (Procreate) | LZF and LZO encoders reproduced (both simple and open), then a pixel model | aqr-zip, aqr-lz4's approach |
| ICNS, MNG, JNG, SVGZ | macOS icons, animations, vector images | PNG, JPEG or JPEG 2000 inside; a PackBits variant (old ICNS); gzip (SVGZ) | the embedded files through their recompressors | aqr-png, aqr-jpeg, aqr-flate |
| ASTC, ETC2, PVRTC textures | mobile games | fixed-rate blocks, no entropy coding | as BCn (below) | |

### 3D and CAD

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| FBX (binary) | 3D asset exchange, game pipelines | property arrays (vertices, indices, normals) optionally zlib-compressed one by one | the zlib arrays, then the arrays modelled as meshes | aqr-flate, aqr-obj's predictors |
| glTF and GLB (without Draco or meshopt), STL, PLY | the web, 3D printing, scans | raw little-endian buffers of floats and indices | a mesh model of the buffers (as aqr-obj's, without the text) | aqr-obj's predictors |
| USDZ, USDC | Apple AR, Pixar pipelines | USDZ is a stored ZIP; USDC compresses its arrays with LZ4 and integer coding | the LZ4 blocks reproduced | aqr-zip, aqr-lz4 |
| 3MF | 3D printing | ZIP of XML | already taken by aqr-zip; the XML has no model yet | aqr-zip |
| Blender (`.blend`) | 3D art | a dump of in-memory structs described by its SDNA; optionally gzip (before 3.0) or zstd | the gzip ones; the structs modelled by their SDNA types | aqr-flate |
| LAS, E57 | lidar, surveying | uncompressed point records (LAZ is LAS's arithmetic-coded form) | the points predicted (LASzip shows how much: LAZ is typically a tenth to a fifth of LAS) | |
| STEP, IGES, IFC | mechanical CAD exchange, BIM (IFC is a STEP file: hundreds of MB a building) | text: entity instances of numbers and references | numbers and references (`#123`, mostly the previous instance's plus a little) modelled by entity type | aqr-obj's predictors, aqr-xsv's fields |
| DXF (ASCII) | AutoCAD exchange, CNC, laser cutting | text of group codes and values, one a line | the group codes predicted from the entity, the coordinates as in aqr-obj | aqr-obj |
| GDSII, OASIS | chip layouts (often GBs, then gzip'd) | GDSII: uncompressed records of 32-bit coordinates; OASIS: varints, optionally DEFLATE blocks (`CBLOCK`) | the polygons' coordinates as deltas; OASIS's raw deflate | aqr-flate |
| Alembic, OpenVDB | VFX caches and volumes | Alembic (Ogawa): uncompressed arrays of samples; OpenVDB: grids of half or full floats, in zlib or Blosc | the arrays against the frame before; the zlib grids | aqr-flate, aqr-hdf5's float model |

### Documents and office

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| HWP 5.0 | Korean government, schools and businesses (Hancom Office) | an OLE compound file whose streams are raw deflate | the deflate streams, the compound file layout | aqr-flate |
| DOC, XLS, PPT (OLE) | legacy Microsoft Office | a compound file of uncompressed streams, embedded pictures (PNG, JPEG, EMF, often zlib-wrapped in PPT) | the compound file's sectors and FAT rebuilt; the pictures through their recompressors | aqr-png, aqr-jpeg, aqr-flate |
| Outlook PST, OST | mail archives | "compressible encryption": a fixed byte substitution, then the messages and attachments as they are | the substitution undone (it is a table), then the contents recompressed | everything |
| mbox, EML, MHTML | mail archives, saved web pages | attachments in base64 or quoted-printable | the transfer encoding undone where its line layout is regular, then the attachments recompressed | everything |
| MOBI, AZW, PalmDOC | e-books | PalmDOC's simple LZ77, or HUFF/CDIC | the PalmDOC encoder reproduced (it is small and deterministic) | |
| SWF | Flash archives (Flashpoint and others) | `CWS` is the whole file in zlib; `ZWS` is LZMA | the zlib ones, then the tags, JPEG and lossless bitmaps (zlib) inside | aqr-flate, aqr-jpeg |
| PostScript, EPS | print, old figures | images in ASCII85, RunLength, LZW, Flate or DCT filters | the filters undone as in PDF | aqr-pdf's stream coding |
| XPS, OXPS | Windows print spool, documents | ZIP of XML, fonts and images | already taken by aqr-zip | aqr-zip |
| Keynote, Pages, Numbers (iWork) | Apple's office documents | ZIP of `.iwa`: protobuf in Snappy chunks without the framing format's CRCs | the Snappy chunks reproduced (if Apple's compressor is Google's; else a record of the parse as aqr-lzo), then the protobuf by field | aqr-zip, aqr-snappy |
| RTF | word processors, Outlook, legal and government documents | text; pictures as hexadecimal digits (`\pict`: PNG, JPEG, WMF) | the hex undone, the pictures recompressed | aqr-png, aqr-jpeg |
| Jupyter notebooks (`.ipynb`) | data science | JSON; plots as base64 PNG in the outputs | the base64 undone, the PNGs recompressed (the base64 row below in general) | aqr-png, aqr-jsonl |
| OneNote (`.one`) | notes | a revision store (MS-ONESTORE) of uncompressed objects; embedded files and pictures as they are | the pictures and files recompressed; the store's references derived | everything |

### Fonts

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| TrueType, OpenType (CFF), TTC | every OS and application; CJK fonts take 5–20 MB each | uncompressed tables: `glyf` outlines (flags and coordinates as deltas of 1 or 2 bytes), CFF charstrings, `hmtx`, `loca` | WOFF2's transforms show the way (`loca` derived, `glyf` split into streams of flags, coordinates and instructions): about 30% under the same coder; a TTC's shared tables once | aqr-infer-recs's fields (it takes their tables now as data of no format) |
| WOFF | web fonts (before WOFF2) | each table in zlib, where that is smaller | the zlib streams, then the tables as above | aqr-flate |

### Data and machine learning

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| safetensors, GGUF, PyTorch `.pt`, ONNX, `.npy`, `.npz` | model weights | raw tensors (`.pt` and `.npz` are stored ZIPs) | bf16 and fp16 exponents entropy-coded: about 30% for bf16 (ZipNN, DFloat11); quantised GGUF gains less | aqr-zip |
| Parquet, ORC, Avro | data lakes, analytics | pages or blocks in Snappy (Parquet's usual default), gzip, LZ4 or zstd | the Snappy and gzip pages, then the column values | aqr-snappy, aqr-flate |
| Arrow IPC, Feather, Zarr (Blosc) | dataframes, arrays | LZ4 frames or zstd buffers; Blosc's shuffle and LZ4 | the LZ4 ones | aqr-lz4 |
| TFRecord | ML datasets | protobuf records, optionally gzip or zlib | the gzip, then the records | aqr-flate |
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
  numbers apply once the literals are parsed; hex-printed binaries (`\x…`, `0x…`, `X'…'`)
  are twice their bytes.
- Logs repeat the data: PostgreSQL's full-page images, binlog's before and after images,
  table maps in every transaction; recycled log files keep old contents in their tails.
- Daily full backups differ little from one another: where several generations are archived
  together, long-range matches (pages by number, dumps in a stable row order) gain the most.

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| mysqldump, MariaDB's `mariadb-dump` (`.sql`, `.sql.gz`) | MySQL backups, migrations | text: `--opt`'s extended `INSERT … VALUES (…),(…);`, lines up to `net_buffer_length` (about 1 MB) of thousands of rows in column order; strings with `\0 \n \r \\ \' \" \Z`; binaries escaped as they are, or `0x…` with `--hex-blob`; `--tab` writes TSV with `\N` | the rows parsed into columns (lines of a megabyte), the hex decoded; the DDL and `/*!40101 … */` boilerplate by the archive | aqr-xsv, aqr-num, aqr-flate |
| mydumper, MySQL Shell's `util.dumpInstance` | large MySQL dumps | chunks of a table per file; mydumper's in gzip or zstd, Shell's TSV in zstd by default with JSON metadata | the gzip ones, then as above; zstd as under Hard | aqr-flate, aqr-xsv |
| MySQL binlog (`binlog.000123`) | replication, point-in-time recovery; up to `max_binlog_size` (1 GB) each | events of a 19-byte header (time, type, server id, size, `log_pos` = the next event's offset) and a CRC-32 (since 5.6); ROW format (since 5.7.7): `TABLE_MAP` then `WRITE/UPDATE/DELETE_ROWS`, each row a null bitmap and values by column type; `binlog_row_image=FULL` keeps both images of an update; optionally zstd (`TRANSACTION_PAYLOAD`, 8.0.20) or encrypted | the header fields and CRCs derived; rows by column from the table map; an update's after image against its before image | aqr-infer-recs's fields |
| MySQL InnoDB (`.ibd`, `ibdata1`, redo), Percona XtraBackup | database servers and their physical backups | 16 KiB pages: a FIL header of 38 bytes (checksum, page number, neighbours, LSN, type, space id) and an 8-byte trailer, CRC-32C by default; records with a 5-byte header and relative next-record offsets; `ROW_FORMAT=COMPRESSED` and page compression in zlib (or LZ4, with holes punched); redo in 512-byte blocks, reused in a circle; XtraBackup's xbstream, `--compress` with qpress (QuickLZ), LZ4 or zstd, incrementals as `.delta` pages by LSN | the checksums, page numbers and space ids derived; the records by column; the zlib pages; free space as it is | aqr-flate, aqr-lz4, aqr-sqlite's approach |
| pg_dump plain (`-Fp`), `pg_dumpall` | PostgreSQL backups, migrations | text: `COPY t (…) FROM stdin;` of TSV rows with `\N` and backslash escapes, ended by `\.` (or `INSERT`s with `--inserts`); `bytea` in hex (`\\x…`, the default since 9.0), timestamps with zones, arrays and JSON as text | the COPY rows as aqr-xsv takes TSV; the hex decoded | aqr-xsv, aqr-num |
| pg_dump custom and directory (`-Fc`, `-Fd`) | PostgreSQL backups (`pg_restore`) | `PGDMP`, a table of contents, each table's COPY text as length-prefixed chunks of one zlib stream (lz4 or zstd from 16); `-Fd` writes `NNNN.dat.gz` per table; `-Ft` is a plain tar | the zlib streams, then as plain | aqr-flate, aqr-xsv |
| PostgreSQL data files, `pg_basebackup`, WAL | database servers, physical backups and their archived WAL | 8 KiB pages: a 24-byte header (LSN, a 16-bit checksum, on by default from 18), line pointers, tuples from the end with 23-byte headers (`xmin` alike in a page, `ctid` mostly pointing at itself); 1 GiB segment files; TOAST values in pglz or LZ4; WAL in 16 MiB segments with CRC-32C records and full-page images after checkpoints (`wal_compression`: pglz, LZ4, zstd); a segment switched early by `archive_timeout` stays 16 MiB; base backups as tar (gzip, LZ4 or zstd on the server from 15) with a JSON manifest; pgBackRest and WAL-G compress with gzip, LZ4, zstd, bzip2 or Brotli | the pages' derived fields and tuples by column; pglz (simple and open); full-page images against the base backup's pages | aqr-flate, aqr-lz4, aqr-sqlite's approach |
| Oracle Data Pump (`expdp`, `.dmp`), `exp` | Oracle backups and migrations | closed: metadata as XML-like DDL, rows in the direct-path stream, the job's master table included; uncompressed unless `COMPRESSION=ALL` (Advanced Compression's licence), so plain dumps are common; split by `%U` and `FILESIZE` | the row pieces by column, once the format is known (reverse engineering, `expdp` and `impdp` the black boxes); the old `exp` format is simpler | aqr-infer-recs |
| Oracle datafiles, RMAN image copies, archived redo (`.arc`) | Oracle servers, physical backups | blocks of 8 KiB (2–32 KiB): a header with the block address and SCN, a 16-bit XOR checksum (`DB_BLOCK_CHECKSUM`), a 4-byte tail check from the SCN; redo in 512-byte (or 4 KiB) blocks with headers of sequence, block number and checksum | the derived fields and the rows by column, as InnoDB's; RMAN's backup sets as under Hard | aqr-sqlite's approach |
| Microsoft SQL Server `.bak`, `.trn` (uncompressed), `.mdf`, `.ldf` | SQL Server backups and detached databases | Microsoft Tape Format: descriptor blocks (`TAPE`, `SSET`, `VOLB`, `MSCI`, `MSDA`) and padding around 8 KiB pages almost as they are (96-byte headers, checksums by default, slot arrays; ROW and PAGE compression within them); differentials hold changed extents; the log in 512-byte sectors each stamped with parity bits (64 and 128 by turns as a VLF is reused); stripes over several files | MTF's blocks parsed, the pages' derived fields and rows by column, as InnoDB's; compressed backups (the default in many installations) as under Hard | aqr-sqlite's approach |
| SSMS "Generate Scripts", `.bacpac` | SQL Server exports | scripts in UTF-16LE by default ("Unicode"): one `INSERT [dbo].[t] ([a], [b]) VALUES (…, CAST(N'…' AS DateTime))` per row, the column list repeated, `0x…` binaries; `.bacpac` is a ZIP of `model.xml` and BCP native data | UTF-16 to UTF-8 reversibly, the statements as a template and columns; the bacpac's entries | aqr-zip, aqr-xsv |
| SQLite `.dump`, WAL, rollback journals | SQLite backups | `.dump`: `INSERT INTO t VALUES(…)` per row, blobs as `X'…'`; WAL frames of a page and a 24-byte header with checksums | the dump as SQL text above; the WAL's frames as aqr-sqlite's pages (In progress) | aqr-sqlite |
| MongoDB `mongodump` (`.bson`, `--archive`, `--gzip`), WiredTiger | MongoDB backups, servers | BSON documents with every field name repeated and int32 lengths, ObjectIds rising (time, machine, counter); `mongoexport` writes Extended JSON lines; WiredTiger's blocks in Snappy by default (zstd or zlib optional) | the lengths derived, fields transposed by path; the gzip; Snappy blocks | aqr-jsonl, aqr-flate, aqr-snappy |
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
| BAM, bgzip (VCF.gz, FASTQ.gz) | genomics, petabytes of it | BGZF: gzip members of 64 KiB one after the other (htslib's zlib or libdeflate) | taking the DEFLATE off alone gains much; fields split into streams (as CRAM) can come later | aqr-flate and flate-zoo have nearly all of it |
| DICOM | medical imaging (PACS) | uncompressed 16-bit, JPEG Lossless, JPEG-LS, RLE | lossless JPEG shared with RAW; uncompressed images through `raster-recons` once it takes 16 bits | RAW's lossless JPEG, aqr-raw |
| NIfTI (`.nii.gz`) | neuroimaging | a gzip of the whole volume | the gzip, then a 3D model of the voxels | aqr-flate |
| mzML, mzXML | mass spectrometry | XML with base64 arrays of floats, optionally zlib | base64 and zlib undone, the m/z and intensity arrays modelled | aqr-flate |
| ROOT | particle physics (CERN) | baskets in zlib, LZ4, LZMA or zstd | the zlib and LZ4 baskets | aqr-flate, aqr-lz4 |
| GRIB2, simple packing | weather | integers packed at a fixed width | unpacked, then a predictive model | |
| SEG-Y | seismic surveys | uncompressed traces of IBM or IEEE floats | a trace model | |
| OSM PBF | OpenStreetMap extracts | zlib-compressed protobuf blobs | the zlib blobs | aqr-flate |
| MBTiles, PMTiles | map tiles | SQLite or a flat archive of PNG, JPEG or gzip'd vector tiles | the tiles through their recompressors | aqr-sqlite, aqr-png, aqr-jpeg, aqr-flate |
| FASTQ, SAM (uncompressed) | sequencers' output, alignments | text: reads' names, nucleotides, quality strings (FASTQ); tab-separated fields (SAM) | names by field against the read before, quality strings by a context model (fqzcomp's), nucleotides as aqr-fasta | aqr-fasta, aqr-xsv |
| MRC, cryo-EM movies | structural biology (TBs per session) | raw 8- or 16-bit counts or floats a frame (TIFF of LZW too) | a model of sparse counts against the frames before | aqr-fits's pixel model |
| SRTM `.hgt`, Esri shapefiles (`.shp`, `.dbf`) | elevation data, GIS | `.hgt`: 16-bit big-endian heights uncompressed; `.shp`: records of doubles; `.dbf`: fixed-width text records | the heights through `raster-recons` once it takes 16 bits; the coordinates as deltas, the `.dbf` by column | aqr-raw, aqr-infer-recs, aqr-xsv |

### Web, logs and capture

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| WARC (`.warc.gz`) | web archives (Common Crawl, Internet Archive) | a gzip member per record | the gzip, then the HTTP bodies (gzip again, images) | aqr-flate, everything |
| HAR, data URIs, base64 in JSON and XML; uuencode, yEnc | web tooling, inline assets; Usenet binaries | base64 (uuencode and yEnc alike: text of 6 or 8 bits a byte, in lines) | base64 undone, the bytes recompressed | everything |
| Browser HTTP caches (Chrome, Firefox, Safari) | browser profiles | bodies as sent: gzip, Brotli, zstd; images | the gzip bodies and images (Brotli and zstd are hard, below) | aqr-flate, aqr-png, aqr-jpeg |
| Rotated logs (`*.log.N.gz`) | every server | gzip, usually zlib's level 6 or 9 | reproduced by zlib emulation | aqr-flate |
| systemd journal | Linux logs | field data over a size threshold in xz, LZ4 or zstd | the LZ4 objects; the journal's hash tables and offsets derived | aqr-lz4 |
| pcap, pcapng | network captures | uncompressed packets | packets modelled by flow and protocol; payloads recompressed | |
| Windows EVTX | event logs | chunks of binary XML with templates | the templates and records modelled | |
| Chromium `.pak` | every Electron app and Chromium browser (`resources.pak`, `chrome_100_percent.pak`) | an index and resources, gzip or Brotli where compressed (HTML, JS, CSS), PNGs | the gzip resources, the PNGs; laid out as aqr-asar does its files | aqr-flate, aqr-png, aqr-asar |

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
| LZFSE, LZVN containers | APFS and HFS+ transparent compression (decmpfs), DMG (ULFO), `.aar`, `.xip` | chunks of LZFSE or LZVN behind a table of their own | aqr-bvx takes whole streams (asset catalogs, `.lzfse`); the chunks are raw LZVN or streams behind those tables | aqr-bvx |
| DMG (UDZO, UDBZ) | macOS distribution | zlib or bzip2 per chunk | the zlib chunks | aqr-flate |
| xar (`.pkg`) | macOS installers | a zlib table of contents; a payload in gzip'd cpio or pbzx (xz chunks) | the zlib and gzip parts | aqr-flate |
| MSI | Windows installers | CAB inside an OLE compound file | the CAB's sectors gathered (aqr-mszip takes CAB files of MSZIP) | aqr-mszip |
| AppX, MSIX, NuGet | Windows apps, .NET packages | ZIP; AppX adds a block map of SHA-256 hashes | aqr-zip; the block map derived | aqr-zip |
| squashfs, initramfs, Android sparse images | firmware, Linux distributions | gzip, xz, zstd or lz4 blocks | the gzip and lz4 blocks | aqr-flate, aqr-lz4 |
| OCI and Docker layers | container registries | tar.gz by Go's `compress/flate` | flate-zoo has Go's encoder already; the tar is aqr-bundle's | aqr-flate, aqr-bundle |
| UEFI firmware volumes | PC firmware | EFI/Tiano compression (LZ77 with Huffman) or LZMA | Tiano's reference encoder (EDK II) reproduced | |
| Intel HEX, Motorola S-record | microcontroller firmware | hex text with per-line checksums | the bytes, with addresses and checksums derived | |
| VMDK, VHD, VHDX, qcow2, VDI | virtual machines, their backups and OVAs | sparse images behind block tables; qcow2's compressed clusters and streamOptimized VMDK's grains (OVA) in deflate | the deflate clusters; the block tables derived; the guest's file system as below | aqr-disk (2026-11-09: dynamic VHD, VHDX, sparse VMDK, qcow2 and VDI read, their disks' files taken; a 2 GB disk's VHDX 510.9 → 461.9 MB, aqr-disk README); aqr-flate for the compressed ones |
| File system images (ext4, NTFS, FAT, APFS, HFS+), E01 | disk dumps, firmware, forensics (E01: chunks of 32 KiB in zlib) | none (NTFS's LZNT1 and WOF's XPRESS for compressed files) | as aqr-iso: the image as its files' contents and the rest, the free blocks found zero or kept; E01's zlib chunks | aqr-disk; FAT and exFAT read by aqr-fat, NTFS by aqr-ntfs, ext2/3/4 by aqr-ext, APFS by aqr-apfs (2026-11-09: a FAT32 volume of /usr/share 26.21 → 23.34 MB, an NTFS one of Ubuntu's 132.5 → 123.8 MB, an ext4 one of 1.57 GB 478.3 → 425.7 MB, an APFS one of 1.57 GB 346.3 → 325.3 MB; their READMEs); aqr-flate |

### Executables and bytecode

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| ELF (x86-64, ARM64) | Linux and Android binaries | uncompressed (debug sections optionally zlib or zstd) | the code is filtered (aqr-exe); left: the tables (relocations, symbols, `.eh_frame`), DWARF (aqr-dwarf: `.debug_info` and Apple's tables so far; not compressed sections) | aqr-exe, aqr-flate, aqr-dwarf |
| Java class files | JARs | uncompressed constant pools and bytecode | as DEX | |
| Python bytecode (`.pyc`) | every Python installation (`__pycache__`, wheels of compiled code) | uncompressed marshal of code objects | the constants and names against the source beside it; the bytecode by field | aqr-wasm's approach |
| SPIR-V, DXBC/DXIL shaders | games' shader caches and packages | uncompressed 32-bit words (SPIR-V); LLVM bitcode in a container (DXIL) | SPIR-V's ids and opcodes by field | aqr-wasm's approach |

### Games

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| BC6H and BC7 textures; KTX, KTX2, engine bundles | most of a game's install size | fixed-rate block compression, no entropy coding (BC7 and BC6H: modes and partitions) | as aqr-tex does BC1–BC5 in DDS | aqr-tex |
| Unity AssetBundle | Unity games | LZ4 (chunked) or LZMA | the LZ4 chunks reproduced | aqr-lz4 |
| Unreal `.pak` with zlib | Unreal Engine 4 games (before Oodle) | zlib per compression block | the zlib blocks | aqr-flate |
| Bethesda BSA, BA2 | Skyrim, Fallout | zlib or LZ4 per file | the zlib and LZ4 | aqr-flate, aqr-lz4 |
| Ren'Py (`.rpa`, `.rpyc`) | visual novels | an archive with a zlib-compressed index; compiled scripts in zlib | the zlib; the images inside through their recompressors | aqr-flate, aqr-png, aqr-jpeg |
| RPG Maker MV and MZ assets | indie games | PNG and OGG with their first 16 bytes XORed by a key in `System.json` | the header restored, then aqr-png or the Vorbis work | aqr-png |
| Minecraft regions (`.mca`), NBT (`.dat`), Bedrock worlds | server backups | zlib per chunk with sector padding; gzip; LevelDB with raw deflate (Bedrock) | Java's default zlib reproduced, the padding dropped | aqr-flate, aqr-snappy's LevelDB tables |
| CD images (BIN/CUE, IMG) | PlayStation, Saturn and other disc images | none; each 2352-byte sector carries EDC and ECC computed from its data | sync, EDC and ECC regenerated (as ECM does): about 12% of a data sector | |
| CSO, ZSO | PSP and PS2 ISOs | deflate (CSO) or LZ4 (ZSO) per block | the blocks | aqr-flate, aqr-lz4 |
| Wwise (`.wem`, `.bnk`) | most AAA games' audio | Vorbis without its setup header (codebooks referenced from a known set), ADPCM, Opus in newer versions; banks of them | as FSB5: the setup rebuilt, the Vorbis recoded; the ADPCM through aqr-ima | aqr-fsb, aqr-vorbis, aqr-ima |
| CRI ADX, HCA, USM, CPK | Japanese games | ADX: 4-bit ADPCM at fixed width; HCA: MDCT with static tables; USM: MPEG-1, VP9 or H.264 in its own container; CPK: an archive of CRILAYLA (an LZ of its own) | ADX's samples modelled (a large gain); CRILAYLA reproduced | aqr-ima's approach |
| Valve VPK, VTF | Source engine games | VPK: files stored; VTF: DXT1/DXT5 textures with mipmaps | the textures as aqr-tex does DDS | aqr-tex |
| XNA, MonoGame `.xnb` | indie games | XMemCompress's LZX (XNA) or LZ4 (MonoGame) | the LZX as aqr-lzx's record of the parse, the LZ4 reproduced | aqr-lzx, aqr-lz4 |
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
| LZMS, XPRESS Huffman (ESD, Windows prefetch, `hiberfil.sys`; LZX of WIM, CAB and CHM is aqr-lzx's, a record of the parse) | Windows install images | Microsoft's encoders are closed and their parsers optimal-style; predicting and correcting (as aqr-flate does) leaves large corrections |
| Oodle (Kraken, Mermaid, Leviathan) | AAA game paks | a closed commercial library with many versions and levels |
| xz, LZMA, 7z alone (also `.deb`, `.rpm`, Arch packages, pbzx) | source and package distribution | optimal parsing over many versions and options, and little to gain over an already strong coder: aqr-lzma's records of the parse (10–18% of the streams) pay only where the data is stored elsewhere too (Done above) |
| zstd, Brotli (also WOFF2, HTTP bodies) | HTTP, packages, file systems | output changes between versions; levels, windows and dictionaries make a wide space; the entropy stage is already good |
| CHD, RVZ, WIA, NSZ | disc and cartridge images for emulators | per-hunk choices among zlib, LZMA, zstd, FLAC and Huffman; the LZMA and zstd ones as above |
| SQL Server's compressed backups, Oracle RMAN backup sets and compressed Data Pump, DB2's `COMPRESS` | database backups | SQL Server's default `MS_XPRESS` is Microsoft's closed encoder; 2022's `QAT_DEFLATE` is DEFLATE by QATzip and ISA-L, whose encoder would have to be matched; RMAN keeps only used blocks in a closed container, its BASIC, LOW, MEDIUM and HIGH said to be bzip2, LZO, zlib and bzip2 (Oracle's documents after 11.1 name only the levels); DB2's is closed |
| Android OTA payloads | system updates | bsdiff and puffin operations over xz, bzip2, zstd or Brotli blobs |

### Other reasons

| format | used in | why it is hard |
|---|---|---|
| Opus | streaming, voice (Discord, WhatsApp, YouTube's WebM audio) | reproducible (a range coder over mostly static tables, and parsing needs no synthesis), but the PVQ pulses are coded uniformly and fine energy as raw bits, so little is left (Balrogg: 3–8%), for a whole CELT and SILK parser with its bit allocation |
| DRM-protected media, encrypted archives and backups | commercial content; iOS, WhatsApp and Telegram backups ; databases under TDE and their encrypted backups | random data without the key; only files that carry their key (PDF with an empty user password) can be done |
| Console packages (Switch NCA, PlayStation PKG), Unreal `.pak` with encryption | games | platform encryption and signatures on top |
| AutoCAD DWG, Native Instruments Kontakt (NCW), Bink video, ECW, MrSID | CAD, sample libraries, game cutscenes, aerial imagery | closed formats that change with versions; need reverse engineering first |
| Blockchains (Bitcoin's `blk*.dat`) | full nodes (hundreds of GB) | mostly hashes, public keys and signatures, which are random; the scripts' templates and amounts are a few percent |
