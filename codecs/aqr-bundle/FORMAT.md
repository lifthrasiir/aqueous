# The `bundle-recons` module

This document specifies the module `bundle-recons` of aqr-formula entry formulas. The module
rebuilds an archive that only bundles files, an ar, tar or cpio archive, bit for bit from the
contents of its members and a *layout* holding the rest of the archive. The layout keeps every
byte of the headers, but the fields that follow from the contents (sizes, name sizes, checksums)
are stored as their XOR with that prediction, and padding that is zeros (or ar's `\n`) is not
stored, so that whatever a writer put there is kept exactly and what follows from the contents
costs zero bytes. The document is complete: an implementation of the module can be written from
it alone. Sections marked *(encoder)* describe the choices this implementation makes when it
builds formulas; the module does not depend on them.

Contents:

1. [Conventions](#1-conventions)
2. [The module](#2-the-module)
3. [The layout](#3-the-layout)
4. [Predictions](#4-predictions)
5. [Encoding](#5-encoding-encoder) *(encoder)*
6. [Validation](#6-validation)
7. [Packs of game engines](#7-packs-of-game-engines-encoder) *(encoder)*
8. [Design notes](#8-design-notes)

## 1. Conventions

- **varint**: unsigned LEB128, least significant group first, of at most 64 bits.
- **XOR coding**: a field stored as `x` against a prediction `p` is `actual = x XOR p`, byte by
  byte over its whole width.
- `pad(q, a)`: the bytes from `q` to the next multiple of `a`, `(a − q mod a) mod a`.
- Positions are byte offsets in the output, from 0.
- `dec(v, w)`: `v` in decimal, without leading zeros (its last `w` digits where it has more),
  left aligned in `w` bytes and
  padded with spaces. `oct(v, w)`, `hex(v, w)`, `HEX(v, w)`: `v mod 8^w` (or `16^w`) in `w`
  digits, padded with zeros on the left; `hex` of the digits `0-9a-f`, `HEX` of `0-9A-F`.

## 2. The module

- **Module**: name `bundle-recons`, version 2.
- **Params**: none (empty).
- **Resources**: none.
- **Inputs**: exactly two:
  1. `data` (class `raw`): the bodies of the members that are not in the layout, in order;
  2. `layout` (class `bundle/layout`): §3.
- **Output**: the archive of §3. Its length must be the `len` the application declares.

It is a part module (`module::decode_part`) of windows of 16 pages for each input and the
output, which it takes and puts in order, in pieces of 64 KiB: it holds nothing, so the
application takes the same memory (about 1.3 MiB of its own) whatever the lengths. (Version 1
was a whole module, which held the inputs twice and the output: three times the archive.)

`data` is what the members hold, as it is; it is a content of the application and may be
recompressed in turn.

## 3. The layout

The layout is records, each a tag byte and its fields, until the layout ends. The archive is
the output of the records in order. A *body* of `n` bytes is the next `n` bytes of `data`, or,
where the record's flags have `INLINE` (1), the next `n` bytes of the layout at the place given.

| tag | record | fields in the layout |
|---|---|---|
| 0 | gap | `varint n`, then `n` bytes: output as they are |
| 1 | zeros | `varint n`: `n` zero bytes are output |
| 2 | ar member | §3.1 |
| 3 | tar member | §3.2 |
| 4 | cpio member | §3.3 |

### 3.1 ar members

```text
u8      flags         INLINE (1); NOPAD (2)
varint  p             bytes of name after the header (BSD `#1/p`)
varint  n             bytes of the body
60 bytes              the header, its size field XOR coded (§4)
p bytes               the name
[n bytes]             the body, if INLINE
[1 byte]              where p + n is odd and NOPAD is clear: the pad byte XOR 0x0A
```

The output is the header, the name, the body, then, where `p + n` is odd and NOPAD clear, the
pad byte.

### 3.2 tar members

```text
u8      flags         INLINE (1); PAD (2)
varint  n             bytes of the body
512 bytes             the header, its size and checksum fields XOR coded (§4)
[n bytes]             the body, if INLINE
[pad(n, 512) bytes]   the padding, if PAD
```

The output is the header, the body, and `pad(n, 512)` bytes of padding: those given where PAD is
set, else zeros. The size field is decoded first, then the checksum field (its prediction is of
the rest of the header).

### 3.3 cpio members

```text
u8      form          0: newc, 1: crc, 2: odc, 3: binary little endian, 4: binary big endian
u8      flags         INLINE (1); PAD (2); UPPER (4)
varint  m             bytes of the name, its NUL included
varint  n             bytes of the body
h bytes               the header, of h bytes by the form, its fields XOR coded (§4)
m bytes               the name
[a bytes]             the name's padding, if PAD
[n bytes]             the body, if INLINE
[b bytes]             the body's padding, if PAD
```

| form | magic | h | alignment |
|---|---|---|---|
| newc | `070701` | 110 | 4 |
| crc | `070702` | 110 | 4 |
| odc | `070707` | 76 | 1 |
| binary | `C7 71` (little endian) or `71 C7` (big endian) | 26 | 2 |

The output is the header, the name, its padding (to the alignment, by position: `a = pad(q, A)`
where `q` is the position after the name), the body, and its padding (`b = pad(q', A)` after the
body): those given where PAD is set, else zeros. The header's checksum (crc) is kept as it is:
the body is taken after the header is put, so it cannot be predicted (version 1 predicted it).

## 4. Predictions

| record | field (offset, width) | prediction |
|---|---|---|
| ar | size (48, 10) | `dec(p + n, 10)` |
| tar | size (124, 12) | where `n < 2^33`: `oct(n, 11)` then a NUL; else `0x80`, three zero bytes, then `n` as 8 bytes big endian |
| tar | checksum (148, 8) | `oct(s, 6)`, a NUL and a space, where `s` is the sum of the header's bytes as unsigned, those of the field taken as 8 spaces |
| cpio newc, crc | filesize (54, 8) | `hex(n, 8)` |
| cpio newc, crc | namesize (94, 8) | `hex(m, 8)` |
| cpio newc | check (102, 8) | `hex(0, 8)` |
| cpio crc | check (102, 8) | none (as it is) |
| cpio odc | namesize (59, 6) | `oct(m, 6)` |
| cpio odc | filesize (65, 11) | `oct(n, 11)` |
| cpio binary | namesize (20, 2) | `m mod 2^16`, as a word |
| cpio binary | filesize (22, 4) | `(n >> 16) mod 2^16`, then `n mod 2^16`, as words |

Words are 16 bits in the form's byte order. Where UPPER is set, `HEX` takes the place of `hex`.
Other fields are stored as they are.

## 5. Encoding *(encoder)*

`aqr_bundle::formula::encode_with` takes an archive by its start: `!<arch>\n` (ar, thin archives
not taken), a cpio header (a magic, then fields of the form's digits, then a name ending with a
NUL that fits), or a tar header (its checksum, as the unsigned sum or the signed sum of some old
writers; with ustar or GNU magic, or a name and an octal mode of v7). It reads members from the
start while their headers parse and their bodies and padding fit, and keeps the rest as a gap:

- **ar**: the global header is a gap. A body of odd size has its pad byte unless the next header
  is right after it (or nothing follows). The member `//` (GNU's long names) is INLINE; symbol
  tables (`/`, `/SYM64/`, `__.SYMDEF…`) are data of kind table; BSD names (`#1/p`) are taken
  apart from the body.
- **tar**: a member's body is its size field's, else the `size` of a pax header (`x`) before it.
  Links, devices, directories and FIFOs (types `1` to `6`) are given no body where the next
  block is a header, zeros or the end, else the size. Bodies of pax headers (`x`, `g`) and GNU
  long names (`L`, `K`) are INLINE. Runs of zero blocks are zeros records, and so is the rest of
  the archive where it is all zeros.
- **cpio**: runs of zero bytes between members and archives are zeros records, and archives one
  after another are read on.

A member's name (from the long names of ar, a pax `path` or a GNU long name, ustar's prefix
joined) gives the kind of its contents (`Options::kind`); the contents of each kind are a stream
of their own, slices of it in order, those of at least 1 KiB slices of their own. The layout
is checked by building the archive from it, which must give the input; else there is no
formula. An archive without a member has none either.

aqr-compress takes an archive at the start of data, and ar and cpio archives (of ASCII headers)
anywhere in it, and cuts it after its last member and the zeros after it.

## 6. Validation

The module fails if:

- the params are not empty, or the inputs are not two;
- the layout is malformed: a truncated varint or field, an unknown tag, form or flag, or a range
  past the end of `data` or of the layout;
- `data` is not used up exactly;
- the output would be longer than `len`, or is not `len` bytes long.

## 7. Packs of game engines *(encoder)*

`packs`: the asset packs of game engines, as aqr-disk's formulas (no module). A pack's directory
is read into the files it holds and their extents in it; each file's contents become a content of
the application, of its kind by its name, and the rest (headers, directories, padding, what no
file points at, what is encrypted) is the stream `{format}/rest`, the pack their concatenation.
What a directory holds that follows from the contents (offsets, sizes, the copies of Unreal's
entries before their data) is about 0.01% to 0.1% of a pack. The formats are named
`<engine>-<extension>` (or the middleware's or tool's: the formats of aqr-compress and the
prefixes of the classes), as one extension is several engines':

- `godot-pck`: Godot's `.pck` (`GDPC`), versions 1 (Godot 3) to 4 (4.4 on: the directory at the
  end). Not read: an encrypted directory; encrypted files are the rest. A pack embedded in an
  executable is not looked for.
- `unity-bundle`: Unity's AssetBundles (`UnityFS`, formats 6 to 8), their table of blocks stored
  or of LZ4: the nodes (serialized files, `.resS`, `.resource`) that lie in blocks stored as they
  are. Those in blocks of LZ4 or LZMA are the rest. Where a node of a resource file
  (`CAB-....resS`) has textures its serialized node streams from it, the bundle is a formula of
  its own (`unity::encode_with`): their pixels rebuilt as a `.resS` file's are.
- `unreal-pak`: Unreal Engine 4 and 5's `.pak`, versions 1 to 11 (the entries encoded in bits
  of 4.26 on, named by the full directory index). Stored entries and those of zlib or gzip (as
  their compressed blocks, which aqr-compress's scan finds) are files; those of other methods
  (Oodle) are files of compressed contents (`Compressed::Opaque`); encrypted and deleted ones,
  and an encrypted index, are not read. IoStore (`.utoc`/`.ucas`) is not read.
- `source-vpk`: Valve's VPK, versions 1 and 2. A directory file's own files (preload bytes and
  data); its archives (`NAME_000.vpk`, ..., of no header) are taken apart by its tree where they
  are archived along with it (aqr-compress's `vpk`: planned before the batches, by their names).
- `unity-assets`: Unity's serialized files (`.assets`, `sharedassetsN.assets`,
  `globalgamemanagers`, `levelN`, the `CAB-*` of bundles), versions 9 to 22 of either byte order,
  their type trees skipped: each object a file `<path id>.<class>` (`tex2d`, `mesh`,
  `audioclip`, ...). Of no magic: its header checked against the data, tried after every other
  format. A `Texture2D` of BC1, BC3, BC4 or BC5 blocks (`unity_assets::texture`: its first fields
  by the engine's version, its data from the object's end, the layout checked against the image's
  size) has its blocks rebuilt by aqr-tex's `bcn-recons`: those in the file
  (`unity_assets::encode_with`), and those it streams from its resource file (`NAME.resS`,
  archived along: `unity-ress`, planned by aqr-compress's `siblings`,
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
  file packages (`AKPK`): banks and streamed files.
- `xact-xwb`: XACT's wave banks (`WBND`, versions 42 on): the waves' play regions, raw (`.pcm`,
  `.adpcm`, `.xwma`, `.xma`). Where any is of ADPCM, the bank is a formula of its own
  (`xact_xwb::encode_with`): those waves' blocks rebuilt by aqr-msadpcm (`msadpcm-recons`; their
  layout is the entry's format, which a content alone would not have), the others contents.
- `cri-awb`: CRI's AFS2 archives (HCA, ADX); `cri-acb`: the AFS2 archive a cue sheet (`@UTF`)
  holds; `cri-cpk`: CPK archives' `TOC` (or `ITOC`), CRILAYLA files compressed; `cri-afs`: AFS
  archives. `cri_utf` reads their `@UTF` tables.
- `gamemaker-win`: GameMaker's `data.win` and `audiogroupN.dat`: the textures of `TXTR` (PNG,
  QOI, bzip2 of QOI) and the sounds of `AUDO`.
- `xna-xnb`: XNA's and MonoGame's compiled content (`.xnb`), stored: a `Texture2D`'s mipmaps
  (Color by aqr-raw, DXT1/3/5 by aqr-tex), a sprite font's texture, a `SoundEffect`'s PCM samples
  (aqr-raw), in a formula of its own (`xna_xnb::encode_with`). Compressed ones (LZX, LZ4) are not
  read.
- `chromium-pak`: Chromium's resource packs (versions 4 and 5: Electron's and NW.js's games),
  brotli resources compressed. Read, but not looked for by aqr-compress.
- Each from its format's public descriptions: `quake-pak` (`PACK`), `doom-wad` (`IWAD`/`PWAD`),
  `quake-wad2`, `hl-wad3`, `gmod-gma`, `bethesda-bsa` (Morrowind, 103 to 105), `bethesda-ba2`
  (`GNRL`, and `DX10` textures of stored chunks), `ea-big`, `renpy-rpa` (its pickled index read),
  `kirikiri-xp3`, `mtf-arc`, `sony-psarc`.

The members' names give their kinds: of Wwise's media, its Vorbis `.wem` and the others `.wav`;
CRI's HCA (`.hca`) is of compressed data by its name (aqr-compress). Wwise's Vorbis media are
recompressed by aqr-wwise (their setups rebuilt, as aqr-fsb does FSB5's). A pack whose one file is
all of it has no formula: its contents would be it again (a chunk of an Unreal 3 texture cache,
alone, starts as a cache does).

## 8. Design notes

- Version 1 was a whole module, which held the inputs twice and the output (three times the
  archive: 299 MiB for gimp.tar's 79 MB, over the 256 MiB bound); version 2 is a part module of
  about 1.3 MiB. gimp.tar at `-9`: 10,936,375 B at no bound before, 10,935,835 B after (the
  generic path that the bound made it fall back to: 9,990,651 B on this file alone, 9% smaller);
  decompressing natively 1.91 -> 2.04 G instructions, resident 180 -> 109 MB; Wasm 1.45 -> 1.49 G,
  536 -> 226 MB. The price is the checksum of cpio's crc form (version 1 predicted it from the
  body, which version 2 has not at hand when it puts the header): its 4 bytes of each member are
  stored.
- Coding each ar archive's formula against the archive raw takes 18% more time on mingw for no
  gain, so it is not compared.
- A module deriving the packs' directories (offsets, sizes) would cost an archive more than it
  saves (0.01% to 0.1% of a pack).
- `chromium-pak` is not looked for: of no magic, and the small members (gzip, text, PNG) archived
  apart code worse than the pack scanned (60 packs: 10.78 -> 11.17 MB).
- CRI's HCA as files of their own (a scan's fallbacks took them for samples) coded worse:
  SE_Kinetic.acb 93.96 -> 97.00 MB; they are of compressed data by their names.
- The bundles' contents are copied into their streams by kind (on top of the tar archive
  inflated), so bottles take more memory when encoding (5.56 -> 7.72 GB for 68).

