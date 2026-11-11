# InstallShield `ISc(` cabinets (data1.hdr / dataN.cab)

Source: the MIT-licensed unshield project (`lib/`: `cabfile.h`, `internal.h`, `helper.c`, `libunshield.c`,
`file.c`, `directory.c`, `file_group.c`, `component.c`, `libunshield.h`). Nothing else was consulted. All
integers are little-endian. "Unshield" below means what that reference does; where the reference is silent the
statement is marked (unsure). Symbol names are pointers only.

Reference facts that shape everything: unshield is a reader, so it only knows what it needs to read.
Bytes it skips are "unknown" below, not "zero". It only ever uses the first header (`header_list`), i.e. no
real multi-header support ("XXX: multi-volume support" comments).

## 1. Common header, volumes, .hdr versus .cab

Pointers: `CommonHeader`, `unshield_read_common_header` (helper.c); `unshield_read_headers`,
`unshield_create_filename_pattern` (libunshield.c); `unshield_fopen_for_reading` (helper.c).

### 1.1 Common header (20 bytes, at offset 0 of every .hdr and .cab)

| Off | Size | Name | Meaning |
|----:|-----:|------|---------|
| 0 | 4 | signature | `0x28635349`, bytes `49 53 63 28` = `ISc(`. Anything else: error. (`MSCF` is detected and reported as a Microsoft cabinet.) |
| 4 | 4 | version | See 1.2. |
| 8 | 4 | volume_info | Read, stored, never interpreted by the reference. (unsure what it holds; plausibly volume count/size info) |
| 12 | 4 | cab_descriptor_offset | Absolute offset in this file of the cab descriptor (section 2). |
| 16 | 4 | cab_descriptor_size | Size of the descriptor. Only tested for non-zero ("No CAB descriptor available" if 0); never used as a bound. |

A header file shorter than 4 bytes is rejected; reading the 20 bytes of a shorter file is not guarded (the
header is read whole into memory, `data`, and all later offsets index it without bounds checks except the file
table, see 6).

### 1.2 Version and the "major version"

The 32-bit `version` field is turned into a small integer `major_version` (used to select layouts) as follows,
by its top byte (`version >> 24`):

| Top byte | major_version |
|---------:|---------------|
| 1 | `(version >> 12) & 0xF` |
| 2 or 4 | `version & 0xFFFF`; if non-zero, divided by 100 (integer division) |
| anything else | stays 0 |

(Example, computed not observed: a top byte 2 with low 16 bits 600 gives 6.)
The reference also allows forcing the major version (`-i`), which overrides the derivation.

Layout selection by major_version (used everywhere below):

| major_version | file descriptor layout | volume header | notes |
|---|---|---|---|
| 0 and 5 | "v5" (0x3A bytes, offset table) | 40 bytes | 0 happens for unrecognised top bytes |
| 6..13 and every other value (including 1..4 and >= 14) | "v6+" (0x57 bytes, packed array) | 64 bytes | |
| >= 17 | same as v6+ | same | strings are UTF-16LE instead of 8-bit |

Note that 1..4 fall into the v6+ branch in the reference (`default:` of the switch), they are not specially
supported; that is probably unintentional (unsure whether any real v1..4 file parses that way).
MD5 verification is done only for major_version >= 6; the v5 descriptor MD5 is copied only when
major_version == 5 (not for 0).

### 1.3 Files and volume numbering

* Names: `NAMEn.hdr` / `NAMEn.cab`. The pattern is derived from the given file name: in the base name, cut
  at the first `.` or decimal digit; the rest is replaced by the volume number in decimal (no zero padding),
  a dot and the suffix. Lookup in the directory is case-insensitive. Volumes are numbered from 1.
* Header discovery (`unshield_read_headers`): for i = 1, 2, ...: if `NAMEi.hdr` exists, read it as the header
  and stop after it; otherwise read `NAMEi.cab` as a header and continue with i+1 until a file is missing or
  one fails to parse. Only the first header is then used. So: with a .hdr, the .hdr is the header and all
  .cab files are pure data volumes; with no .hdr, `data1.cab` itself carries the descriptor.
* `data1.hdr`: common header, then (at `cab_descriptor_offset`) the cab descriptor and everything it points
  to (file table, names, file descriptors, groups, components). The reference never reads a volume header
  from the .hdr and never reads file data from it. (unsure: whether real .hdr files contain a volume header
  after the common header; it is irrelevant for reading.)
* `dataN.cab` (data volume): common header (20 bytes), then immediately the *volume header* (section 1.4),
  then file data at absolute offsets. A `data1.cab` without a .hdr additionally contains the descriptor,
  located by its own `cab_descriptor_offset`. The signature and 20-byte common header are re-validated on
  every volume that is opened for data; its `cab_descriptor_offset` etc. are ignored there.
* Within a data volume, file data offsets (`data_offset`, `first_file_offset`, `last_file_offset`) are
  absolute offsets from the start of that .cab file.

### 1.4 Volume header (in every .cab, at offset 20)

v5 (major_version 0 or 5), 40 bytes, all u32:

| Off (from 20) | Name |
|---:|------|
| 0 | data_offset (read, not used for seeking) |
| 4 | unknown (ignored) |
| 8 | first_file_index |
| 12 | last_file_index |
| 16 | first_file_offset |
| 20 | first_file_size_expanded |
| 24 | first_file_size_compressed |
| 28 | last_file_offset |
| 32 | last_file_size_expanded |
| 36 | last_file_size_compressed |

v6+, 64 bytes; offsets and sizes are 64-bit values stored as (low u32, high u32):

| Off (from 20) | Size | Name |
|---:|---:|------|
| 0 | 8 | data_offset |
| 8 | 4 | first_file_index |
| 12 | 4 | last_file_index |
| 16 | 8 | first_file_offset |
| 24 | 8 | first_file_size_expanded |
| 32 | 8 | first_file_size_compressed |
| 40 | 8 | last_file_offset |
| 48 | 8 | last_file_size_expanded |
| 56 | 8 | last_file_size_compressed |

CAUTION: in the reference code the high halves are read into fields but all later arithmetic uses only the
low 32-bit fields (the struct fields `*_high` are never consulted). Offsets/sizes beyond 4 GiB in split
information are therefore not handled by the reference (unsure whether real files have them). The 64-bit
file descriptor `data_offset` (v6+) is, by contrast, a real 64-bit value, but it is passed to a seek taking
`long`/`_fseeki64` (OS-dependent; 32-bit `long` on Windows without the macro).

Indices (`first_file_index`, `last_file_index`) are file indices into the cab's file table, 0-based, in the
global file numbering of the descriptor. Meaning: the volume holds data for files first..last; the first
and last may be partial (continuations/beginnings of split files).

### 1.5 Split files, how flagged

* v6+: the file descriptor flag bit 1 (`SPLIT`) says the file spans volumes.
* v5: the flag in the descriptor is not trusted. When a volume is opened for a file index, unshield sets
  SPLIT itself if (a) index < file_count-1, index == that volume's last_file_index and the volume's
  last_file_size_compressed != the descriptor's compressed_size ("split file last in volume"); or else
  (b) index > 0, index == first_file_index and first_file_size_compressed != the descriptor's
  compressed_size ("split file first in volume"). (Comparison uses compressed size even for uncompressed
  files, where it is expected to equal expanded size.)
* The descriptor's `volume` field (v6+, u16) / the volume where the search starts (v5) is the volume in which the
  file *starts* (see 3). Continuation proceeds to volume+1, +2, ... (consecutive .cab numbers).

## 2. Cab descriptor

Pointers: `unshield_get_cab_descriptor`, `CabDescriptor` (cabfile.h/libunshield.c).

Located at absolute offset `cab_descriptor_offset` in the header file ("CD" below). Every offset stored
*inside* the descriptor structures is relative to CD (not to the file start), except where stated.

| Off from CD | Size | Name | Meaning |
|---:|---:|------|---------|
| 0x00 | 12 | unknown | skipped |
| 0x0C | 4 | file_table_offset | relative to CD; "FT" = CD + this |
| 0x10 | 4 | unknown | skipped |
| 0x14 | 4 | file_table_size | |
| 0x18 | 4 | file_table_size2 | warning if different from the one above; otherwise unused |
| 0x1C | 4 | directory_count | |
| 0x20 | 8 | unknown | skipped |
| 0x28 | 4 | file_count | |
| 0x2C | 4 | file_table_offset2 | relative to FT; used by v6+ for the descriptor array (section 3) |
| 0x30 | 14 | unknown | skipped (0xE bytes) |
| 0x3E | 71*4 | file_group_offsets[71] | offsets (relative to CD), 0 = empty |
| 0x15A | 71*4 | component_offsets[71] | likewise; ends at 0x276 |

(Reference comments confirm 0x3E and 0x15A as the starts of the two arrays; 71 = 0x47, `MAX_*_COUNT`.)

File groups (needed only if you want group membership; for plain enumeration all `file_count` files can be
taken by index):

* For each non-zero slot of `file_group_offsets`, there is a singly linked list (a hash bucket): a node at
  CD + off is three u32: name_offset, descriptor_offset, next_offset (all relative to CD; next 0 ends the
  list). Order of groups = slot order, then list order.
* A group descriptor at CD + descriptor_offset: u32 name_offset (relative to CD, string), then skip 0x48
  bytes if major_version <= 5, else 0x12 bytes, then i32 first_file, i32 last_file (file indices; the range
  is inclusive in unshield's usage, `first_file`..`last_file`) (unsure about the "inclusive" and about the case
  of empty groups; the reference just stores both).
* Components use the same bucket lists; the component descriptor (`unshield_component_new`) is not needed to
  list files. Skip it.

## 3. File table and file descriptors

Pointers: `unshield_get_file_table`, `unshield_directory_name` (directory.c); `unshield_read_file_descriptor`,
`unshield_file_name` (file.c); `unshield_get_utf8_string`, `unshield_header_get_string` (helper.c).

### 3.1 File table

Array of `directory_count + file_count` u32 at FT = CD + file_table_offset. All values in it are offsets
*relative to FT*.

* entries [0, directory_count): offset (rel. FT) of the NUL-terminated name of each directory.
* entries [directory_count, directory_count + file_count): in v5 (major 0 or 5): offset (rel. FT) of file
  number i's descriptor. In v6+ these entries are ignored by the reference (descriptors are found by the
  array in 3.3).

The array must fit in the header (checked: start and byte count inside the file size; empty table is an
error). `file_table_size` is not used to bound anything.

Strings (directory names, file names, group/component names): NUL-terminated, 8-bit characters when
major_version < 17 (code page not specified; unshield optionally converts, default none), UTF-16LE
NUL-terminated (two-byte units) when >= 17. File-name offset in the descriptor is relative to FT; group and
component name offsets are relative to CD. Directory names may contain backslashes (path separators; unshield
has a conversion step "-R" to leave names raw: the stored separator style is Windows) (unsure of exact
forms; e.g. an empty directory name for the root).

### 3.2 v5 file descriptor (major 0 or 5), at FT + file_table[directory_count + i]

Size 0x3A for 5 (0x2A for major 0, where the MD5 is absent; size is implied by fixed layout, there is no size
field).

| Off | Size | Name | Meaning |
|---:|---:|------|---------|
| 0x00 | 4 | name_offset | rel. FT |
| 0x04 | 2 | directory_index | index into directory table; asserted `< directory_count` |
| 0x06 | 2 | unknown | skipped |
| 0x08 | 2 | flags | section 3.4 |
| 0x0A | 4 | expanded_size | |
| 0x0E | 4 | compressed_size | |
| 0x12 | 0x14 | unknown | skipped |
| 0x26 | 4 | data_offset | absolute offset in the data volume file (see 4/1.3) |
| 0x2A | 16 | md5 | v5 only; not checked at extraction |

Volume of the data: no field. The reader starts at the header's volume index (the header's own number,
1 for data1.hdr/data1.cab) and moves to the next volume while the file's index > that volume's
`last_file_index` (the loop "Trying next volume..."). Hence files are laid out in index order across volumes.

### 3.3 v6+ file descriptor (everything else), array element i

Located at FT + file_table_offset2 + i * 0x57 (fixed size 0x57 = 87 bytes, no per-record size field;
note file_table_offset2 is relative to FT, not CD).

| Off | Size | Name | Meaning |
|---:|---:|------|---------|
| 0x00 | 2 | flags | section 3.4 |
| 0x02 | 8 | expanded_size | u64 |
| 0x0A | 8 | compressed_size | u64 |
| 0x12 | 8 | data_offset | u64, absolute in the data volume |
| 0x1A | 16 | md5 | of the expanded data |
| 0x2A | 16 | unknown | skipped |
| 0x3A | 4 | name_offset | rel. FT |
| 0x3E | 2 | directory_index | asserted `< directory_count` |
| 0x40 | 12 | unknown | skipped (0xC) |
| 0x4C | 4 | link_previous | file index (see below) |
| 0x50 | 4 | link_next | file index |
| 0x54 | 1 | link_flags | 0 none, 1 prev, 2 next, 3 both |
| 0x55 | 2 | volume | volume number (1-based) in which the file's data starts |

Links: if `link_flags & 1` (PREV), this file is another name for the data of file `link_previous` (the
reference extracts that file instead and uses its data, size, MD5). `link_next` is parsed and ignored
(unsure of any further meaning, e.g. chained duplicates).

Reference checks on a descriptor: if the COMPRESSED flag is clear but compressed_size != expanded_size it
only warns.

### 3.4 Flags (u16)

| Bit value | Name | Meaning |
|---:|---|---|
| 0x0001 | SPLIT | data continues in the next volume(s) (v5: recomputed, see 1.5) |
| 0x0002 | OBFUSCATED | data bytes are obfuscated (4.3); applies to the whole stored stream of the file, including chunk length prefixes |
| 0x0004 | COMPRESSED | data is chunked deflate (4.1) |
| 0x0008 | INVALID | entry is not a real file; skip |

Other bits are unused by the reference (unsure what they mean). A file is "valid" (extractable) when
INVALID is clear and name_offset != 0 and data_offset != 0. Such entries are skipped (not errors).

32/64-bit summary: v5: expanded, compressed, data_offset all u32. v6+: all three u64; volume u16; the volume
header's first/last offsets and sizes are 64-bit on disk but only low 32 bits are used (1.4).

## 4. File data

Pointers: `unshield_file_save`, `unshield_file_save_old`, `unshield_file_save_raw`, `unshield_uncompress`,
`unshield_uncompress_old`, `unshield_deobfuscate`, `unshield_reader_read`, `unshield_reader_open_volume`
(file.c).

The stored stream of a file is `compressed_size` bytes (COMPRESSED) or `expanded_size` bytes (otherwise),
starting at `data_offset` in the file's starting volume. All integers in the stream (chunk lengths) are
little-endian after deobfuscation.

### 4.1 Compressed files ("new" framing, default)

The stored stream is a sequence of chunks:

| Size | Meaning |
|---:|---------|
| 2 | n: byte length of the deflate data that follows (u16, so up to 65535). 0 is an error. |
| n | a complete raw DEFLATE stream (RFC 1951, no zlib header, no Adler-32; zlib window bits -15) |

* Each chunk is inflated independently: a fresh inflater (no preset dictionary, no window carried from the
  previous chunk) and run to the end of stream (the stream must end with its final block; reference demands
  `Z_STREAM_END`). So chunks can be decoded in isolation. (A chunk that is not self-contained is an error in
  the reference: "Decompression failed ... HINT: Try -O".)
* The reference appends one extra zero byte after the n bytes of a chunk before inflating "to make inflate
  happy". That byte is not stored in the file and is not part of the chunk length. It is a workaround for
  deflate decoders needing look-ahead (unsure if any chunk really depends on it).
* Output size per chunk: the reference has a 64 KiB output buffer and needs the entire chunk to inflate
  within it (otherwise zlib's no-progress/buffer error). The usual maximum is 0x8000 bytes of expanded data
  per chunk (unsure: not stated in the source; only the 64 KiB ceiling is).
* The loop's accounting: remaining = compressed_size; each chunk subtracts 2 + n. The file ends when
  the remaining count reaches 0; the sum of chunk outputs must equal expanded_size ("Expanded size expected
  to be ..." otherwise). A chunk length overrunning the remaining compressed_size is not diagnosed in
  particular; the unsigned counter would wrap (unsure).
* No marker bytes are searched or stripped in this framing. (Sync-flush trailer bytes `00 00 FF FF`
  appear only in the old framing, 4.4; if a new-framing chunk has them at its end, they are just part of the n
  bytes, after the final block, ignored by inflate once the stream end is reached.) (unsure whether real
  new-framing chunks contain them)
* Validation after the file: output length == expanded_size; for major_version >= 6, MD5 of the complete
  expanded data equals the descriptor's MD5 (16 bytes, raw digest). Not done for v5.

### 4.2 Uncompressed files

The stored stream is `expanded_size` bytes, copied verbatim (after deobfuscation if flagged). No chunking.
`compressed_size` should equal `expanded_size` (warning otherwise).

### 4.3 Obfuscation (flag 0x0002)

Applied to the *stored* stream before chunk parsing. Per byte, with a counter `k` that is the byte's position
within the file's stored stream (starting at 0, counting every stored byte including the 2-byte chunk length
prefixes; the counter keeps counting across read calls and across volumes, never reset):

1. XOR the stored byte with 0xD5.
2. Rotate the 8-bit result right by 2 bits.
3. Subtract (k mod 0x47) (that is, k mod 71) with 8-bit wraparound.

The result is the plain stored byte. There is no key table; 0x47 is `OFFSET_COUNT`. Note the reference
deobfuscates per read call (length prefix first, then chunk data) but the counter is persistent so it equals
one continuous pass. Order relative to decompression: deobfuscate first, then inflate. The counter is a
(wrapping) `unsigned`, so beyond 2^32 bytes it would wrap (irrelevant).

Pitfall: for split files the counter continues into the next volume's part; it is not restarted at each
volume.

### 4.4 Split files

* The reader works on a "volume cursor": the current volume file, and `volume_bytes_left` = bytes of this
  file's stream remaining in that volume. When a read needs more than that, the reader reads what is left and
  opens volume+1 (the next consecutive .cab, re-checking the common header; reading its volume header), and
  continues with the stream's next bytes. A chunk (length prefix or data) can straddle a volume boundary; the
  stream is simply concatenated before parsing.
* Where the part lives in a volume (when the file is flagged SPLIT): if file index == the volume's
  `last_file_index` and `last_file_offset != 0x7FFFFFFF`: offset = last_file_offset, bytes =
  last_file_size_compressed (compressed files) or last_file_size_expanded (uncompressed). Otherwise if index
  == `first_file_index`: offset = first_file_offset, bytes = first_file_size_*. Otherwise (index is neither):
  the reference does nothing for that volume and reports success without seeking (unsure; a file spanning
  three volumes must have its middle part described somehow; the source does not make this clear; the first/
  last pair suggests a middle volume with a single huge file has first==last==that index with the "last"
  branch winning, which would be wrong for a middle part; flagged as an open point). If the file is not
  SPLIT: offset = descriptor data_offset, bytes = descriptor sizes.
* v5 quirk: `last_file_offset == 0` in the volume header is read as 0x7FFFFFFF (i.e. "no partial last
  file").
* The first part of a split file is therefore in volume `file.volume`, as the volume's *last* file.

### 4.5 "Old" framing (`-O`, `unshield_file_save_old`)

Used for cabinets where the new framing fails (unshield suggests it in its error hints; the source ties it to
no specific version number, so which InstallShield versions need it is unknown to me: (unsure)). Differences:

* No 2-byte length prefixes. The whole stored stream is cut into chunks at the 4-byte marker
  `00 00 FF FF` (a deflate sync flush). Each chunk is inflated as raw deflate (window bits -15) with
  block-by-block inflation until the input is down to its last byte; it has no final-block requirement.
  Dictionary is not carried between chunks (fresh inflater each).
* Marker detection has a workaround: if the byte right after a found marker has bit 0 set (a real chunk
  starts with a block header whose first bit, BFINAL, would be 0 in this producer), the marker is taken as
  data inside a chunk and the search continues.
* The marker at the very end may be missing for small files whose compressed and expanded sizes are close;
  an external-file reader appends the missing tail of the marker (unshield treats it for files stored outside
  the cabinet, `unshield_reader_create_external`: the file is read from the extracted directory tree on disk).
  (unsure for what product this is relevant.)
* Reads the entire remaining volume portion into memory at once, then splits. Output limited to 64 KiB per
  chunk.
* No MD5 check; same expanded-size check.
* Obfuscation is the same algorithm and applies to the whole stream as read.
* External files: if `data_offset` equals the size of the data volume (file starts at its end), the file is
  not in the cabinet (it is stored uncompressed next to it as `<dir>/<directory name>/<file name>`) and the
  "new" mode just errors "not inside the cabinet".

## 5. Versions before 5

The reference does not implement any pre-5 specific layout. README: "Support InstallShield versions 5 and
later". Facts from code:

* For version top bytes other than 1, 2, 4 the major version stays 0 and the v5 layout is used.
* Major versions 1..4 select the v6+ layout (by fall-through), which is probably not right for real old
  files (unsure).
* There is no PKWARE DCL / blast / "Implode" support anywhere in unshield's lib. The only compression is
  DEFLATE through zlib, in the two framings above. Old InstallShield 3 `.Z`/`.LIB` archives use other, different
  formats and are not covered (not part of `ISc(` cabinets in this source).
* The version-dependent behaviours that exist: descriptor layout (0/5 vs rest), volume header size (40 vs
  64), file-group descriptor skip (0x48 for <= 5), component descriptor skip (0x6C for 0/5), MD5 only for >= 6,
  UTF-16 strings for >= 17, v5-only split detection and start-volume search (`major_version <= 5`).

## 6. Things easy to get wrong, and validation done by the reference

* Offset bases differ: file table offsets (directory names, file names, v5 descriptor offsets) are relative
  to FT = CD + file_table_offset. File-group and component offsets, and string offsets within group/
  component descriptors, are relative to CD. `file_table_offset2` is relative to FT. `data_offset` and the
  volume header's offsets are absolute in the *data volume* file. `cab_descriptor_offset` is absolute in the
  header file.
* v6+ descriptors are an array (stride 0x57) at FT + file_table_offset2; the u32 table entries after the
  directory entries are not used for them. v5 uses the table entries and descriptors have stride-less
  arbitrary positions.
* Directory index in descriptors is u16 in both layouts.
* The file's volume is a field only for v6+; for v5 it is found by scanning with `last_file_index`.
* Sizes (and in v5, offsets) are u32 for v5; u64 for v6+ descriptors but volume-header partials are
  effectively 32-bit in the reference.
* Obfuscation covers the length prefixes; the counter runs through the whole stored stream, across volumes.
  The modulus is 71 (0x47). Subtraction wraps at 8 bits; rotate is on an 8-bit value (the XOR is applied
  first).
* The compressed-chunk length prefix is 2 bytes and excludes itself. The sum over chunks of (2 + n) equals
  `compressed_size` exactly (the reference loops on that count).
* The chunk is raw deflate (no zlib wrapper). The appended zero byte is not stored.
* UTF-16 names for major_version >= 17 (two-byte NUL terminator; name offsets still byte offsets).
* Link flags: a file with PREV link has its own (possibly meaningless) offsets/sizes; resolve the link first.
* Empty or INVALID entries (flag 8, name_offset 0, data_offset 0) must be skipped, not treated as errors.
* The reference's `volume` for v6+ is 1-based, matching `dataN.cab` numbering.
* A volume count is not stored in a place the reference reads; stop at the first missing dataN.cab (unsure).
* Validation actually done: signature; header >= 4 bytes; non-zero descriptor size; file table bounds and
  non-empty; file_table_size == file_table_size2 (warning); `directory_index < directory_count` (assert);
  uncompressed size equality (warning); common header of every opened data volume; for compressed files
  chunk length non-zero and inflate success per chunk; total output == expanded_size; MD5 for >= 6; "file not
  inside cabinet" when data_offset == size of the volume file. Not validated: bounds of descriptor/string
  offsets within the header (the reference trusts them), volume_info, unknown fields, sums of the
  file-table sizes.
* Verifying against real files: expect `file_table_size == file_table_size2`; for v6+ every non-link,
  valid file should MD5-match after inflating; for compressed files `sum(2+n) == compressed_size` and total
  inflated bytes == expanded_size, a good structural check even for v5 which has no checksum usage here.

## 7. Checked against real files (written by the implementer, not from unshield)

Six `data1.cab` / `data1.hdr` pairs of Windows' `InstallShield Installation Information`
(versions `0x020004b0` twice, `0x04000834` twice, `0x04000b54`, `0x0400076c`: major numbers 12, 21,
29 and 19 by 1.2):

* Every `data1.cab` is, from the volume header's first u32 (`0x200`) to its last byte, a sequence of
  chunks of section 4.1 and nothing else (8883 chunks in all, no stored gap, no flag 2 file): each
  is a raw DEFLATE stream that ends exactly at its length (`eof` with no bytes left), with no
  byte appended.
* A chunk holds 32768 bytes of data; the last chunk of a file holds what is left. All of them
  decode alone (no window carried over).
* The v6+ descriptors (0x57 bytes, array at FT + `file_table_offset2`) read as 3.3 says: for the
  files in the volume, flags, sizes, `data_offset` and MD5 agree with the chunks (all 79 compressed files
  of one cabinet matched their MD5, and those of another that are in its volume). Entries with flag 8, and the first few entries of
  `data1.hdr` (all fields 0), are skipped as 3.4 says. The descriptor's `volume` is 2 for the
  files that are in `data2.cab`, which is not kept in the installation cache: a reader that
  ignores the field reads another file's bytes.
* In the common header of a data volume `cab_descriptor_size` is 0 and the 4 bytes after it are the
  volume header's `data_offset` (u32 at offset 20, the low half of its u64).
* aqr-flate's zlib emulation rebuilds 8881 of the 8883 chunks bit for bit (the profile adaptive,
  zlib emulation tried).
