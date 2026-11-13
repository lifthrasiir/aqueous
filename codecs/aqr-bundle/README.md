# aqr-bundle

Archives that only bundle files, rebuilt bit for bit from the contents of their members
(`FORMAT.md`): ar (System V and GNU, BSD, Windows libraries), tar (v7, ustar, GNU, pax) and cpio
(newc, crc, odc, binary of either byte order). The headers are kept, their sizes, name sizes and
checksums derived from the contents (but for the checksum of cpio's crc form); padding of zeros
is not stored.

Nothing is gained on an archive alone: what it is for is that its contents become contents of
the application, to be recompressed by their formats (a PNG file in a tar archive by aqr-png,
the objects of an ar library by aqr-exe), and stored once where the same files are archived
along with it. A tar archive in gzip (`.tar.gz`, `.tgz`, `.crate`, Homebrew bottles) is taken in
the gzip's data.

## How it does

Measured 2026-10-26 on an Apple M4 (10 cores), through `aqr` (all threads, level mid) against
the commit before aqr-bundle (`e6ec924`), and with `aqr-bundle-check`.

| | before | after | time | peak footprint |
|---|---|---|---|---|
| the 10gb corpus (79,431 files, 10.0 GB) | 2810.8 MB | 2775.0 MB (−1.3%) | 109.8 → 106.0 s | 8.59 → 7.49 GB |
| `gimp.tar` and its 4143 files, calgary (163 MB) | 26.78 MB | 14.25 MB | 2.1 → 1.7 s | |
| mingw of the 10gb corpus (1.5 GB, 4660 ar archives) | 344.60 MB | 342.05 MB | 13.3 → 15.7 s | |
| 68 Homebrew bottles (`.tar.gz`, 281 MB) | 219.99 MB | 219.70 MB | 16.3 → 17.3 s | 5.56 → 7.72 GB |
| 1500 crates (`.crate`, 58 MB) | 56.41 MB | 56.40 MB | 0.93 → 1.35 s | 1.02 → 1.08 GB |

- What comes of it is sharing: of the 10gb corpus, `gimp.tar` and its files −11.3 MB (the tar
  archive 11.87 → 6.93 MB), and the rest of the −35.8 MB from what came with it in aqr-compress
  (slices of 1 KiB shared, files of 4 KiB held; JPEG files of the same picture, baseline and
  progressive, −15.2 MB). The 4660 ar archives come to +1.0 MB: their members are 1 KB
  objects on average, which compress about as well with their headers, and coding each
  archive's formula against it raw takes 18% more time on mingw.
- Every ar and tar archive of the 10gb corpus is rebuilt (4656 ar, 494,950 members; 8 tar, 6810
  members; 4 ar archives of no member are not taken), the module at 6.6 GB/s to encode and
  28 GB/s to decode (`aqr-bundle-check`). The corpus has no cpio archive.
- A bottle gains little: its tar archive is mostly Mach-O dylibs, whose code aqr-exe takes, but
  that is a few MB of the data; the formula of a bottle of 88 MB (icu4c) archives larger than
  the tar archive raw. The bottles take more memory when encoding, as the members' contents are
  copied into their streams by kind (on top of the tar archive inflated).
- With aqr-zip's `zip-recons`, the contents of large entries are slices of their own: a tar
  archive in a ZIP archive (silesia's `mozilla`) is taken apart, and found again where the same
  file is archived along with it.

## Memory

`bundle-recons` is a part module: it takes the data and the layout and puts the archive in
pieces through windows, holding nothing, so its application needs about 1.3 MiB (plus the
windows) whatever the archive's length (version 1, a whole module, needed three times the
archive: 299 MiB for gimp.tar's 79 MB, over a 256 MiB bound). gimp.tar of the 10gb corpus, M4,
`-9`: 10,936,375 B before at no bound and 10,935,835 B after (the generic path that the bound made it
fall back to is 9,990,651 B on this file alone, 9% smaller); decompress native 1.91 -> 2.04 G
instructions, resident 180 -> 109 MB, Wasm 1.45 -> 1.49 G, 536 -> 226 MB. The price is the
checksum of cpio's crc form (version 1 predicted it from the body, which version 2 has not at
hand when it puts the header): its 4 bytes of each member are stored.

## Packs of game engines

`packs`: the asset packs of game engines, as aqr-disk's formulas (no module). A pack's directory
is read into the files it holds and their extents in it; each file's contents become a content of
the application, of its kind by its name, and the rest (headers, directories, padding, what no
file points at, what is encrypted) is the stream `{format}/rest`, the pack their concatenation.
What a directory holds that follows from the contents (offsets, sizes, the copies of Unreal's
entries before their data) is about 0.01% to 0.1% of a pack: a module deriving it would cost an
archive more than it saves. The formats are named `<engine>-<extension>` (or the middleware's or
tool's: the formats of aqr-compress and the prefixes of the classes), as one extension is several
engines' (Godot's and Wwise's `.pck`, Unreal's and Quake's `.pak`):

- `godot-pck`: Godot's `.pck` (`GDPC`), versions 1 (Godot 3) to 4 (4.4 on: the directory at the
  end). Not read: an encrypted directory; encrypted files are the rest. A pack embedded in an
  executable is not looked for.
- `unity-bundle`: Unity's AssetBundles (`UnityFS`, formats 6 to 8), their table of blocks stored
  or of LZ4: the nodes (serialized files, `.resS`, `.resource`) that lie in blocks stored as they
  are. Those in blocks of LZ4 or LZMA are the rest.
- `unreal-pak`: Unreal Engine 4 and 5's `.pak`, versions 1 to 11 (the entries encoded in bits
  of 4.26 on, named by the full directory index). Stored entries and those of zlib or gzip (as
  their compressed blocks, which aqr-compress's scan finds) are files; those of other methods
  (Oodle) are files of compressed contents (`Compressed::Opaque`); encrypted and deleted ones,
  and an encrypted index, are not read. IoStore (`.utoc`/`.ucas`) is not read.
- `source-vpk`: Valve's VPK, versions 1 and 2. A directory file's own files (preload bytes and
  data); its archives (`NAME_000.vpk`, …, of no header) are taken apart by its tree where they
  are archived along with it (aqr-compress's `vpk`: planned before the batches, by their names).
- `unity-assets`: Unity's serialized files (`.assets`, `sharedassetsN.assets`,
  `globalgamemanagers`, `levelN`, the `CAB-*` of bundles), versions 9 to 22 of either byte order,
  their type trees skipped: each object a file `<path id>.<class>` (`tex2d`, `mesh`,
  `audioclip`, ...). Of no magic: its header checked against the data, tried after every other
  format. Not read: versions below 9. A `Texture2D` of BC1, BC3, BC4 or BC5 blocks
  (`unity_assets::texture`: its first fields by the engine's version, its data from the object's
  end, the layout checked against the image's size) has its blocks rebuilt by aqr-tex's
  `bcn-recons`: those in the file (`unity_assets::encode_with`), and those it streams from its
  resource file (`NAME.resS`, archived along: `unity-ress`, planned by aqr-compress's `siblings`,
  `unity_assets::encode_resource`). Textures of uncompressed pixels of 8-bit samples (Alpha8, R8,
  RG16, RGB24, RGBA32, ARGB32, BGRA32) have their surfaces rebuilt by aqr-raw's `raster-recons`
  alike. Not taken: BC7, BC6H, crunched, ASTC, ETC.
- `unity-resource`: Unity's `.resource` files: FSB5 banks (and the videos, WebM and MP4, of
  VideoClips) back to back, where more than one bank (a bank alone is aqr-fsb's).
- `unreal-upk`: Unreal Engine 3's packages (`.upk`, `.u`, `.umap`) and texture caches (`.tfc`):
  the exports (`Outer/Name.Class`) and thumbnails of a package stored as it is (the exports' table
  as of versions 865 to 893, else none); the compressed chunks of others, files of zlib, of LZO1X
  (a file of chunks alone, which aqr-compress rebuilds by aqr-lzo: `unreal_upk::lzo_blocks`) or of
  compressed contents (LZX: `Opaque`).
- `fmod-bank`: FMOD Studio's banks (RIFF `FEV `): the FSB5 banks of their `SND ` chunks.
- `wwise-bnk`: Wwise's SoundBanks: the media of `DIDX`/`DATA` (`<id>.wem`). `wwise-pck`: Wwise's
  file packages (`AKPK`): banks and streamed files (unverified on real files).
- `xact-xwb`: XACT's wave banks (`WBND`, versions 42 on): the waves' play regions, raw (`.pcm`,
  `.adpcm`, `.xwma`, `.xma`). Where any is of ADPCM, the bank is a formula of its own
  (`xact_xwb::encode_with`): those waves' blocks rebuilt by aqr-msadpcm (`msadpcm-recons`; their
  layout is the entry's format, which a content alone would not have), the others contents.
- `cri-awb`: CRI's AFS2 archives (HCA, ADX); `cri-acb`: the AFS2 archive a cue sheet (`@UTF`)
  holds; `cri-cpk`: CPK archives' `TOC` (or `ITOC`), CRILAYLA files compressed; `cri-afs`: AFS
  archives (the last two unverified on real files). `cri_utf` reads their `@UTF` tables.
- `gamemaker-win`: GameMaker's `data.win` and `audiogroupN.dat`: the textures of `TXTR` (PNG,
  QOI, bzip2 of QOI) and the sounds of `AUDO`.
- `chromium-pak`: Chromium's resource packs (versions 4 and 5: Electron's and NW.js's games),
  brotli resources compressed. Read, but not looked for by aqr-compress: of no magic, and their
  small members (gzip, text, PNG) archived apart code worse than the pack scanned (60 packs:
  10.78 -> 11.17 MB).
- Unverified on real files (no sample on this machine), each from its format's public
  descriptions: `quake-pak` (`PACK`), `doom-wad` (`IWAD`/`PWAD`), `quake-wad2`, `hl-wad3`,
  `gmod-gma`, `bethesda-bsa` (Morrowind, 103 to 105), `bethesda-ba2` (`GNRL`, and `DX10`
  textures of stored chunks), `ea-big`, `renpy-rpa` (its pickled index read), `kirikiri-xp3`,
  `mtf-arc`, `sony-psarc`.

The members' names give their kinds: of Wwise's media, its Vorbis `.wem` and the others
`.wav`; CRI's HCA (`.hca`) is of compressed data by its name (aqr-compress), as files of their own
a scan's fallbacks took it for samples and coded it worse (SE_Kinetic.acb 93.96 -> 97.00 MB).
Both are of codecs a recompressor could take (Wwise's Vorbis as aqr-fsb does FSB5's, HCA's
MDCT), which is left.

A pack whose one file is all of it has no formula: its contents would be it again (a chunk of an
Unreal 3 texture cache, alone, starts as a cache does).

Compressed data is left for separate work: Unity's LZ4 blocks (LZ4HC, which aqr-lz4 does not
reproduce) and LZMA, the zlib blocks of Unreal's entries rebuilt as streams of their own (rather
than found by the scan), Oodle not at all.

## Tests

`cargo rt -p aqr-bundle`; `aqr-bundle-check [-v] FILE_OR_DIR...` (the archives
found, whether each is rebuilt, zstd -3 of the archives against the module's inputs, speeds);
`aqr-bundle-packs FILE...` (the game packs read: their files, the bytes taken, whether the
formula gives each back). The packs' tests write their packs themselves (`tests/packs.rs`).
`scripts/gen-fixtures.sh` makes `tests/data` with the tools of this machine (bsdtar, bsdcpio,
Python's tarfile, the ar of macOS, llvm-ar and mingw).
