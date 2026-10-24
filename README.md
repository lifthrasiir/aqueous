# Aqueous: recompressors

Where each lossless recompressor stands, what it measures, and the formats not taken yet.
Formats are specified in each crate's `FORMAT.md`, archives and formulas in `spec/index.bs`,
and the design is argued in `xform-design.md`.

## How it was measured

- 2026-10-23, Apple M4, one thread (`-t 1`), at commit `2abe944`.
- Through `aqr-show` (`aqr/src/bin/show.rs`): each file is archived (recompression, archive
  coding and the round-trip check) at level `mid`, then extracted.
  - aqr-obj is not wired into aqr-compress yet, and was measured with `aqr-obj bench`.
  - aqr-git, aqr-lz4 and aqr-snappy: see their rows.
- aqr-jpeg, aqr-png, aqr-psd and aqr-mp3 were re-measured on 2026-10-24 (otherwise as above),
  and aqr-raw's images measured then.
- aqr-exe alone was measured on another machine, an AMD Ryzen 7 9700X under Windows 11 (at
  `aa7a327`, otherwise as above): its speeds are not comparable to the others'. Its memory is
  the peak working set.
- Samples: files of 256 KiB or more from each crate's `corpus/` (aqr-jpeg: `samples/` and the
  JPEG files of codec-corpus), taken in the order of their paths' hashes up to about 400 MB.
- Columns:
  - **Ratio**: Σ archive / Σ input, as *best ~ worst*. Modules' resources (built-in tables)
    are shared by the files of an archive. Worst: one file per archive, as here, its resources
    in it (and files kept raw where they do not pay for them). Best: in the limit of many
    files per archive, Σ (archive − resources) / Σ input, the formulas taken where they pay
    without them (`aqr-show -r`). One figure where the two agree to three decimals (no
    resources, or a few KB). **Tentative**: the actual decoder modules (Wasm) are not embedded
    yet, and will add their blocks.
  - **zstd -3**: the same files under `zstd -3`, for reference.
  - **Speed**: Σ input / Σ time, MB/s. Compression covers recompression, archive coding and
    the check; decompression covers archive decoding and rebuilding.
  - **Memory**: peak RSS over input size, for one process that compresses, checks and
    decompresses. It includes the input, the archive and the output, so its floor is about
    2×. Given as the median over files of 8 MB or more, or else as a linear fit
    (fixed MB + k × input).

## Done

### Images

| format | crate | sample | ratio (tentative) | zstd -3 | comp. MB/s | decomp. MB/s | memory / input | notes |
|---|---|---|---|---|---|---|---|---|
| PNG, APNG | aqr-png | 379 files, 391 MB | 0.802 | 0.997 | 42 | 90 | 7 MB + 10.3× | the data, the raw image and the tokens are held a window at a time; the encoder's symbols (2 bytes per byte of data on photos) are what still follows the pixels |
| JPEG | aqr-jpeg | 96 files, 186 MB | 0.785 | 0.913 | 39 | 82 | 28 MB + 8.2× | sequential files are coded a stripe at a time: a 1.3 MB 7680×4320 file takes 54 MB. Progressive ones still hold their coefficients whole: 259 MB for a 1.4 MB one of that size |
| BMP, PGM, PPM, PAM | aqr-raw | 17 files, 52 MB | 0.223 | 0.400 | 73 | 303 | about 35 MB + 4× | 8-bit samples; not a sample of real files (PNGs of aqr-png's corpus converted, README of aqr-raw). 16-bit and RLE images are not taken |
| PSD, PSB | aqr-psd | 56 files, 439 MB | 0.330 | 0.555 | 45 | 127 | 28 MB + 5.4× | pixels coded in 8-bit documents only; aqr-psd itself takes a few MB besides its streams, the rest is the archive's (a file kept raw is scanned by the parts aqr-psd tells) |

### Audio

| format | crate | sample | ratio (tentative) | zstd -3 | comp. MB/s | decomp. MB/s | memory / input | notes |
|---|---|---|---|---|---|---|---|---|
| MP3 | aqr-mp3 | 402 files, 401 MB | 0.855 ~ 0.893 | 0.971 | 46 | 119 | 61 MB + 6.4× | see below |
| FLAC; WAV, AIFF | aqr-flac; aqr-raw | 28 files, 401 MB | 0.945 | 0.999 | 65 | 165 | 12.5× | the resource is about 2.6 KB per archive. Measured when aqr-flac took WAV and AIFF too: aqr-raw has them now, with the same streams |

The MP3 resource is the built-in tables (131 KB raw, about 38 KB in every archive), so a
typical 1 MB track is coded alone too (worst). The speeds are those of the files coded
(`-r`); the memory is a fit over concatenations of the sample (9 to 120 MB, each coded as one
file with `-e`), as the sample's files are all about 1 MB (32–59 MB each, median 41 MB). The
fixed part is mostly the tables. Before the contexts shared 500 tables, the resource was
1.5 MB raw and 340 KB archived, no file of the sample was coded alone (0.975), and the same
measurements gave 20 / 104 MB/s and 28–52 MB more memory. It was about 140 MB + 14.7× before
the rANS symbols were held a run of 1024 frames at a time and the output assembled as the
reservoir is rebuilt.

### Documents, data, 3D

| format | crate | sample | ratio (tentative) | zstd -3 | comp. MB/s | decomp. MB/s | memory / input | notes |
|---|---|---|---|---|---|---|---|---|
| PDF | aqr-pdf | 144 files, 402 MB | 0.855 | 0.896 | 46 | 198 | 11.4× (max 22.9×) | 18 files kept as they are (no formula paid) |
| SQLite 3 | aqr-sqlite | 84 files, 309 MB | 0.236 | 0.373 | 38 | 395 | 8.9× | |
| Wavefront OBJ | aqr-obj | 30 files, 625 MB | 0.085 | 0.228 | 125 | 334 | 4.7× | `aqr-obj bench`; not in aqr-compress yet |

### Containers and general-purpose compression

| format | crate | sample | ratio (tentative) | zstd -3 | comp. MB/s | decomp. MB/s | memory / input | notes |
|---|---|---|---|---|---|---|---|---|
| gzip, zlib | aqr-flate | 4 files, 4 MB | 0.932 | 0.977 | 7 | 58 | 34 MB + 35.6× | a small sample (all there is here); 2 of 4 not coded. Per-stream figures on 924 streams are in aqr-flate's README |
| ZIP | aqr-zip | 5 files, 327 MB | 0.954 | 0.998 | 8 | 23 | 14× | dominated by enwik9.zip (322 MB, 1 GB of text: 4.5 GB peak); two Python wheels come to 0.76 |
| mozlz4, LZ4 frames | aqr-lz4 | a Firefox profile (README) | −9 to −18% against `zstd -3` of the files, −57% solid | | | 650–1150 (the compressor) | not measured | no sample on this machine; liblz4 1.10.0's fast compressor reproduced exactly |
| LevelDB tables (Snappy) | aqr-snappy | 33 tables of a Chrome profile, 13.3 MB (README) | −30.5% against `zstd -3` | | | 315 (tables rebuilt) | not measured | both hashes of Snappy 1.2.2 |
| git checkouts | aqr-git | 15 repositories, 1196 MB (README) | 0.452 (with zstd -19) | 0.651 (`tar \| zstd -19 --long=27`) | 87–117 | 135–750 | 0.1–2.1× beyond input and output (the largest object is the floor) | directories, measured outside `aqr-show`; `-A` (every zlib stream rebuilt): 0.255 at 25–49 MB/s |

### Executables

| format | crate | sample | ratio (tentative) | zstd -3 | comp. MB/s | decomp. MB/s | memory / input | notes |
|---|---|---|---|---|---|---|---|---|
| PE, Mach-O (x86, x86-64, ARM64) | aqr-exe | 261 files, 401 MB | 0.339 | 0.383 | 17 | 336 | 5.3× (14 MB + 4.7×) | **Ryzen 7 9700X, not the M4** (above). Samples from `Program Files` and `System32`; 121 files not coded (108 of them .NET). 11 large binaries (1529 MB): 0.306 at 36 and 268 MB/s, 4.7×. The disfilter alone: 238 and 296 MB/s, −20% under zstd -9. Mach-O, thin or universal, and ARM64 code (in Mach-O and PE): VS Code's 34 universal binaries (493 MB) −16.7% against the files as they are; the ARM64 filter alone −17.6% under zstd -9 (xz's BCJ −12.1%). No ELF yet |

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
| aqr-sqlite | memory (chunked processing); WAL and rollback journals; WITHOUT ROWID secondary indexes |
| aqr-obj | wiring into aqr-compress; compression speed of `%.17g` files; connectivity of faces in random order; `.mtl` |
| aqr-zip | DEFLATE only; other methods (bzip2, LZMA, AES) are kept |

## Worth doing

The criteria: a weak entropy coder (prefix codes, Rice codes, fixed widths, none at all) or a
deterministic encoder that can be reproduced, and a field where the format makes most of the
bytes. Gains are estimates unless a tool is cited.

### Video production and broadcast

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| Apple ProRes (422, 4444) | editing, mastering, camera recording | intra-only DCT; per slice, coefficients in a Rice/exp-Golomb hybrid whose parameter adapts to the previous value only | context modelling of the coefficients (as Lepton); no quantisation to reproduce, only the coding. 20–30% estimated | aqr-jpeg's coefficient model; a MOV atom parser is new |
| Avid DNxHD, DNxHR | broadcast, Avid workflows | intra DCT, VLC; constant bit rate, so every frame is padded | the padding, then the VLC recoded | with ProRes |
| DV, HDV, XDCAM (MPEG-2), MPEG-4 Part 2 (Xvid, DivX), H.263 | archived broadcast and camcorder footage, DVD VOB, old downloads | DCT, VLC tables fixed by the standard | coefficients recoded (as packMP3). Inter frames also need motion vectors and macroblock modes modelled, but nothing reconstructed | aqr-mp3's structure |
| Motion JPEG (AVI, MOV) | dashcams, webcams, older cameras' video, security cameras | a JPEG per frame, often without its Huffman tables (the standard ones implied) | each frame through aqr-jpeg; the frames' headers predicted from each other | aqr-jpeg |
| HuffYUV, FFVHuff, UT Video, MagicYUV | lossless capture, intermediates | spatial prediction, then static Huffman tables per stream or slice | the residuals context-modelled | aqr-raw's pixel model |
| H.264 CAVLC (baseline profile) | older phones, video calls, some security cameras | VLC tables chosen by the neighbours' coefficient counts | parsing needs no reconstruction, so the syntax elements can be recoded under richer contexts | aqr-mp3's structure |
| Theora | old web video, game cutscenes | VP3: DCT tokens with Huffman tables set in the stream header | as Vorbis | the Vorbis work |
| CineForm, ProRes RAW | high-end cameras, post-production | wavelets, or a RAW-specific coding | after ProRes | |

### Audio

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| Ogg Vorbis | games, streaming, music | floors and residues as vector-quantised Huffman codebooks, set in the stream's setup header | the codebooks are fixed per stream, with no context: recoding the decoded symbols with context mixing gains 8–12% (Balrogg); rehuff and OptiVorbis only re-optimise the codebooks within the format | aqr-mp3's structure (the Ogg pages are new) |
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
| GIF | the web, animations | LZW (when codes widen and when to clear differ by encoder) | the encoder's choices reproduced, the pixels modelled | aqr-raw's pixel model |
| TIFF, GeoTIFF, COG, OME-TIFF | scanning, print, GIS, microscopy | uncompressed, PackBits, LZW or deflate, with an optional horizontal predictor | the existing recompressors per strip or tile | aqr-psd (PackBits), aqr-flate |
| CCITT Group 3 and 4 | fax, scanned documents in TIFF and PDF (`CCITTFaxDecode`) | modified Huffman and READ codes with fixed tables | the bilevel pixels context-modelled (JBIG's context model, the reference point, beats G4 clearly) | |
| ICO, CUR, TGA, PCX, ILBM; RLE BMP | icons, old games, screenshots, textures | uncompressed or RLE | the samples through `raster-recons`; the RLE runs reproduced (uncompressed BMP and PNM are done) | aqr-raw |
| Radiance HDR | HDR environment maps | RGBE, per-scanline RLE | the RLE reproduced, the RGBE pixels modelled | |
| OpenEXR | VFX, rendering | per block: ZIP (zlib after a predictor and byte split), PIZ (wavelet, then Huffman), RLE, and lossy ones | ZIP blocks through aqr-flate; PIZ's Huffman recoded | aqr-flate |
| GIMP XCF | image editing | tiles in RLE or zlib | the zlib tiles; the RLE reproduced | aqr-flate |
| Krita (`.kra`), Procreate | digital painting | ZIP of layer tiles compressed with LZF (Krita) or LZO (Procreate) | LZF and LZO encoders reproduced (both simple and open), then a pixel model | aqr-zip, aqr-lz4's approach |
| Clip Studio Paint (`.clip`) | illustration, comics | an SQLite database plus chunks of zlib-compressed tile data | the zlib chunks, the database | aqr-sqlite, aqr-flate |
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

### Data, databases and machine learning

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| safetensors, GGUF, PyTorch `.pt`, ONNX, `.npy`, `.npz` | model weights | raw tensors (`.pt` and `.npz` are stored ZIPs) | bf16 and fp16 exponents entropy-coded: about 30% for bf16 (ZipNN, DFloat11); quantised GGUF gains less | aqr-zip |
| Parquet, ORC, Avro | data lakes, analytics | pages or blocks in Snappy (Parquet's usual default), gzip, LZ4 or zstd | the Snappy and gzip pages, then the column values | aqr-snappy, aqr-flate |
| Arrow IPC, Feather, Zarr (Blosc) | dataframes, arrays | LZ4 frames or zstd buffers; Blosc's shuffle and LZ4 | the LZ4 ones | aqr-lz4 |
| RocksDB and Cassandra SSTables, Kafka segments | databases, logs | Snappy or LZ4 blocks (zstd elsewhere) | as LevelDB's tables | aqr-snappy, aqr-lz4 |
| Firefox IndexedDB | browser storage | SQLite rows whose values are Snappy-compressed | the values through aqr-snappy inside aqr-sqlite | aqr-sqlite, aqr-snappy |
| Lucene, Elasticsearch | search indices | stored fields in Lucene's own LZ4 | Lucene's LZ4 reproduced | aqr-lz4 |
| Redis RDB | cache snapshots | LZF-compressed strings | LZF reproduced | |
| PostgreSQL (data files, `pg_dump -Fc`) | databases | pglz or LZ4 for TOAST values; zlib per block in custom dumps | pglz is simple and open; the zlib blocks | aqr-flate, aqr-lz4 |
| TFRecord | ML datasets | protobuf records, optionally gzip or zlib | the gzip, then the records | aqr-flate |
| MATLAB `.mat` (v5–v7) | engineering, science | each variable zlib-compressed (v7.3 is HDF5) | the zlib streams | aqr-flate |

### Science, medicine, geography

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| BAM, bgzip (VCF.gz, FASTQ.gz) | genomics, petabytes of it | BGZF: gzip members of 64 KiB one after the other (htslib's zlib or libdeflate) | taking the DEFLATE off alone gains much; fields split into streams (as CRAM) can come later | aqr-flate and flate-zoo have nearly all of it |
| DICOM | medical imaging (PACS) | uncompressed 16-bit, JPEG Lossless, JPEG-LS, RLE | lossless JPEG shared with RAW; uncompressed images through `raster-recons` once it takes 16 bits | RAW's lossless JPEG, aqr-raw |
| NIfTI (`.nii.gz`) | neuroimaging | a gzip of the whole volume | the gzip, then a 3D model of the voxels | aqr-flate |
| mzML, mzXML | mass spectrometry | XML with base64 arrays of floats, optionally zlib | base64 and zlib undone, the m/z and intensity arrays modelled | aqr-flate |
| FITS (`fpack`) | astronomy | Rice coding of 32-pixel blocks | the Rice codes recoded | aqr-flac's Rice handling |
| HDF5, NetCDF-4 | climate, simulation | shuffle filter and deflate per chunk | DEFLATE recompressed per chunk | aqr-flate |
| ROOT | particle physics (CERN) | baskets in zlib, LZ4, LZMA or zstd | the zlib and LZ4 baskets | aqr-flate, aqr-lz4 |
| GRIB2, simple packing | weather | integers packed at a fixed width | unpacked, then a predictive model | |
| SEG-Y | seismic surveys | uncompressed traces of IBM or IEEE floats | a trace model | |
| OSM PBF | OpenStreetMap extracts | zlib-compressed protobuf blobs | the zlib blobs | aqr-flate |
| MBTiles, PMTiles | map tiles | SQLite or a flat archive of PNG, JPEG or gzip'd vector tiles | the tiles through their recompressors | aqr-sqlite, aqr-png, aqr-jpeg, aqr-flate |

### Web, logs and capture

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| WARC (`.warc.gz`) | web archives (Common Crawl, Internet Archive) | a gzip member per record | the gzip, then the HTTP bodies (gzip again, images) | aqr-flate, everything |
| HAR, data URIs, base64 in JSON and XML | web tooling, inline assets | base64 | base64 undone, the bytes recompressed | everything |
| Browser HTTP caches (Chrome, Firefox, Safari) | browser profiles | bodies as sent: gzip, Brotli, zstd; images | the gzip bodies and images (Brotli and zstd are hard, below) | aqr-flate, aqr-png, aqr-jpeg |
| Rotated logs (`*.log.N.gz`) | every server | gzip, usually zlib's level 6 or 9 | reproduced by zlib emulation | aqr-flate |
| systemd journal | Linux logs | field data over a size threshold in xz, LZ4 or zstd | the LZ4 objects; the journal's hash tables and offsets derived | aqr-lz4 |
| pcap, pcapng | network captures | uncompressed packets | packets modelled by flow and protocol; payloads recompressed | |
| Windows EVTX | event logs | chunks of binary XML with templates | the templates and records modelled | |

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
| tar | everything Unix | none, 512-byte blocks and headers | headers predicted and padding dropped; it is what most `.tar.gz` needs besides the gzip | |
| LZFSE, LZVN | APFS and HFS+ transparent compression (decmpfs), DMG (ULFO), `.aar`, `.xip`, asset catalogs (`.car`) | LZ77; LZFSE adds FSE (tANS) | the reference is open source, so the encoder can be reproduced exactly, as Snappy's and LZ4's are | aqr-lz4, aqr-snappy |
| DMG (UDZO, UDBZ) | macOS distribution | zlib or bzip2 per chunk | the zlib chunks | aqr-flate |
| xar (`.pkg`) | macOS installers | a zlib table of contents; a payload in gzip'd cpio or pbzx (xz chunks) | the zlib and gzip parts | aqr-flate |
| CAB (MSZIP), MSI | Windows installers | MSZIP is deflate per block; MSI is CAB inside OLE | MSZIP is easy | aqr-flate |
| AppX, MSIX, NuGet | Windows apps, .NET packages | ZIP; AppX adds a block map of SHA-256 hashes | aqr-zip; the block map derived | aqr-zip |
| squashfs, initramfs, Android sparse images | firmware, Linux distributions | gzip, xz, zstd or lz4 blocks | the gzip and lz4 blocks | aqr-flate, aqr-lz4 |
| OCI and Docker layers | container registries | tar.gz by Go's `compress/flate` | flate-zoo has Go's encoder already; tar is what is missing | aqr-flate |
| UEFI firmware volumes | PC firmware | EFI/Tiano compression (LZ77 with Huffman) or LZMA | Tiano's reference encoder (EDK II) reproduced | |
| Intel HEX, Motorola S-record | microcontroller firmware | hex text with per-line checksums | the bytes, with addresses and checksums derived | |

### Executables and bytecode

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| ELF (x86-64, ARM64) | Linux and Android binaries | uncompressed (debug sections optionally zlib or zstd) | the filters on code sections; the zlib debug sections | aqr-exe, aqr-flate |
| Android DEX | APKs | uncompressed, with shared string, type and method tables | the tables and bytecode modelled (as Pack200 did for class files) | |
| Java class files | JARs | uncompressed constant pools and bytecode | as DEX | |
| WebAssembly | the web, plugins | uncompressed sections | instructions and indices modelled | |

### Games

| format | used in | how it compresses | what is left | reuse |
|---|---|---|---|---|
| BCn textures (DXT1–5, BC7): DDS, KTX, inside engine bundles | most of a game's install size | fixed-rate block compression, no entropy coding | endpoints and indices split into streams and predicted (the lossless side of crunch and Oodle Texture) | |
| Unity AssetBundle | Unity games | LZ4 (chunked) or LZMA | the LZ4 chunks reproduced | aqr-lz4 |
| Unreal `.pak` with zlib | Unreal Engine 4 games (before Oodle) | zlib per compression block | the zlib blocks | aqr-flate |
| Bethesda BSA, BA2 | Skyrim, Fallout | zlib or LZ4 per file | the zlib and LZ4 | aqr-flate, aqr-lz4 |
| Ren'Py (`.rpa`, `.rpyc`) | visual novels | an archive with a zlib-compressed index; compiled scripts in zlib | the zlib; the images inside through their recompressors | aqr-flate, aqr-png, aqr-jpeg |
| RPG Maker MV and MZ assets | indie games | PNG and OGG with their first 16 bytes XORed by a key in `System.json` | the header restored, then aqr-png or the Vorbis work | aqr-png |
| Minecraft regions (`.mca`), NBT (`.dat`), Bedrock worlds | server backups | zlib per chunk with sector padding; gzip; LevelDB with raw deflate (Bedrock) | Java's default zlib reproduced, the padding dropped | aqr-flate, aqr-snappy's LevelDB tables |
| CD images (BIN/CUE, IMG) | PlayStation, Saturn and other disc images | none; each 2352-byte sector carries EDC and ECC computed from its data | sync, EDC and ECC regenerated (as ECM does): about 12% of a data sector | |
| CSO, ZSO | PSP and PS2 ISOs | deflate (CSO) or LZ4 (ZSO) per block | the blocks | aqr-flate, aqr-lz4 |

## Hard

### Strong entropy coders

They already code arithmetically or adapt to context, so remodelling leaves little, and
reproducing their coding is expensive.

| format | used in | why it is hard |
|---|---|---|
| H.264 (CABAC), HEVC, AV1, VVC | nearly all distributed video | context-adaptive arithmetic coding is already close to the entropy; gaining more needs prediction and motion compensation redone, a whole decoder. H.264 CAVLC is the exception (above) |
| VP8, VP9, WebP (lossy) | YouTube, WebRTC, web images | a boolean arithmetic coder with coefficient contexts and probabilities updated per frame; reproducible, but little left |
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
| LZX, LZMS, XPRESS Huffman (WIM, ESD, CHM, CAB-LZX, Windows prefetch) | Windows install images | Microsoft's encoders are closed and their parsers optimal-style; predicting and correcting (as aqr-flate does) leaves large corrections |
| Oodle (Kraken, Mermaid, Leviathan) | AAA game paks | a closed commercial library with many versions and levels |
| RAR | archives | a closed encoder, with licence restrictions |
| xz, LZMA, 7z (also `.deb`, `.rpm`, Arch packages, pbzx) | source and package distribution | optimal parsing over many versions and options, and little to gain over an already strong coder |
| zstd, Brotli (also WOFF2, HTTP bodies) | HTTP, packages, file systems | output changes between versions; levels, windows and dictionaries make a wide space; the entropy stage is already good |
| bzip2 | source distribution | recompressing is deterministic, but gains little: BWT is already a strong model |
| CHD, RVZ, WIA, NSZ | disc and cartridge images for emulators | per-hunk choices among zlib, LZMA, zstd, FLAC and Huffman; the LZMA and zstd ones as above |
| Android OTA payloads | system updates | bsdiff and puffin operations over xz, bzip2, zstd or Brotli blobs |

### Other reasons

| format | used in | why it is hard |
|---|---|---|
| Opus | streaming, voice (Discord, WhatsApp, YouTube's WebM audio) | reproducible (a range coder over mostly static tables, and parsing needs no synthesis), but the PVQ pulses are coded uniformly and fine energy as raw bits, so little is left (Balrogg: 3–8%), for a whole CELT and SILK parser with its bit allocation |
| DRM-protected media, encrypted archives and backups | commercial content; iOS, WhatsApp and Telegram backups | random data without the key; only files that carry their key (PDF with an empty user password) can be done |
| Console packages (Switch NCA, PlayStation PKG), Unreal `.pak` with encryption | games | platform encryption and signatures on top |
| AutoCAD DWG, Native Instruments Kontakt (NCW) | CAD, sample libraries | closed formats that change with versions; need reverse engineering first |
