# RAR 1.5–4.x archives and the v2.9 compression: decoding notes

Clean-room description of the RAR archive format with signature `Rar!\x1A\x07\x00` and of
its "2.9" compression (LZ + Huffman, PPMd blocks, standard filters), written for an
implementer who must not read any RAR implementation. RAR5 (`Rar!\x1A\x07\x01\x00`) is in
[rar5-decoding.md](rar5-decoding.md).

Sources:

- libarchive, commit `348e5a2e27503f5bf114e298e2bd9ddcd59ab48e`:
  `libarchive/archive_read_support_format_rar.c` (archive reader and decoder) and
  `libarchive/archive_ppmd7.c` (its PPMd variant H with the RAR range decoder). Behaviour
  only; no code, identifiers or comments are carried over.
- RARLAB's `technote.txt` of RAR 4.20 (in `rarlinux-4.2.0.tar.gz` from rarlab.com) for
  header fields.
- lzma-sdk 26.00 `C/Ppmd7.c`, `C/Ppmd7Dec.c` (public domain), referred to for the PPMd model.
- Black-box observation of rar 6.24 (`third_party/rar-dl/v624/rar/rar -ma4`) and
  bsdtar 3.7.4.

Every statement about the bit stream below was checked with a throwaway decoder written from
these notes (Python, plus lzma-sdk's PPMd model under the range decoder of §4.3): it decodes
all fixtures of §9 (rar 6.24 output, and libarchive's own test archives that are valid) with
matching CRCs. Statements marked *(libarchive)* describe what libarchive does where nothing
else confirmed it; *(observed)* means rar 6.24 output, not a format rule; *(unverified)*
marks guesses.

## 1. Conventions

- Multi-byte header fields are little-endian.
- CRC is CRC-32 (reflected polynomial 0xEDB88320, initial and final XOR 0xFFFFFFFF, as
  zlib's `crc32`). A 16-bit header CRC is the low 16 bits of the CRC-32.
- The compressed stream is read MSB-first: the first bit of the stream is bit 7 of the first
  byte. "Read n bits" yields an unsigned integer whose first-read bit is the most
  significant.
- "Align" means skipping to the next byte boundary (no-op when already aligned).

## 2. Archive structure

### 2.1 Signature and self-extracting archives

An archive starts with the 7-byte marker `52 61 72 21 1A 07 00`. A self-extracting archive
(file starting with `MZ` or `\x7FELF`) has the marker later; libarchive searches for it at
offsets that are multiples of 16, up to 512 KiB from the start (the technote: search the
marker sequence; the SFX module does not contain it). Everything after the marker is a
sequence of blocks.

### 2.2 Common block header

| offset | size | field |
|---|---|---|
| 0 | 2 | header CRC (low 16 bits of CRC-32, see per type) |
| 2 | 1 | block type |
| 3 | 2 | flags |
| 5 | 2 | header size (bytes, from offset 0, includes this 7-byte prefix) |
| 7 | 4 | added size, present only if flags & 0x8000 |

Flags common to all types: 0x4000 = "old versions skip/remove this block when updating",
0x8000 = an added data area follows the header. For file and subblock headers, the data area
size is the packed-size field instead (§2.4); 0x8000 is always set there.

The header CRC covers bytes from offset 2 to the end of the header (header size − 2 bytes).
*(libarchive)* For the old-style types 0x75–0x79 and 0x7B with 0x8000 set, libarchive also
runs the CRC over the added data area; rar 6.24 never writes such blocks.

| type | name | handling |
|---|---|---|
| 0x72 | marker | the fixed 7 bytes above |
| 0x73 | main (archive) header | §2.3 |
| 0x74 | file header | §2.4 |
| 0x75 | old comment | RAR 2.x; skip |
| 0x76 | old authenticity info | skip |
| 0x77 | old subblock | skip |
| 0x78 | old recovery record | skip |
| 0x79 | old authenticity info | skip |
| 0x7A | subblock ("new subblock") | same layout as a file header; §2.6 |
| 0x7B | end of archive | §2.7 |

Blocks are skipped by header size (+ added size, or + packed size for 0x74/0x7A).
libarchive treats an unknown type as an error.

### 2.3 Main header (0x73)

Header size is 13: the common 7 bytes, 2 reserved bytes, 4 reserved bytes (all zero in rar
6.24 output). If flags & 0x0200 an "encryption version" byte follows. The CRC covers bytes
2..12.

| flag | meaning |
|---|---|
| 0x0001 | volume (part of a multi-volume set) |
| 0x0002 | archive comment present (RAR 2.x style; RAR 3.x uses a CMT subblock instead) |
| 0x0004 | locked |
| 0x0008 | solid |
| 0x0010 | new volume naming (`name.partN.rar`) |
| 0x0020 | authenticity information (not set by RAR 3.x) |
| 0x0040 | recovery record present |
| 0x0080 | block headers encrypted: everything after this header is encrypted (unsupported) |
| 0x0100 | first volume (set by RAR 3.0 and later) |
| 0x0200 | encryption version byte present |

Observed: 0x0000 for a plain archive, 0x0008 solid, 0x0040 with `-rr`, 0x0004 with `-k`,
0x0111 for the first volume of a `-v` set and 0x0011 for the others.

### 2.4 File header (0x74)

| offset | size | field |
|---|---|---|
| 7 | 4 | packed size (low 32 bits) |
| 11 | 4 | unpacked size (low 32 bits) |
| 15 | 1 | host OS: 0 MS-DOS, 1 OS/2, 2 Windows, 3 Unix, 4 Mac OS, 5 BeOS |
| 16 | 4 | file CRC (§2.8) |
| 20 | 4 | modification time, MS-DOS format (bits 0–4 seconds/2, 5–10 minutes, 11–15 hours, 16–20 day, 21–24 month, 25–31 years since 1980) |
| 24 | 1 | version needed to unpack, 10·major + minor (§3 uses 29) |
| 25 | 1 | method: 0x30 stored, 0x31–0x35 compressed (fastest … best) |
| 26 | 2 | name size |
| 28 | 4 | attributes (DOS/Windows attribute bits for hosts 0–2, Unix mode for 3–5) |
| 32 | 4+4 | high 32 bits of packed size, then of unpacked size, if flags & 0x0100 |
| … | name size | name (§2.5) |
| … | 8 | salt, if flags & 0x0400 |
| … | var | extended time, if flags & 0x1000 (§2.5) |

The header CRC covers bytes 2..(header size − 1). The packed data (packed size bytes)
follows the header immediately.

| flag | meaning |
|---|---|
| 0x0001 | continued from the previous volume |
| 0x0002 | continued in the next volume |
| 0x0004 | encrypted (unsupported) |
| 0x0008 | file comment (RAR 2.x; not set by RAR 3.x) |
| 0x0010 | solid: the decoder state of the previous file is kept (§3.2, §3.9) |
| 0x00E0 | dictionary: bits 7..5 = n gives a window of 64 KiB << n (n = 0..6, 64 KiB–4 MiB); n = 7 marks a directory |
| 0x0100 | 64-bit sizes (high halves present) |
| 0x0200 | Unicode name (§2.5) |
| 0x0400 | salt present |
| 0x0800 | old file version: name ends in `;n` |
| 0x1000 | extended time present |
| 0x8000 | always set (data area follows) |

The first file of a solid archive has 0x0010 clear, the others set. libarchive rejects
any file header with 0x0010 ("solid archive support unavailable"), so it reads only
non-solid archives.

Methods 0x31–0x35 all use the same decoder; the byte only records the level. Version 29
means the §3 algorithm. Versions 15, 20 and 26 denote the older RAR 1.5 and 2.x algorithms,
not described here (libarchive does not check the version and would misdecode them). rar
6.24 writes 29 for every compressed file and for files it stored because compression did
not help, and 20 for `-m0`, directories and symlinks.

Directories have dictionary bits 111, packed and unpacked size 0. A Unix symlink (host 3,
mode 0o120000) is stored (method 0x30): its data is the target path and the file CRC is the
CRC of that target.

### 2.5 Names and extended time

#### Names

The name uses `\` as path separator. Without flag 0x0200 it is in an unspecified 8-bit
charset (rar 6.24 on macOS writes UTF-8). With 0x0200:

- If the name field contains no NUL byte, the whole field is UTF-8.
- Otherwise the field is `plain` NUL `encoded`, and the Unicode name is rebuilt from
  `encoded` as UTF-16 code units, using `plain` as a reference:
  1. Read a byte `H` (high byte for op 1 and 3). Output index `k` = 0.
  2. While input remains and `k` < name size: when no flag bits remain, read a flag byte and
     take its four 2-bit ops from the most significant pair down. For each op:
     - 0: read byte `b`; emit unit `b`.
     - 1: read byte `b`; emit unit `H·256 + b`.
     - 2: read bytes `lo`, `hi`; emit unit `hi·256 + lo`.
     - 3: read byte `n`. If `n` & 0x80, read byte `c` and use high byte `H`, else `c` = 0 and
       high byte 0. Emit (`n` & 0x7F) + 2 units, each `high·256 + ((plain[k] + c) & 0xFF)`
       where `k` is the index of the unit being emitted.
     Each emitted unit increments `k`. An op whose bytes are missing ends decoding.
  libarchive bounds the output at name-size units.

#### Extended time

A 16-bit field followed by up to four records, for modification, creation, access and
archive time in that order. Each time owns 4 bits of the field: bits 15–12 modification,
11–8 creation, 7–4 access, 3–0 archive. For a time whose nibble has bit 3 set:

- except for the modification time (which reuses the header's DOS time), a 4-byte DOS time
  follows;
- then (nibble & 3) bytes of a sub-second fraction in units of 100 ns, the bytes being the
  most significant ones of a 24-bit value (1 byte gives bits 23–16, 2 bytes bits 23–8, …,
  least significant byte first);
- nibble bit 2 adds one second.

rar 6.24 writes `00 F0` + 3 fraction bytes by default (modification time only).

### 2.6 Subblocks (0x7A)

Same layout as a file header; the name gives the kind. They are not part of the file list.
rar 6.24 writes `CMT` (archive comment, before the first file) and `RR` (recovery record,
after the last file). The comment's data is compressed with the §3 algorithm exactly like a
file (method 0x33, version 29; the file-CRC field is the CRC of the comment text). The RR
header carries extra bytes after the name (`Protect+` …) and the data is the recovery
record; skip it. libarchive skips all subblocks.

### 2.7 End of archive (0x7B)

| flag | meaning (observed) |
|---|---|
| 0x0001 | another volume follows |
| 0x0002 | a 4-byte CRC follows: CRC-32 of all bytes of this volume before this block (from offset 0, the marker included) |
| 0x0004 | (set in volumes, meaning not established; the 7 trailing zero bytes below) |
| 0x0008 | a 2-byte volume number follows (0-based) |

A plain archive ends with `C4 3D 7B 00 40 07 00` (flags 0x4000, size 7). A volume ends with
flags 0x400F (0x400E on the last), size 20: CRC (4), volume number (2), 7 zero bytes. The
end block is optional (libarchive accepts archives without it).

### 2.8 Locating and reassembling file data

- Non-split file: its packed bytes are the packed-size bytes after its header.
- Split file (volumes): the file appears in consecutive volumes with the same name; the first
  part has flag 0x0002, middle parts 0x0003, the last 0x0001. The packed stream is the
  concatenation of the parts' data areas, in volume order; the bit reader of §3 runs across
  the boundaries as if they were not there. Header fields other than the sizes and CRC are
  repeated. In a part with 0x0002 set, the file-CRC field is the CRC-32 of *that part's
  packed bytes*; in the last part it is the CRC-32 of the whole unpacked file; each header
  carries the total unpacked size, and packed size counts that part only.
- A volume set can be read as one byte stream by concatenating the volumes: a reader that
  accepts marker and main headers anywhere between blocks sees the continuation headers in
  order (this is how libarchive and `cat *.part*.rar | bsdtar -xf -` work).
- Unpacked output is checked against the file CRC (CRC-32 of the unpacked bytes).
- Stored files (0x30): the data area is the content.
- Solid archives: the files' packed streams are separate (each starts at its own data area,
  byte-aligned), but the decoder state of §3.2 carries from one file to the next whenever the
  next header has flag 0x0010. Extracting file *k* requires decoding files 0..*k*−1.
  rar 6.24 can reset the solid state at volume starts (`-s=v` etc.); a reset shows as a file
  without 0x0010.

### 2.9 Encryption

Flag 0x0004 (file) and main flag 0x0080 (headers) mean AES encryption keyed by the password
and salt. libarchive reports them and does not decrypt. Out of scope.

## 3. The v2.9 compressed stream: LZ blocks

### 3.1 Overview

The stream is a sequence of *blocks*. Each block starts byte-aligned with a header (§3.3)
selecting LZ (Huffman-coded literals and matches) or PPMd (§4). Output bytes go to a sliding
window; filters (§5) post-process declared ranges of the output.

### 3.2 Decoder state

| state | initial value | reset |
|---|---|---|
| window (power of two ≥ dictionary size of §2.4) | zeros | per non-solid file |
| output position *P* (bytes produced; window index = *P* mod size) | 0 | per non-solid file |
| previous code lengths *Lprev*[404] | zeros | per non-solid file; also when a table's keep bit is 0 |
| current Huffman codes (main, distance, low-distance, length) | — | per table |
| repeat cache *R*[0..3] (distances) | 0, 0, 0, 0 | per non-solid file |
| last match (*LD*, *LL*) | (0, 0) | per non-solid file |
| low-offset memo *LO* and its run counter *LRUN* | 0, 0 | per non-solid file and at every LZ table read |
| "read tables at start" flag *T* | true | set by the end-of-file signal (§3.9) |
| PPMd model, escape byte *E* (initially 2), PPMd active | none | per non-solid file |
| filter programs, last program index, pending filters (§5) | empty | per non-solid file |

*(libarchive)* does not reset *R*, *LD*/*LL*, nor the filter programs between non-solid
files; valid streams do not depend on that (rar 6.24 starts each non-solid file's first
filter record with the reset form, §5.2).

### 3.3 Block header

At a block start (file start when *T*, after main symbol 256 + bit 1, after PPMd escape
code 0): align, then read 1 bit. 1 = PPMd block (§4.1). 0 = LZ block:

1. Reset *LO* = 0, *LRUN* = 0.
2. Read 1 bit *keep*. If 0, set *Lprev*[i] = 0 for all i.
3. Read the 20 precode lengths: repeat until 20 values are known: read 4 bits *v*. If
   *v* ≠ 15, the next length is *v*. If *v* = 15, read 4 bits *z*: *z* = 0 means a length of
   15; otherwise *z* + 2 lengths of 0 (truncated at 20).
4. Build the precode (§3.4) and decode 404 code lengths, i = 0..403:
   - symbol 0–15: *Lnew*[i] = (*Lprev*[i] + symbol) mod 16; i += 1 (a delta against the
     previous table; with *keep* = 0 the previous values are 0);
   - 16: read 3 bits *n*; repeat *Lnew*[i−1] for *n* + 3 entries;
   - 17: read 7 bits *n*; repeat *Lnew*[i−1] for *n* + 11 entries;
   - 18: read 3 bits *n*; *n* + 3 entries of 0;
   - 19: read 7 bits *n*; *n* + 11 entries of 0.
   Runs stop at 404. Symbols 16/17 at i = 0 are invalid.
5. *Lprev* := *Lnew*. Split: entries 0–298 main code (299 symbols), 299–358 distance code
   (60), 359–375 low-distance code (17), 376–403 length code (28). Build the four codes.

### 3.4 Canonical Huffman codes

Length 0 = symbol absent; lengths 1–15. Codes are assigned in order of increasing length,
and within a length in increasing symbol order, as consecutive integers, the code value
doubling (shift left) when moving to the next length; codes are read MSB-first. Formally:
`code = 0; for len in 1..15: for sym in order: if L[sym] == len: assign code to sym; code += 1;
code <<= 1` (the shift after each length). An incomplete code is accepted (reading an
unassigned code is an error); an over-subscribed set is invalid. An alphabet with all lengths
0 is allowed as long as it is never read.

### 3.5 Main symbols

| main symbol | meaning |
|---|---|
| 0–255 | literal byte |
| 256 | read 1 bit: 1 = a new block header follows (§3.3, possibly PPMd); 0 = end of file (§3.9) |
| 257 | filter record (§5.1) |
| 258 | repeat the last match: distance *LD*, length *LL*; nothing if *LL* = 0. *R* unchanged (*R*[0] = *LD* always holds after a match, see below) |
| 259–262 | repeat distance *R*[k], k = sym − 259; length = length-code symbol (table B) + 2 |
| 263–270 | short match, length 2, distance from table C |
| 271–298 | match with explicit distance; length from table A (symbol − 271) |

Table A/B, length symbols (main 271–298 use index = symbol − 271, length = base + 3 +
extra; length-code symbols 0–27 after 259–262 give base + 2 + extra):

| index | base | extra bits | main 271+ lengths | after 259–262 |
|---|---|---|---|---|
| 0–7 | 0–7 | 0 | 3–10 | 2–9 |
| 8 | 8 | 1 | 11–12 | 10–11 |
| 9 | 10 | 1 | 13–14 | 12–13 |
| 10 | 12 | 1 | 15–16 | 14–15 |
| 11 | 14 | 1 | 17–18 | 16–17 |
| 12 | 16 | 2 | 19–22 | 18–21 |
| 13 | 20 | 2 | 23–26 | 22–25 |
| 14 | 24 | 2 | 27–30 | 26–29 |
| 15 | 28 | 2 | 31–34 | 30–33 |
| 16 | 32 | 3 | 35–42 | 34–41 |
| 17 | 40 | 3 | 43–50 | 42–49 |
| 18 | 48 | 3 | 51–58 | 50–57 |
| 19 | 56 | 3 | 59–66 | 58–65 |
| 20 | 64 | 4 | 67–82 | 66–81 |
| 21 | 80 | 4 | 83–98 | 82–97 |
| 22 | 96 | 4 | 99–114 | 98–113 |
| 23 | 112 | 4 | 115–130 | 114–129 |
| 24 | 128 | 5 | 131–162 | 130–161 |
| 25 | 160 | 5 | 163–194 | 162–193 |
| 26 | 192 | 5 | 195–226 | 194–225 |
| 27 | 224 | 5 | 227–258 | 226–257 |

(Index *i* ≥ 8 has extra bits ⌊(*i* − 4)/4⌋ and base doubling every 4 indices.) The extra
bits follow the symbol directly.

Table C, short distances (main 263–270, index = symbol − 263, distance = base + 1 + extra):

| index | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 |
|---|---|---|---|---|---|---|---|---|
| base | 0 | 4 | 8 | 16 | 32 | 64 | 128 | 192 |
| extra bits | 2 | 2 | 3 | 4 | 5 | 6 | 6 | 6 |
| distances | 1–4 | 5–8 | 9–16 | 17–32 | 33–64 | 65–128 | 129–192 | 193–256 |

### 3.6 Explicit distances (main 271–298)

After the length (symbol + extra bits), read a distance-code symbol *s* (0–59).
Distance = base(*s*) + 1 + extra, where the extra value has *b*(*s*) bits:

| slots | extra bits *b* | distances |
|---|---|---|
| 0, 1, 2, 3 | 0 | 1, 2, 3, 4 |
| 4, 5 | 1 | 5–6, 7–8 |
| 6, 7 | 2 | 9–12, 13–16 |
| 8, 9 | 3 | 17–24, 25–32 |
| 10, 11 | 4 | 33–48, 49–64 |
| 12, 13 | 5 | 65–96, 97–128 |
| 14, 15 | 6 | 129–192, 193–256 |
| 16, 17 | 7 | 257–384, 385–512 |
| 18, 19 | 8 | 513–768, 769–1024 |
| 20, 21 | 9 | 1025–1536, 1537–2048 |
| 22, 23 | 10 | 2049–3072, 3073–4096 |
| 24, 25 | 11 | 4097–6144, 6145–8192 |
| 26, 27 | 12 | 8193–12288, 12289–16384 |
| 28, 29 | 13 | 16385–24576, 24577–32768 |
| 30, 31 | 14 | 32769–49152, 49153–65536 |
| 32, 33 | 15 | 65537–98304, 98305–131072 |
| 34, 35 | 16 | 131073–196608, 196609–262144 |
| 36–47 | 16 | 262145 + 65536·(s − 36) … +65535 |
| 48–59 | 18 | 1048577 + 262144·(s − 48) … +262143 |

base(*s*) is the sum of 2^*b* over all lower slots (base(0) = 0).

How the extra bits are read:

- *s* ≤ 9: read *b* bits directly.
- *s* ≥ 10: read *b* − 4 high bits directly (none for slots 10, 11), giving `high`; the low
  4 bits come from the low-offset mechanism:
  - if *LRUN* > 0: *LRUN* −= 1, low = *LO* (no bits read);
  - else read a low-distance-code symbol *q*: if *q* = 16, *LRUN* = 15 and low = *LO*;
    otherwise low = *q* and *LO* = *q*.
  - extra = high·16 + low.

Then the length is adjusted: +1 if distance ≥ 0x2000, and another +1 if distance ≥ 0x40000.
So explicit matches have lengths 3–258 below 8 KiB, 4–259 below 256 KiB, 5–260 above.

### 3.7 Repeat-cache and last-match updates

- 259–262 with k: the match uses *R*[k]; move it to the front (*R*[k] removed, the entries
  before it shift up one, *R*[0] = that distance).
- 263–270 and 271–298: push the distance to the front (*R*[3] drops out).
- 258: no change.
- After every match (258–298): *LD*, *LL* := its distance and length.
- *(unverified)* PPMd matches (§4.4) do not touch *R*, *LD*, *LL* (libarchive); no sample
  distinguishes the two possibilities.

### 3.8 Copying

A match copies *length* bytes from window position *P* − distance, one byte at a time
(overlap repeats the pattern), modulo the window size. Distances never exceed the window
nor *P* in valid streams (libarchive's window starts zeroed, so it would yield zeros).

### 3.9 End of file, solid continuation

- In an LZ block the file normally ends with main symbol 256, bit 0, then 1 bit *T*: 1 =
  the next file (solid) starts with a block header, 0 = it continues with the current
  Huffman codes and no header. Padding to the byte boundary follows (zero bits).
- Decoders stop at the unpacked size. The end signal can follow the last byte after further
  data: rar 6.24 sometimes writes 256, 1, a whole new (dummy) table, and only then 256, 0,
  *T*. A solid decoder must therefore keep decoding after the last byte until it meets the
  end signal (or the data ends) to learn *T*.
- In a PPMd block the end of file is escape code 2 (§4.4); the next solid file always starts
  with a block header (*T* = true).
- Across solid files the window, *P*, *R*, *LD*/*LL*, *Lprev*, codes, PPMd model, escape
  byte and filter programs carry over; each file starts a fresh bit reader at its own data
  area.

## 4. PPMd blocks

### 4.1 Header

After the PPMd bit (1) of §3.3, read 7 bits *F*:

- *F* & 0x20: a new model. Read 8 bits *M*; memory = (*M* + 1) MiB. The order is
  *o* = (*F* & 0x1F) + 1, and if *o* > 16, *o* = 16 + 3·(*o* − 16) — orders 2–16 and 19,
  22, …, 64 are expressible (*o* = 1 is invalid in libarchive).
- *F* & 0x40: read 8 bits, the new escape byte *E*. Without 0x40 the previous *E* stays —
  *verified*: a solid file whose block has flags 0x23 decodes only with the escape of the
  previous file's block. *(libarchive resets *E* to 2 instead.)* Initial *E* = 2.
- Without 0x20 the existing model continues as it is (rar 6.24 does this at the start of a
  solid file, flags 0x43); there must be one. The order bits are then ignored.

Field order: *F*, then *M* if 0x20, then *E* if 0x40. A block header is byte-aligned and
1 + 7 + 8 + 8 bits keep the alignment; the range decoder (§4.3) then starts at that byte.

With a new model: allocate a PPMd var.H model of the given size and initialise it with the
given order (lzma-sdk `Ppmd7_Construct`, `Ppmd7_Alloc(memory bytes)`, `Ppmd7_Init(order)`).
Every PPMd block, new model or not, (re)starts the range decoder.

### 4.2 Model

The model is PPMd variant H exactly as lzma-sdk's `C/Ppmd7.c` (contexts, SEE, binary
contexts, `Ppmd7_Update1`, `Update1_0`, `Update2`, `UpdateBin`, `UpdateModel`, restart on
memory exhaustion). Symbol decoding follows lzma-sdk `C/Ppmd7Dec.c` (`Ppmd7z_DecodeSymbol`)
step for step — first the masked/unmasked frequency walk, the binary-context path with its
probability update and `InitEsc` assignment, the escape loop with `Ppmd7_MakeEscFreq` and SEE
update — with only the range-coder primitives replaced by those of §4.3 (7-Zip's own name
for this combination is "Ppmd7a", the original PPMd range coder). Decoding an escape from the
order −1 context is an error here (the 7z variant's end marker does not exist).

libarchive's `archive_ppmd7.c` is the same model with a pluggable range decoder. (It also
copies the escape byte into the model's `InitEsc` when a header carries 0x40; that value is
overwritten before any use, so it has no effect.)

### 4.3 Range decoder (where RAR differs from 7z's Ppmd7z)

State: 32-bit *Low*, *Range*, *Code*; constants TOP = 2^24, BOT = 2^15.

- Init: *Low* = 0, *Range* = 0xFFFFFFFF, *Code* = the next 4 bytes, big-endian. (7z's
  variant reads a leading zero byte first; RAR does not.)
- GetThreshold(total): *Range* := *Range* / total (integer); return
  (*Code* − *Low*) / *Range* (all mod 2^32).
- Decode(start, size): *Low* += start·*Range*; *Range* *= size; then normalise. (7z
  subtracts from *Code* instead of adding to *Low*.)
- Normalise (after every Decode, immediately):

  ```
  loop:
    if (Low xor (Low + Range)) >= TOP:          # top byte settled?
      if Range >= BOT: stop
      Range = (−Low) mod 2^32 & (BOT − 1)       # carry-less underflow fix
    Code = (Code << 8 | next byte); Range <<= 8; Low <<= 8   (all mod 2^32)
  ```
- Binary contexts: with probability *p* (the 16-bit BinSumm entry, scale 2^14):
  v = GetThreshold(2^14); if v < *p*: bit 0, Decode(0, *p*); else bit 1,
  Decode(*p*, 2^14 − *p*). (7z computes `(Range >> 14) * p` and compares *Code*; the
  results differ.) The probability update and the rest of the binary path are the model's.
- The frequency paths call GetThreshold(SummFreq or the escape-loop total) and then Decode
  with the chosen interval, exactly where `Ppmd7Dec.c` calls its RC macros.

Bytes consumed: 4 at init plus one per normalisation step. This count is where the block
ends: after an escape code 0 the next block header starts at the byte after the last byte
the range decoder has read.

### 4.4 Symbols and escapes

Decode a symbol *s*. If *s* ≠ *E*: output byte *s*. Otherwise decode a code *c*:

| *c* | meaning |
|---|---|
| 0 | end of PPMd block: a new block header follows at the next byte (§3.3) |
| 1 | *(observed)* literal byte *E* |
| 2 | end of file (and of the block); the next solid file starts with a block header |
| 3 | filter record: the bytes of a §5.1 record (flags byte, length bytes, body), each decoded as a PPMd symbol *(format unverified: libarchive does not support it and rar 6.24 never writes it)* |
| 4 | match: decode 4 more symbols *b*1, *b*2, *b*3, *b*4; distance = (*b*1·2^16 + *b*2·2^8 + *b*3) + 2, length = *b*4 + 32 |
| 5 | match: decode *b*; distance 1, length = *b* + 4 |
| other | *(libarchive)* output byte *E* |

PPMd matches copy from the window like LZ matches. Escape-coded bytes (the code and the
match parameters) do not get output.

### 4.5 End of a PPMd stream

After escape code 2 the encoder flushes: the 4 bytes of *Low*, most significant first —
these are the last 4 bytes the decoder reads in §4.3, so *Code* = *Low* when it stops.
*(observed)* Every PPMd stream ending a file, from rar 6.24 and from the older RAR versions
behind libarchive's test archives, then has 8 more bytes `00 BF 88 67 F6 A9 FF D4`. At an
escape code 0 (switch to LZ) there is the 4-byte flush and no trailer.

## 5. Filters

### 5.1 Record framing

An LZ main symbol 257 (or PPMd escape 3) introduces a record. Read a byte *G* (8 bits from
the bit stream, no alignment). Body length: *G* & 7 = 0–5 → (*G* & 7) + 1 bytes; 6 → read a
byte *n*, length *n* + 7; 7 → read two bytes, length = first·256 + second. Then the body
bytes (8 bits each). The body is read with its own MSB-first bit reader; reading past its end
makes the record invalid; leftover bits are padding (zero in rar output).

### 5.2 Record body

Numbers use the *VM number* format: read 2 bits *t*: 0 → 4 bits; 1 → 8 bits *v*, and if
*v* < 16 the value is 0xFFFFFF00 | *v*·16 | (4 more bits); 2 → 16 bits; 3 → 32 bits.

Fields in order:

1. If *G* & 0x80: number *N*. *N* = 0 deletes all programs and pending filters and selects
   index 0 (a new program follows); otherwise index = *N* − 1, which must be ≤ the number of
   known programs (equal = new program). Without 0x80 the index is the last one used.
2. Number *S*: block start = *P* (current output position) + *S*, plus 258 if *G* & 0x40.
3. If *G* & 0x20: number = block length; otherwise the length last used with this program
   (0 for a new one).
4. If *G* & 0x10: 7 bits *m*; for i = 0..6 with bit i of *m* set (bit 0 first), a number for
   register *r*i. Unset registers are 0.
5. If the index names a new program: number *C* (1 ≤ *C* ≤ 65536), then *C* bytecode bytes
   (§5.3); the program is appended to the list.
6. If *G* & 0x08: number *D* (≤ 0x2000 − 0x40), then *D* bytes of global data (only a VM
   program could use them).

Registers handed to the filter (VM convention): *r*0–*r*6 as above (by default 0), then
*r*3 = 0x3C000, *r*4 = block length, *r*5 = the program's use count (0 the first time it is
used, +1 on each later record naming it), *r*7 = 0x40000 — in libarchive *r*3–*r*5 are set
*before* the mask overrides, so an explicit register 4 would replace the length seen by
E8. The standard filters read only *r*0, *r*1 and the length.

Per-program state: the last block length and the use count. Global state: the program list
and the last used index; for solid streams they carry over (rar 6.24's second solid file
continues numbering, e.g. *N* = 2 for a new program after one existing).

### 5.3 Bytecode and standard filters

Bytecode byte 0 is the XOR of bytes 1..*C*−1 (checked). Then, bit-wise from byte 1, a flag
bit "static data present" (then a VM number = size − 1 and the bytes), followed by RarVM
instructions. Standard programs are recognised by length and CRC-32 of the whole bytecode
and executed natively; a decoder without a RarVM supports only these:

| filter | bytecode length | CRC-32 |
|---|---|---|
| delta | 29 | 0x0E06077D |
| x86 E8 | 53 | 0xAD576887 |
| x86 E8/E9 | 57 | 0x3CD7E57E |
| RGB | 149 | 0x1C2C5DC8 |
| audio | 216 | 0xBC85E701 |
| Itanium | ? | ? (exists in the format; not covered by any source here) |

A re-encoder needs the exact bytecode to declare a program. As written by rar 6.24 (and
found in libarchive's test archives):

```
delta  2f019a4180ec27482f09766dd3ea415b5944e8175ce16c914c4e3f7700
e8     971b012807069808000000d13a101592ec50cb9920b925f0291915530312ae511035592b6004156d4066ab023449043602523e9700
e8e9   841b0128111069808000000d13a101c689d280ac9762855cc905c92f8148c8aa981895728881aac95b0020ab6a03355811a24821b01291f4b8
rgb    c5019a4195c9a64dba4b140af49b804c0015a6a807262ac9c48b8662320f8664240666711998cc433331990066883330ccd10e980b3334400cd14666199a28cc4980b3334500cd18666199a30cc8980b3334604cd10668a520626688334628050f320c4cd14668c50041e48fc8855e027cc9268183b09dc2de9c78acd668b40e71dbb249386e022a2c412b109882490314f4e19700
audio  47019a4195e5720dc26482749324b14006d8384400a8013411dca1ba01990cc4033119a4066622604d9a400d668e60d030401826c1c8f6e62613789208e850bc5a07c6e9f520a9a0ed3733473966907019a39bcf258380c1bd30166e233493811609b050183b4dc84c059b88c528e0769390980b37118a59c480424843a947ee43346047d44a0dbbd359a486ee05094026c9342476a0306a20ea022004a041509e503fe6e128944601bd8b40f0681136c9a1923811419ca89510ee50662b00209511040262ac668c6aca2640b2671b4b26cc648a6271a2b8
```

### 5.4 When filters run

Filters act on output positions, not on the window: the window keeps the unfiltered bytes
(later matches copy unfiltered data), and only the bytes handed out as the file's content are
filtered. Pending filters are kept in declaration order; their blocks must not overlap
(a later block starts at or after the end of the earlier one) with one exception: a filter
whose start equals the previous one's start and whose length equals the previous one's
output length is chained — it runs on the previous filter's output. When output reaches the
end of the first pending block, the block's bytes are filtered and emitted. Filters apply to
LZ and PPMd output alike. rar 6.24 never chains filters and declares all filters of a file in
the file's first LZ block, at position 0 of the file (§7.4).

libarchive's limits: E8/E8E9 blocks 5..0x3C000 bytes; delta, RGB, audio at most 0x1E000
bytes (output is placed after the input in the 256 KiB VM memory); a block length larger
than the window is invalid.

### 5.5 Standard filter semantics

In the following, `in` is the block (unfiltered), `out` the result, *L* the block length,
arithmetic on bytes mod 256.

**Delta** (*r*0 = channel count *n*, 1..128): the input holds the channels one after another.

```
k = 0
for c in 0..n-1:
  prev = 0
  for j = c, c+n, c+2n, … < L:
    prev = prev − in[k]; k += 1
    out[j] = prev
```

**E8** and **E8E9**: in place; *F* = the block's offset *within the current file* (bytes of
this file before the block start; in solid archives counted from the file's own start —
verified, the absolute stream position fails).

```
i = 0
while i <= L − 5:
  if in[i] == 0xE8 or (E8E9 and in[i] == 0xE9):
    A = signed 32-bit LE at i+1
    C = (F + i + 1) mod 2^32
    if A < 0 and C >= −A:      A = A + 2^24
    elif 0 <= A < 2^24:        A = A − C
    (else unchanged)
    store A at i+1 (mod 2^32); i += 5
  else: i += 1
```

**RGB** (*r*0 = *S*, *r*1 = *B* ∈ {0, 1, 2}; requires 3 ≤ *L*, *S* ≤ *L*). Channel-major
input like delta, 3 channels; for a 24-bit image *S* = row bytes + 3 (rar 6.24: 903 for a
300-pixel BMP row).

```
k = 0
for c in 0..2:
  p = 0
  for j = c, c+3, … < L:
    if j >= S:
      up = out[j − S + 3]; ul = out[j − S]
      d1 = |up − ul|; d2 = |p − ul|; d3 = |up − ul + p − ul|     (integers, not mod 256)
      if d1 > d2 or d1 > d3: p = (d2 <= d3) ? up : ul
    p = p − in[k]; k += 1
    out[j] = p
for i = B, B+3, … while i < L − 2:
  out[i] += out[i+1]; out[i+2] += out[i+1]
```

**Audio** (*r*0 = channel count *n*, 1..128). Channel-major input; each channel has its own
predictor:

```
for c in 0..n-1:
  W1 = W2 = W3 = 0                     # weights, signed
  D1 = D2 = D3 = 0; last = 0; lastdelta = 0   # lastdelta signed 8-bit
  e[0..6] = 0; count = 0
  for j = c, c+n, … < L:
    d = in[k] as signed byte; k += 1
    D3 = D2; D2 = lastdelta − D1; D1 = lastdelta
    pred = floor((8·last + W1·D1 + W2·D2 + W3·D3) / 8) mod 256
    v = (pred − d) mod 256
    E = 8·d
    e[0] += |E|; e[1] += |E − D1|; e[2] += |E + D1|; e[3] += |E − D2|
    e[4] += |E + D2|; e[5] += |E − D3|; e[6] += |E + D3|
    lastdelta = (v − last) as signed byte; last = v; out[j] = v
    if count mod 32 == 0:              # also on the first sample
      x = the smallest index with the minimum e[x] (scan 0..6, strict <)
      e[0..6] = 0
      x=1: if W1 >= −16: W1 −= 1      x=2: if W1 < 16: W1 += 1
      x=3: if W2 >= −16: W2 −= 1      x=4: if W2 < 16: W2 += 1
      x=5: if W3 >= −16: W3 −= 1      x=6: if W3 < 16: W3 += 1
    count += 1
```

**Itanium**: a standard filter of the format, but neither libarchive nor rar 6.24's `-ma4`
(its `-mc` modules are A, C, D, E, T) implement it, and its semantics are not covered by
the allowed sources. A decoder may reject it.

## 6. What libarchive does not support

- Solid archives (rejected), encrypted files and headers, RAR 1.5/2.x compression
  (versions < 29, silently misdecoded), RarVM programs other than the five standard ones,
  the Itanium filter, filter records inside PPMd (escape 3), comments/ACL/stream subblocks
  (skipped), seeking in compressed files.
- Deviations: the escape byte reverts to 2 for a PPMd header without 0x40; the window is
  sized from the file size (≤ 4 MiB) rather than the dictionary bits; repeat state and
  programs are not reset between files.
- Failures on valid archives with bsdtar 3.7.4: an LZ block that switches to PPMd before
  producing output (`pp_mct.rar`'s `audio.wav`: "Internal error extracting RAR file"),
  filters over PPMd output (`ppmd_x86.rar`, `test_read_format_rar_filter_incomplete_block.rar`),
  `test_read_format_rar3_lowdist_reset.rar` (fixed in the checked-out source), multi-volume
  sets given as one file argument (pipe the concatenation instead).

## 7. Encoder freedom

Everything below is accepted by the decoder and must be recorded (or predicted) by a
bit-exact re-encoder. Observations are from rar 6.24 `-ma4` on the fixtures of §9 (and
libarchive's test archives, made by older RAR versions, where noted).

### 7.1 Container

- All header fields are free data: names (and the Unicode encoding ops of §2.5 — rar chooses
  them), times and the extended-time layout, attributes, host OS, version (29 vs 20),
  method byte (0x31–0x35 all decode the same), header flags such as 0x4000, subblocks,
  recovery record, volume sizes and split points, end blocks.
- Dictionary bits: rar 6.24 uses the smallest power of two ≥ the file size (whole solid set),
  at least 128 KiB, capped by `-md` (default 4 MiB for `-ma4`; `-md64k` gives bits 000). The bits limit the distances the encoder may use.
- Stored vs compressed: rar stores (method 0x30, version 29) when compression does not pay.

### 7.2 LZ blocks and tables

- Block boundaries: rar 6.24 starts a new table after 16384 main symbols (16383 in a file's
  first block, not counting the 256 that switches), with exceptions near filters, PPMd
  switches and file ends.
- Keep bit: rar writes 1 except for the first table of a file and the first after PPMd, plus
  occasional 0 (e.g. every 16 KiB in delta-filtered data).
- Code lengths: each is free (1–15) as long as the code is decodable. rar 6.24's lengths
  equal a plain Huffman construction on the block's symbol counts (the 256 that ends the block
  included) where equal weights merge the shallower subtree first (heap order: weight, then
  subtree depth, then creation order) — every table of rar 6.24 output except one matched;
  older RAR versions (libarchive's archives) often differ. Lengths reach 15 (observed; the
  format maximum). Unused symbols get 0; an alphabet with one used symbol gets a second
  symbol (often symbol 0) so that both have length 1; an unused alphabet is all 0.
- Precode lengths: the same Huffman rule applied to the counts of the precode symbols
  (verified on all rar 6.24 tables). Precode transmission: a run of ≥ 3 zero lengths is
  written with the 15/*z* escape (runs up to 17), shorter runs literally; a real length 15
  would need 15, 0 (never occurs).
- Code-length run-lengths (verified on all 606 tables): scanning the 404 new lengths, a run
  of ≥ 3 zeros → 18 (3–10) or 19 (11–138), longer runs split greedily into 138-chunks then
  the rest; otherwise a run of ≥ 3 entries equal to the previous (nonzero) length, counted
  from the entry after the first → 16 (3–10) or 17 (11–138); otherwise the delta symbol.
  The zero-run test comes first.
- End of the stream: 256, 0, *T*, zero padding; sometimes a final 256, 1 + a dummy table
  (main code {0, 256}) before it. *T* is 1 except inside solid archives where the next file
  keeps the codes.
- Alignment padding before a block header: zero bits.

### 7.3 The parse

The sequence of literals and matches (positions, lengths, distances) is the main freedom
(a "record of the parse"). Given a match (*d*, *l*), rar 6.24's choice of encoding is almost
deterministic:

- (*d*, *l*) = (*LD*, *LL*) → 258;
- else *d* = *R*[k] (lowest such k) → 259 + k with length *l* (2–257);
- else *l* = 2 and *d* ≤ 256 → short symbol 263–270;
- else explicit 271–298 (needs *l* ≥ 3, 4 or 5 by distance class).

Exceptions: a few explicit matches whose distance was in *R* (at internal chunk boundaries
of the encoder, e.g. every 30000 bytes in `zeros.rar`, at 2^20-aligned positions in
`big_md4m.rar`); the re-encoder needs one flag per match for them. rar caps every match at
257 bytes (explicit lengths 258–260 never occur) and splits longer runs (continuation pieces
then come out as 258 or 259). The low-offset repeat symbol 16 is optional: rar 6.24 used it
once in `m2_x86.rar` and never at `-m3`/`-m5`; when it is used, the next 15 qualifying
distances must share the low 4 bits.

### 7.4 Filters

- Choice and placement: rar 6.24 declares *all* filters of a file in one LZ block at the
  file's start (position 0 of the file, before any data; if the file's data is PPMd, an LZ
  block with a two-symbol main code carries the records and then switches to PPMd). E8 and
  E8E9 blocks are 64 KiB aligned to file offsets (the last one shorter); delta blocks are
  multiples of 256 bytes; audio blocks 64 KiB; RGB covers the pixel data.
- Record encoding is fully determined by the semantic fields (verified on 458 records):
  0x80 iff the program differs from the last used one (or is new); *N* = 0 when no programs
  exist (file start, non-solid), else index + 1; 0x40 iff start − *P* ≥ 258; 0x20 iff the
  length differs from that program's last length (or the program is new); 0x10 iff some
  register is nonzero, with *m* naming exactly the nonzero ones (delta/audio: *r*0; RGB:
  *r*0, *r*1); 0x08 never; every VM number in its shortest form; zero padding bits; body
  length in the shortest framing (1–6 direct, 7–262 via 6, else 7).
- PPMd escape 3 is never used.

### 7.5 PPMd

- When to use PPMd and where to switch (e.g. `-mct` switched at 128 KiB / 256 KiB
  boundaries inside a file), order and memory (`-mc<order>:<MB>t`; default order 4 and
  *M* = 14; order 63 rounds down to 61), new model vs continued model (rar continues the
  model at a solid file start: flags 0x43), and whether to resend *E*.
- Escape byte: rar 6.24 picks the least frequent byte value in the first 64 KiB of the
  block's data, the highest value on ties (255 for text without 0xFF).
- Escape code 1 for a literal *E*; escape 4/5 matches (rar used 5 a lot, 4 rarely).
- Range-encoder flush: 4 bytes of *Low*; then the 8-byte trailer at the end of a file.

### 7.6 Solid streams

- *T* at each file end and the keep bit of the next table; whether the next file's first
  block is LZ or PPMd; the shared filter-program numbering.

## 8. Decoder outline

```
decode_file(data, size, solid):
  if not solid: reset state (§3.2)
  bits = reader over data; base = P
  if T: read_block_header()
  while P − base < size (and, for solid, until the end signal):
    if PPMd: decode per §4.4
    else:
      s = main code symbol
      0–255: put byte;  256: bit ? read_block_header() : (T = bit; stop)
      257: read filter record;  258–298: match per §3.5–3.7
  apply pending filters whose blocks end within the file (§5.4)
  check CRC-32 of the output
```

## 9. Test vectors

### 9.1 Generated (`third_party/rar-dl/fixtures4/`, gitignored)

Made with rar 6.24 `a -ma4 -ep`; originals in `fixtures4/corpus/`; `INDEX.txt` lists the
switches. All pass `unrar t`. "bsdtar" = `bsdtar -xf` (libarchive 3.7.4) result.

| fixture | exercises | bsdtar |
|---|---|---|
| `m0_text.rar`, `random.rar`, `m1_hello.rar` | stored files (version 20 / 29) | ok |
| `m1_text.rar`, `m3_text.rar`, `m5_text.rar` | LZ, single table, short distances, repeats | ok |
| `m1_x86.rar`, `nofilters_x86.rar` | 21 tables, keep bit 1 with deltas, 2 MiB window, no filters | ok |
| `m2_x86.rar` | low-offset repeat symbol 16 | ok |
| `m3_x86.rar` | E8 + E8E9 + delta (12-channel) filters, program switching (0x80 numbers) | ok |
| `two_x86.rar` | several non-solid files, filter reset per file | ok |
| `image.rar` | delta filter, 9 channels | ok |
| `rgb_forced.rar` | RGB filter, *S* = 903, *B* = 0 and 2; trailing dummy table before end | ok |
| `audio.rar`, `audio_forced.rar` | audio filter (4 channels) + delta | ok |
| `delta_forced.rar`, `table.rar` | delta, keep bit 0 tables | ok |
| `e8_forced.rar` | E8 on text | ok |
| `zeros.rar` | symbol 258 chains, matches of 257 | ok |
| `skew.rar`, `fib.rar` | code lengths 15, Huffman shape | ok |
| `big_md64k.rar`, `big_md4m.rar` | dictionary bits 000 and 110, slots ≥ 48, 95 filters | ok |
| `ppmd_text.rar` | PPMd only, order 4, *M* 14, *E* 255, flush + trailer | ok |
| `pp_mct.rar`, `pp_o2_m1.rar`, `pp_o8_m16.rar`, `pp_o63_m128.rar` | orders 2/4/8/61, memory 1–128 MB; `audio.wav`: LZ filter block → PPMd (*E* 186) → LZ | text ok, audio fails |
| `ppmd_x86.rar` | LZ block carrying E8/E8E9 records, then PPMd (*E* 167): filters over PPMd output | fails |
| `solid_all.rar` | solid LZ, 6 files, *T* = 0 and 1, programs shared, E8 offset per file | unsupported |
| `solid_ppmd.rar` | solid, PPMd → LZ switch inside a file (keep 0) | unsupported |
| `solid_ppmd_esc.rar` | escape kept across a PPMd header without 0x40 | unsupported |
| `solid_ppmd_filters.rar` | PPMd model continued (flags 0x43), LZ filter blocks in between | unsupported |
| `vol_x86.part1-3.rar` | split file, per-part CRCs, end blocks with CRC and number | ok (concatenated) |
| `vol_solid.part1-4.rar` | solid + volumes | unsupported |
| `names.rar` | Unicode name encoding, directory | ok |
| `cmt_rr.rar` | CMT subblock (LZ-compressed comment), RR subblock | ok |
| `times.rar` | extended times (mtime, ctime, atime) | ok |
| `locked.rar`, `symlink.rar` | main flag 0x0004; symlink stored as target | ok |

### 9.2 libarchive's test archives (`libarchive/test/*.rar.uu`, uudecode)

| archive | content / feature | bsdtar 3.7.4 |
|---|---|---|
| `test_read_format_rar.rar`, `_windows`, `_unicode`, `_subblock`, `_noeof`, `_sfx.exe` | headers: stored files, symlinks, dirs, Unicode names (Japanese paths), Windows host, a subblock, no end block, SFX search | ok |
| `test_read_format_rar_compress_normal.rar` | LZ (older RAR) | ok |
| `test_read_format_rar_compress_best.rar` | PPMd blocks (older RAR, same 8-byte trailer) | ok |
| `test_read_format_rar_binary_data.rar` | 64 LZ tables, two files | ok |
| `test_read_format_rar_multi_lzss_blocks.rar` | several LZ tables, many 258s | ok |
| `test_read_format_rar_ppmd_lzss_conversion.rar` | PPMd ↔ LZ switches (escape 0, 256+1), PPMd matches | ok |
| `test_read_format_rar_filter.rar` | E8E9 + delta filters (`bsdcat.exe`) | ok |
| `test_read_format_rar_filter_incomplete_block.rar` | delta filter over PPMd output | fails in 3.7.4 |
| `test_read_format_rar3_lowdist_reset.rar` | low-offset state reset at a new table (symbol 16 before the reset) | fails in 3.7.4 (fixed in the source) |
| `test_read_format_rar_multivolume.part000[1-4].rar` | volumes of the PPMd/LZ conversion file | ok (concatenated) |
| `test_rar_multivolume_*.part*.rar`, `test_read_splitted_rar_a?`, `test_read_large_splitted_rar_a?`, `test_splitted_rar_seek_support_a?` | volumes; the `splitted` sets are byte splits of one archive | ok (concatenated) |
| `test_read_format_rar_encryption_*.rar`, `test_read_format_rar4_*encrypted*.rar` | encryption | unsupported |
| `test_read_format_rar_invalid1`, `_overflow`, `_ppmd_use_after_free*`, `_symlink_huge`, `_newsub_huge`, `_endarc_huge`, `_unbound_staticdata`, `_seek_data_cursor0` | malformed inputs for robustness | errors expected |

## 10. Open points

- Itanium filter: fingerprint and semantics unknown here; would need a sample from an old
  RAR version and an oracle that implements it.
- PPMd escape 3 (filter inside PPMd) and escape codes ≥ 6: not confirmed.
- Whether PPMd matches update the LZ repeat state (§3.7).
- Main flag/end-block bit 0x0004 meanings; the exact rar heuristics for block cuts,
  dictionary choice and PPMd switching (recorded as data, not needed for decoding).
