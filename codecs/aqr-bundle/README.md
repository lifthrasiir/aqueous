# aqr-bundle

Archives that only bundle files, rebuilt bit for bit from the contents of their members, and the
asset packs of game engines taken apart into their files. Nothing is gained on an archive alone:
what it is for is that its contents become contents of the application, to be recompressed by
their formats (a PNG file in a tar archive by aqr-png, the objects of an ar library by aqr-exe),
and stored once where the same files are archived along with it. `FORMAT.md` specifies the module
`bundle-recons` and the game packs, and how they work.

## What is taken

- **ar** (System V and GNU, BSD, Windows libraries), **tar** (v7, ustar, GNU, pax) and **cpio**
  (newc, crc, odc, binary of either byte order): the module `bundle-recons`. The headers are kept,
  their sizes, name sizes and checksums derived from the contents (but for the checksum of cpio's
  crc form); padding of zeros is not stored. A tar archive in gzip (`.tar.gz`, `.tgz`, `.crate`,
  Homebrew bottles) is taken in the gzip's data; a tar archive in a ZIP archive is taken apart
  with aqr-zip's `zip-recons`. Not taken: thin ar archives, and an archive of no member.
- **Packs of game engines** (`packs`: aqr-disk's formulas, no module), formats named
  `<engine>-<extension>` (or the middleware's or tool's):
  - `godot-pck` (versions 1 to 4; not an encrypted directory), `unity-bundle` (UnityFS formats 6 to
    8; the nodes in blocks stored as they are, those of LZ4 or LZMA stay in the rest),
    `unreal-pak` (versions 1 to 11; stored entries and those of zlib or gzip are files, Oodle
    entries compressed contents; not encrypted or deleted ones, not IoStore), `source-vpk`
    (versions 1 and 2, and its archives by the tree), `unreal-upk` (Unreal Engine 3's packages and
    texture caches).
  - `unity-assets` (serialized files, versions 9 to 22, each object a file; textures of BC1, BC3,
    BC4, BC5 and of 8-bit uncompressed pixels rebuilt, also those in a `.resS` file; not BC7,
    BC6H, crunched, ASTC, ETC), `unity-resource` (FSB5 banks and videos back to back).
  - Audio: `fmod-bank`, `wwise-bnk` and `wwise-pck`, `xact-xwb` (ADPCM waves rebuilt by
    aqr-msadpcm), `cri-awb`, `cri-acb`, `cri-cpk`, `cri-afs`.
  - `gamemaker-win` (the textures of `TXTR` and the sounds of `AUDO`), `xna-xnb` (stored content:
    textures, sprite fonts, PCM; not the compressed ones), `chromium-pak` (read, but not looked
    for by aqr-compress).
  - Unverified on real files (each from its format's public descriptions): `cri-cpk`, `cri-afs`,
    `wwise-pck`, `quake-pak`, `doom-wad`, `quake-wad2`, `hl-wad3`, `gmod-gma`, `bethesda-bsa`,
    `bethesda-ba2`, `ea-big`, `renpy-rpa`, `kirikiri-xp3`, `mtf-arc`, `sony-psarc`.

A pack whose one file is all of it has no formula. Wwise's Vorbis media are recompressed by
aqr-wwise; HCA's are of compressed data.

## Results

On an Apple M4 (10 cores) through `aqr` (all threads, level mid, 2026-10-26), against the build
before aqr-bundle:

| | before | after | time | peak footprint |
|---|---|---|---|---|
| the 10gb corpus (79,431 files, 10.0 GB) | 2810.8 MB | 2775.0 MB (-1.3%) | 109.8 -> 106.0 s | 8.59 -> 7.49 GB |
| `gimp.tar` and its 4143 files, calgary (163 MB) | 26.78 MB | 14.25 MB | 2.1 -> 1.7 s | |
| mingw of the 10gb corpus (1.5 GB, 4660 ar archives) | 344.60 MB | 342.05 MB | 13.3 -> 15.7 s | |
| 68 Homebrew bottles (`.tar.gz`, 281 MB) | 219.99 MB | 219.70 MB | 16.3 -> 17.3 s | 5.56 -> 7.72 GB |
| 1500 crates (`.crate`, 58 MB) | 56.41 MB | 56.40 MB | 0.93 -> 1.35 s | 1.02 -> 1.08 GB |

- What comes of it is sharing: of the 10gb corpus, `gimp.tar` and its files -11.3 MB (the tar
  archive 11.87 -> 6.93 MB), the rest of the -35.8 MB from what came with it in aqr-compress
  (JPEG files of the same picture, baseline and progressive, -15.2 MB). The 4660 ar archives
  come to +1.0 MB: their members are 1 KB objects on average.
- Every ar and tar archive of the 10gb corpus is rebuilt (4656 ar, 494,950 members; 8 tar, 6810
  members); the module encodes at 6.6 GB/s and decodes at 28 GB/s. The corpus has no cpio archive.
- A bottle gains little: its tar archive is mostly Mach-O dylibs, whose code aqr-exe takes, but
  that is a few MB of the data.
- `bundle-recons` holds nothing: about 1.3 MiB whatever the archive's length. gimp.tar of the 10gb
  corpus at `-9`: 10,935,835 B, decompressing natively 2.04 G instructions at 109 MB resident.

## Tests

- `cargo rt -p aqr-bundle`: `tests/packs.rs` writes the game packs itself; the rest rebuilds the
  fixtures of `scripts/gen-fixtures.sh` (`tests/data`, made with bsdtar, bsdcpio, Python's
  tarfile, the ar of macOS, llvm-ar and mingw).
- `aqr-bundle-check [-v] FILE_OR_DIR...`: the archives found, whether each is rebuilt, zstd -3 of
  the archives against the module's inputs, speeds.
- `aqr-bundle-packs FILE...`: the game packs read, their files, the bytes taken, whether the
  formula gives each back.

## Left

- **Compressed packs.** Unity's LZ4 blocks (LZ4HC, which aqr-lz4 does not reproduce) and LZMA, XNB's
  LZX and LZ4, and Oodle are not recompressed; Unreal's zlib blocks are found by the scan, not
  rebuilt as streams of their own.
- **IoStore** (`.utoc`/`.ucas`), encrypted directories and files, and packs embedded in an
  executable are not read.
- **Unverified formats.** `cri-cpk`, `cri-afs`, `wwise-pck` and the formats listed above have no
  real sample on this machine.
- **BC7 and others in Unity.** BC7, BC6H, crunched, ASTC and ETC textures of `unity-assets` are not
  rebuilt.
- **HCA.** HCA's MDCT is not recompressed.
- **Bottles.** A bottle of 88 MB (icu4c) archives larger than the tar archive raw; its formula
  should be given up where it does not pay.
- **Archives.** Members are copied, not sliced from the archive's bytes (the memory of a `.tar.gz`); COFF
  objects of ar archives (aqr-exe) and old GNU sparse files are not taken; tar archives in other data are
  not found.
