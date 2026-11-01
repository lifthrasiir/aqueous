# RAR5 decoding and re-encoding notes

A clean-room description of the RAR 5.0 archive format ("Rar!" 1A 07 01 00) and its
compression (LZ + Huffman with filters), complete enough to decode bit-exactly and to write a
re-encoder that records the parse (as `codecs/aqr-lzo` does) and reproduces the original
bytes. RAR 1.5–4.x is out of scope.

Sources, read on the "dirty" side only:

- libarchive's RAR5 reader, `libarchive/archive_read_support_format_rar5.c` (BSD-2), at
  commit `348e5a2e27503f5bf114e298e2bd9ddcd59ab48e` (2026-10-29) of `third_party/libarchive`,
  and its test archives `libarchive/test/test_read_format_rar5*.uu` as data.
- RARLAB, "RAR 5.0 archive format", <https://www.rarlab.com/technote.htm> (fetched
  2026-11-01), called "the technote" below. The implementer may read it.
- Black-box observation of RAR 7.23 and 6.24 for macOS arm64 (`third_party/rar-dl`) and of
  bsdtar 3.7.4 (libarchive 3.7.4, the system one) on archives generated for this document.
  Every "observed" statement below comes from decoding their output with a scratch decoder
  written from this description; it decodes all fixtures listed in "Test vectors" with
  matching CRC32/BLAKE2sp.

Conventions: bytes are numbered from 0; "LE" is little-endian; bit strings of the
compressed stream are read most significant bit first (see 2.1). `n mod 2^k` is the
non-negative remainder. Sizes "KiB/MiB/GiB" are powers of two.

## 1. Archive structure

The technote describes the archive layout, the general header format, the vint, and every
header and extra record. This section only adds what it leaves out, where libarchive and real
archives differ from it, and what a decoder needs to find and reassemble the data.

### 1.1 Signature and SFX

The 8-byte signature `52 61 72 21 1A 07 01 00` is followed by the headers. An archive may be
preceded by an SFX module; the technote says to search for the signature up to the maximum
SFX size (1 MiB). libarchive accepts a signature at offset 0, or, if the file starts with
`MZ` or `\x7FELF`, searches for it on 16-byte aligned offsets within the first 512 KiB
(`test_read_format_rar5_sfx.exe`). RAR 4.x's 7-byte signature (`... 1A 07 00`) is a different
format.

### 1.2 Headers

Each header: CRC32 (LE u32) of the bytes from the size field through the end of the header;
header size (vint, counting from the type field to the end of the extra area); type; flags;
[extra area size]; [data area size]; type-specific fields; extra area. The data area (if the
"data present" flag 0x0002 is set) follows the header immediately and is not covered by the
header CRC or size.

Additions to the technote:

- vints may be longer than necessary. RAR writes the locator record of the main header with
  zero groups at the most significant end (`a2 88 86 80 00`, i.e. a 5-byte vint whose last
  two bytes carry no value), so it can patch the offsets later. A re-encoder should keep
  headers as raw bytes rather than regenerate them.
- libarchive bounds the header size at 2 MiB (the technote's "3 bytes" rule) and rejects
  headers smaller than 3 bytes.
- Header types: 1 main, 2 file, 3 service, 4 archive encryption, 5 end of archive. An unknown
  type is skipped if its flag 0x0004 is set, an error otherwise. libarchive treats type 0 as
  end of input.
- Flag 0x0004 (skip if unknown) is also set by RAR on the service headers it writes (QO:
  flags 0x0006; RR: 0x0007) and on the end header (0x0004).
- After the end-of-archive header RAR reads nothing more. Non-last volumes written by
  RAR 7.23 with `-v200k` end with 8 zero bytes after the end header, so the volume is exactly
  the requested size; whatever follows the end header must be preserved as opaque bytes.

Main header: archive flags 0x0001 volume, 0x0002 volume number present (every volume but the
first), 0x0004 solid, 0x0008 recovery record present, 0x0010 locked. Its extra area may hold
a locator record (type 1: flags, then the quick-open offset and/or the recovery offset as
vints) and a metadata record (type 2). libarchive reads the locator and ignores it; it fails
on any other main-header record type.

### 1.3 File and service headers

Fields in order (technote "File header and service header"): file flags, unpacked size,
attributes, [mtime u32 if file flag 0x0002], [data CRC32 u32 if file flag 0x0004],
compression information, host OS, name length, name, then the extra area.

File flags: 0x0001 directory, 0x0002 mtime present, 0x0004 CRC32 present, 0x0008 unpacked
size unknown (decode until the last block; libarchive rejects such entries).

Compression information (vint), as a bit table. The technote numbers bits from 1; this table
gives the shift from bit 0:

| bits (from 0) | mask | meaning |
|---|---|---|
| 0–5 | 0x00003F | algorithm version: 0 = RAR 5.0 algorithm, 1 = RAR 7.0 algorithm |
| 6 | 0x000040 | solid: continue the state left by the previous file (never on service headers) |
| 7–9 | 0x000380 | method 0–5; 0 = stored, 1–5 = compressed (all compressed methods decode alike) |
| 10–14 | 0x007C00 | dictionary exponent N: base size = 128 KiB << N |
| 15–19 | 0x0F8000 | version 1 only: fraction F; dictionary = base + base / 32 * F |
| 20 | 0x100000 | version 1 only: dictionary bits use the version-1 layout but the data uses the version-0 algorithm |

For version 0, N is 0–15 (128 KiB to 4 GiB) and bits 15–20 are zero. Version 1 is written
only when the dictionary exceeds 4 GiB: observed `ci = 0x43D81` for 5 GiB (version 1,
method 3, N = 15, F = 8), `0x83D81` for 6 GiB (F = 16), `0x4D81` for 64 GiB (N = 19, F = 0).
With `-md4g` the archive stays version 0 (N = 15). Section 2.10 gives what version 1 changes.

libarchive reads only bits 10–13 as N (so 64 GiB reads as 1 MiB), ignores the version
field, rejects dictionaries above 64 MiB, and requires a nonzero dictionary for
non-directories. Hence it rejects all version 1 archives with N = 15, and silently decodes
N ≥ 16 archives with the version-0 algorithm (wrong output; it then fails on a bad distance
or the CRC).

Directories have file flag 0x0001, compression information 0 and no data. Entries with a
redirection record (symlink, hard link, …) have an empty data area; their unpacked size is
not the size of any data (observed: 0 for a symlink by RAR 7.23, the target name's length in
libarchive's older fixture, the target's size for a hard link).

Extra records of file/service headers: 1 encryption, 2 hash, 3 time, 4 version, 5
redirection, 6 Unix owner, 7 service data (technote). libarchive skips encryption (and then
refuses to read the data) and service data. RAR writes a time record (type 3) by default,
flags 0x13 (Unix time with nanoseconds, mtime only); with `-tsc -tsa` flags 0x1F.

Service headers (type 3) carry named data: `CMT` (archive comment, before the files,
stored), `QO` (quick open, after the files, stored), `RR` (recovery record, last before the
end header), `ACL`/`STM` (NTFS, Windows only, not observed here). libarchive skips every
service header's data without decoding it.

### 1.4 Data area, volumes and checks

The data area of a file header is the file's packed data: for method 0 the file bytes
themselves, otherwise a compressed stream (section 2).

Multi-volume archives (`name.partN.rar`): a file may be split across volumes. Each volume
repeats the file header; the part in the next volume has header flag 0x0008 ("continues from
the previous volume"), the part before it 0x0010 ("continues in the next volume"). The
packed stream is the concatenation of the parts' data areas in volume order; the split falls
on any byte, including inside a compressed block (libarchive merges such a block). Service
headers (QO) may sit between a split file's header and the end of its volume. Header fields
other than the data size and the hashes repeat the first part's values.

Checks: the data CRC32 (file flag 0x0004) or the hash record (type 2, hash type 0 =
BLAKE2sp, 32 bytes) of the last part covers the unpacked data; for every part except the
last it covers that part's packed bytes (verified for both CRC32 with `-v` and BLAKE2sp with
`-htb -v`). With `-htb` the CRC32 field is absent. BLAKE2sp is the standard parallel
BLAKE2s (8 leaves, 64-byte rounds interleaved, 32-byte digest). CRC32 is the zlib/PKZIP CRC.
With encryption and the "tweaked checksums" flag the stored checksums are key-dependent
(technote); not covered here.

The end header's flag 0x0001 means "not the last volume". libarchive moves to the next
volume only when the volumes are concatenated into one input stream
(`archive_read_open_filenames`, or `cat *.part*.rar | bsdtar -xf -`); given only the first
volume, bsdtar 3.7.4 writes the file zero-filled past the end of the first part and exits 0.

### 1.5 What libarchive does not support

Encrypted data and encrypted headers (types 1 extra / 4 header), unknown unpacked size,
dictionaries above 64 MiB, algorithm version 1, decoding of service data (comments, NTFS
streams; it never decompresses them), the quick-open and recovery records (ignored),
seeking. Its multivolume support needs a concatenated stream. Everything below is therefore
partly confirmed by libarchive and partly only by black-box observation of RAR's output;
section 6 lists what remains unverified.

## 2. The compressed stream

### 2.1 Bit reader

Within the data area the stream is a sequence of bytes read as a big-endian bit string: bit
7 of byte 0 first, then bit 6, …, bit 0 of byte 0, bit 7 of byte 1, and so on. "Read n bits"
yields an unsigned n-bit number whose first bit read is the most significant. n can reach 34
(section 2.6, version 1).

### 2.2 Blocks

A file's packed stream is a sequence of blocks. Each block is byte-aligned:

| offset | size | content |
|---|---|---|
| 0 | 1 | flags |
| 1 | 1 | check byte |
| 2 | k | block size S, LE, k = 1, 2 or 3 bytes |
| 2 + k | S | block body (bit stream) |

Flags byte:

| bits | meaning |
|---|---|
| 0–2 | B − 1, where B (1–8) is the number of bits used in the last byte of the body |
| 3–5 | k − 1 (0, 1 or 2; libarchive rejects larger values) |
| 6 | last block of this file's stream |
| 7 | tables present at the start of the body |

Check byte = `0x5A ⊕ flags ⊕ (S & 0xFF) ⊕ ((S >> 8) & 0xFF) ⊕ ((S >> 16) & 0xFF)`; it must
match. Example (`methods/723-m3-t1.rar`, first block of wer.dll): `88 C7 4C 59`: flags 0x88
(tables present, not last, k = 2, B = 1), check C7, S = 0x594C = 22860.

The body holds exactly `8·(S − 1) + B` meaningful bits; the remaining `8 − B` low bits of
the last byte are padding (RAR writes zeros: 2008 of 2008 blocks checked). Bit reading
restarts at the first bit of each body. If the tables flag is set, the body begins with the
tables (2.3), and symbols follow immediately with no alignment. Then symbols are decoded
(2.4) while the bit position is below the end; the last symbol ends exactly at the end in all
of RAR's output (the decoder may treat any overrun as an error). A symbol never spans two
blocks.

The block with the last flag ends the file's stream; the stream must have produced exactly
the unpacked size by then. RAR writes no bytes after it (data area size = sum of the
blocks). libarchive does not check the last flag, only the unpacked size, and errors if the
output would exceed the declared size.

If a block has no tables, the tables of the previous block are used. "Previous" crosses file
boundaries in a solid stream (RAR does this: in `misc/manysmall-solid-723.rar` 280 of the
300 files' only block has no tables). The first block of a non-solid file must have tables.

### 2.3 Tables

The tables of a block define four prefix codes:

| alphabet | size | use |
|---|---|---|
| main (literals and commands) | 306 | 0–255 literal, 256 filter, 257 repeat last copy, 258–261 repeat distance 0–3, 262–305 explicit copy with length slot 0–43 |
| distance slot | 64 (version 0) or 80 (version 1) | distance slot of an explicit copy |
| low distance | 16 | the low 4 bits of distances with ≥ 4 extra bits |
| repeat length | 44 | length slot of a repeat-distance copy |

They are transmitted as one sequence of code lengths (0–15) in that order: 306 + 64 + 16 + 44
= 430 lengths (446 for version 1). The sequence is itself coded with a 20-symbol
"length code", whose own 20 code lengths come first.

**Length-code lengths.** Read 4-bit values until 20 lengths are known:

- 0–14: the next length is that value.
- 15: read another 4 bits, z. If z = 0 the next length is 15; otherwise the next `z + 2`
  lengths are 0 (3 to 17 zeros; truncated if fewer than that remain to fill 20).

**Main code lengths.** With the length code built from those 20 lengths, decode symbols
until all 430 (446) lengths are filled:

| symbol | meaning |
|---|---|
| 0–15 | the next length is the symbol |
| 16 | read 3 bits r: repeat the previous length `3 + r` times (3–10) |
| 17 | read 7 bits r: repeat the previous length `11 + r` times (11–138) |
| 18 | read 3 bits r: `3 + r` zeros |
| 19 | read 7 bits r: `11 + r` zeros |

A repeat or zero run is truncated at the end of the sequence. Symbols 16/17 as the very
first entry are an error (no previous length). Runs may cross from one alphabet into the
next; the sequence is only split into the four alphabets after it is complete.

**Canonical codes.** For each alphabet, codes are assigned canonically: symbols with nonzero
length are ordered by (length, symbol index); the first gets the all-zero code of its length;
each next code is the previous code plus one, shifted left by the difference in length when
the length grows. Codes are read bit by bit in stream order (most significant code bit
first). The maximum length is 15. libarchive rejects over-subscribed length sets (Kraft sum
above 1); incomplete sets are accepted, decoding an unassigned bit pattern being an error. An
alphabet may have no codes at all, which is fine as long as it is never used. RAR's codes are
always complete (see section 3.5 for one-symbol alphabets).

### 2.4 Symbols

Decode a symbol s with the main code:

- **s < 256**: output the byte s.
- **s = 256**: a filter record follows (2.7). No output.
- **s = 257**: repeat the last copy: if the last copy length L is nonzero, copy L bytes from
  distance D0 (the first entry of the distance cache). Neither the cache nor L changes. If L
  is 0 (no copy yet), nothing happens (libarchive).
- **258 ≤ s ≤ 261**: repeat distance i = s − 258. Take D = cache[i]; move it to the front
  (entries 0..i−1 shift down by one, cache[0] = D). Decode a length slot with the
  *repeat length* code and its extra bits (2.5) to get the length n. No length adjustment.
  Set L = n and copy n bytes from distance D.
- **s ≥ 262**: explicit copy. Length slot = s − 262; read its extra bits now, giving n
  (2.5). Then decode the distance slot with the *distance slot* code and its extra bits,
  giving D (2.6). Adjust: if D > 0x100 add 1 to n; if D > 0x2000 add 1 more; if D > 0x40000
  add 1 more (so +0 to +3). Push D at the front of the cache (dropping cache[3]), set L = n
  (adjusted), copy n bytes from distance D.

"Copy n bytes from distance D" appends, one byte at a time, the byte D positions before the
current end of the output (so overlapping copies replicate). D counts within the whole solid
stream, not only the current file.

### 2.5 Lengths

A length slot c (0–43, same rule for explicit and repeat copies):

- c < 8: length `2 + c`, no extra bits.
- c ≥ 8: e = c / 4 − 1 (integer division), base = `2 + ((4 + (c mod 4)) << e)`; read e bits x;
  length = base + x.

| slots | extra bits | lengths |
|---|---|---|
| 0–7 | 0 | 2, 3, …, 9 |
| 8–11 | 1 | 10–11, 12–13, 14–15, 16–17 |
| 12–15 | 2 | 18–21, 22–25, 26–29, 30–33 |
| 16–19 | 3 | 34–41, 42–49, 50–57, 58–65 |
| 20–23 | 4 | 66–81, 82–97, 98–113, 114–129 |
| 24–27 | 5 | 130–161, 162–193, 194–225, 226–257 |
| 28–31 | 6 | 258–321, 322–385, 386–449, 450–513 |
| 32–35 | 7 | 514–641, 642–769, 770–897, 898–1025 |
| 36–39 | 8 | 1026–1281, 1282–1537, 1538–1793, 1794–2049 |
| 40–43 | 9 | 2050–2561, 2562–3073, 3074–3585, 3586–4097 |

The longest copy is 4097 bytes (4100 for an explicit copy at a distance above 0x40000).

### 2.6 Distances

A distance slot c (0–63, or 0–79 in version 1):

- c < 4: distance `1 + c`, no extra bits.
- c ≥ 4: e = c / 2 − 1, base = `1 + ((2 + (c mod 2)) << e)`; distance = base + x where x
  (e bits) is read as follows:
  - e < 4: x is e raw bits.
  - e ≥ 4: first read `e − 4` raw bits h (none when e = 4), then decode one symbol l (0–15)
    with the *low distance* code; x = `h · 16 + l`.

| slot c | e | distances |
|---|---|---|
| 0, 1, 2, 3 | 0 | 1, 2, 3, 4 |
| 4, 5 | 1 | 5–6, 7–8 |
| 6, 7 | 2 | 9–12, 13–16 |
| 8, 9 | 3 | 17–24, 25–32 |
| 10, 11 | 4 | 33–48, 49–64 |
| 2j, 2j+1 (j ≥ 2) | j − 1 | `2^j + 1` – `3·2^(j−1)`, `3·2^(j−1) + 1` – `2^(j+1)` |
| 62, 63 | 30 | up to 4 GiB |
| 78, 79 (version 1) | 38 | up to 1 TiB |

Version 1's top slot needs a 34-bit raw read (e − 4 = 34).

A distance larger than the number of bytes produced so far in the (solid) stream, or larger
than the dictionary size, is invalid. A decoder whose window holds at least the dictionary
size of history can serve every valid distance (a circular window of exactly the dictionary
size works too, as a distance equal to the size reads the byte about to be overwritten).
Observed maximum distances stay a few KiB below the dictionary size (126687 for 128 KiB,
1043185 for 1 MiB).

### 2.7 Filter records

After symbol 256:

1. Block start: read 2 bits m; then `m + 1` bytes, each as 8 bits, least significant byte
   first. This is the filter's start relative to the current output position (the number of
   bytes of this file decoded so far).
2. Block length: same encoding.
3. Filter type: 3 bits.
4. Only for type 0 (delta): channels − 1 in 5 bits (1–32 channels).

Types: 0 delta, 1 x86 E8, 2 x86 E8/E9, 3 ARM. Types 4–7 are invalid (RAR 4.x had others;
RAR5 has none).

Constraints (libarchive's checks; RAR's output always satisfies them): length ≥ 4; length ≤
4 MiB (0x400000) and ≤ half the dictionary; the filter's start (absolute) is not before the
end of the previously declared filter, so filters are declared in increasing order of start
and never overlap. Start offsets are relative to the current output position, so a filter
cannot start before the data already decoded; it may start far ahead (RAR declares filters
up to ~100 KiB before their start). Up to 8192 may be pending in libarchive.

Filter positions are positions in the current file's output, counted from the file's first
byte (also in a solid stream, where the file's first byte is not stream position 0). A
filter should lie within its file.

### 2.8 Filter transforms

Let `src[0..len)` be the decoded (unfiltered) bytes of the filter's region and `start` its
position within the file. The filter produces `dst[0..len)`, which replaces the region in the
file's output. **The window is not changed:** later copies read the unfiltered bytes.

**Delta (type 0), C channels.** The region was stored channel by channel; each channel's
bytes are differences. Read src sequentially:

```
k = 0
for ch in 0 .. C-1:
    acc = 0
    for p = ch, ch+C, ch+2C, ... while p < len:
        acc = (acc - src[k]) mod 256
        dst[p] = acc
        k += 1
```

(The stored byte is the negated difference: `src = (previous − current) mod 256`.)

**x86 (type 1: E8 only; type 2: E8 and E9).** dst starts as a copy of src. Scan:

```
i = 0
while i < len - 4:
    b = src[i]; i += 1
    if b == 0xE8 or (type == 2 and b == 0xE9):
        pos = (start + i) mod 2^24          # file position of the 4 address bytes
        a = LE32(src[i .. i+4])
        if a >= 2^31:                        # negative as a signed 32-bit number
            if (a + pos) mod 2^32 < 2^31:
                write LE32((a + 2^24) mod 2^32) at dst[i]
        else if a < 2^24:
            write LE32((a - pos) mod 2^32) at dst[i]
        i += 4
```

So the opcode byte is looked at for indices 0 … len−5, and after a match the 4 address bytes
are skipped whether or not they were rewritten.

**ARM (type 3).** dst starts as a copy of src. For i = 0, 4, 8, … while i ≤ len − 4: if
`src[i+3] == 0xEB` (BL), let v = the 24-bit LE number in src[i..i+3]; write
`(v − (start + i) / 4) mod 2^24` (integer division) back into dst[i..i+3]; byte i+3 stays
0xEB.

### 2.9 Output order and flushing

The file's output is its decoded byte sequence with each filter region replaced by the
filter's result. A whole-file decoder can apply the filters after decoding. A streaming
decoder (libarchive's behaviour) keeps the declared filters in a queue and repeatedly:

- writes out unfiltered bytes up to the start of the first pending filter (or up to the
  decoded end if none is pending);
- once the decoded end has reached the first pending filter's end and everything before its
  start has been written, runs that filter on the region (taken from the window) and writes
  its result, then removes it from the queue.

Because a filter is at most half the dictionary long and runs as soon as its data is there,
the region is still in the window. libarchive also stops decoding a block when more than half
the window is pending, and resumes later; any decoder must not let undelivered output be
overwritten in a circular window.

### 2.10 Dictionary and algorithm version

The dictionary size from the compression information is the window the decoder must keep.
For version 0 it is 128 KiB << N. For version 1 it is that plus `base / 32 · F`, so not
necessarily a power of two (5 GiB, 6 GiB, …); a decoder should not assume a power-of-two
window.

Version 1 (RAR 7.0) differs from version 0, as observed, only in the distance-slot alphabet:
80 symbols instead of 64 (the table sequence is 446 lengths long), the extra slots 64–79
continuing the rule of 2.6. Decoding version 1 archives (text, x86 code with E8/E8E9 filters,
delta filters) with this one change reproduces their CRCs; decoding them with 64 slots fails
at once. Distance slots ≥ 64 were not exercised (they need over 4 GiB of history). Per the
technote, compression information bit 20 in a version-1 header means the data uses the
version-0 algorithm (64 slots) with a version-1 dictionary field; RAR writes it when
appending version-1 files to a version-0 solid stream. Not observed.

RAR picks the dictionary field itself (section 3.1). In a solid stream all files share one
window; libarchive requires the same dictionary size for every solid file.

### 2.11 State and solid streams

Decoder state: the window (output history), the four code tables, the distance cache (4
entries), the last copy length L, the pending filter queue.

At the start of a file whose solid flag (compression information bit 6) is clear: empty the
history, cache = [0, 0, 0, 0], L = 0, no tables, no filters. (libarchive keys the reset on the
main header's solid flag and does not reset the cache or L for non-solid files; RAR's output
never depends on it.) The first file of a solid archive has the flag clear, later files set.

With the solid flag set, the history continues (copies may reach into previous files), and
so do the tables (blocks without tables, observed), the distance cache and L (kept by
libarchive; RAR was not observed to use a repeat distance or symbol 257 before the first
explicit copy of a file, so this is unconfirmed but harmless to keep). The filter queue is
per file and file-relative (filters of one file are complete when it ends). Output positions
used by filters restart at 0 for each file (verified: an E8E9 filter in the second file of
`misc/solid-723.rar`).

A stored file (method 0) does not feed the window in libarchive. RAR does not write stored
files inside solid streams (it compresses even incompressible data there: 300000 random
bytes became 300627), so whether stored data should enter the window is moot.

Directory and redirection entries carry no stream and do not affect the state. Service
headers' data is a separate stream; whether a compressed service stream (NTFS ACL/STM) shares
or interrupts the solid state is not known (none observed; libarchive never decodes them).

## 3. Encoder freedom

Everything below is accepted by the decoder but chosen by the compressor. A re-encoder must
record these choices, or predict them and record the exceptions. RAR's own choices, where
observed, are noted to help prediction.

**RAR's output depends on the version, the method and the number of threads.** For the same
input: 6.24 and 7.23 differ for methods 2–5 and for binary data at method 1 (identical only
for text at `-m1`); `-mt1` and `-mt8` differ at every method; reruns with identical settings
are identical. The default thread count is the number of CPU threads. Emulating RAR's parser
is therefore not practical; the parse has to be recorded.

### 3.1 Per-file choices (headers)

- Method field: RAR stores (method 0) a file in a non-solid archive when compression does not
  help (random data, a 24-byte file), keeping the archive's dictionary field in the
  compression information (`0x0C00`). In a solid archive it always compresses.
- Dictionary field: observed equal to the smallest power of two ≥ the largest file
  (non-solid) or the total size (solid), at least 128 KiB, at most the `-md` value (default
  32 MiB); not reduced for `-si` input. The same field is written for every file of an
  archive, including stored ones; directories and links have 0.
- Algorithm version: 1 only for dictionaries above 4 GiB.
- File order in solid archives is RAR's (by extension), not the command line's.
- All other header content (vint padding, extra records and their order, QO/RR/CMT contents,
  volume padding) is best kept verbatim.

### 3.2 The parse

The sequence of literals and copies (lengths, distances), i.e. RAR's match finding and
lazy evaluation, is entirely the encoder's. Observed traits of RAR 7.23 and 6.24:

- The parser works on independent segments: with `-mt1` they are 128 KiB of input; with
  several threads the boundaries move. At a segment start the parser does not know the
  distance cache, so it emits an explicit copy even when the distance is in the cache
  (with `-mt1`, 29 of the 30 such copies seen were at positions 131072·k; the other one,
  at `-m1`, 29 bytes after one).
- Copies are at most 4096 bytes long (a 1,000,000-byte run of zeros is one literal followed
  by copies of 4096 at distance 1, chained with symbol 257).

### 3.3 Equivalent encodings of a copy

Given a copy (n, D), the encoder can choose:

- an explicit copy (slot and extra bits are then unique for (n, D): the length is coded as
  n minus the distance adjustment, which must leave at least 2, so a copy at D > 0x40000
  is at least 5 bytes long, at D > 0x2000 at least 4, at D > 0x100 at least 3);
- a repeat distance i, if D = cache[i] (any n ≥ 2), which reorders the cache differently from
  an explicit copy (which pushes D and can create duplicate cache entries);
- symbol 257, if D = cache[0] and n = L (same effect as repeat distance 0 with length L).

RAR's rule, observed without exception: symbol 257 whenever it applies (repeat distance 0 is
never used with n = L); a repeat distance whenever D is in the cache, except at segment
starts. A re-encoder needs to record only the exceptions.

A copy can also be split into several, or overlap literals; that is part of the parse.

### 3.4 Filters

Whether to filter, the type (E8 vs E8E9 per region), start, length, delta channel count, and
where in the symbol stream the record is declared (anywhere between the previous filter's
declaration and the moment the output reaches the filter's start). The byte counts of the
start and length fields (1–4) are a choice too; RAR always uses the minimal count (1726 of
1726 records).

RAR 7.23's choices (`-mt1`): x86 filters on 64 KiB chunks at file offsets that are multiples
of 64 KiB, the type chosen per chunk, the last chunk cut where the code region ends; it also
detects E8 content in AArch64 Mach-O files (it never chose ARM in these tests; ARM appears in
libarchive's ELF ARMv7 fixture, made by an older RAR). Delta filters cover detected tables
(channels 2, 3, 4, 6, 8, 12, 16 seen; lengths multiples of 256 in executables), and with
`-mcND+` cover every 64 KiB chunk. All filters of a 128 KiB segment are declared at its first
symbol (relative starts 0 and 65536). `-mcE+` forces type 1, `-mc-` disables filters.

### 3.5 Blocks and tables

- Block boundaries. RAR closes a block after at most 16384 symbols (16383 for a file's first
  block), at segment ends, and at the end of each file. A filter record counts as 6 units plus
  its start field's byte count (determined later from the fixtures: aqr-rar's `FORMAT.md`
  §5.6), so a block with filters holds fewer tokens.
- Table transmission: in non-solid files every block observed has tables; in solid streams
  files often start with a block without tables, and RAR resends tables every few hundred
  symbols across small files.
- Last-block flag, check byte, B (used bits): determined by the content.
- Size field width k: RAR always uses the minimal one (1 byte below 256, 2 below 65536);
  verified on 747 blocks.
- Padding bits: zero.
- Code lengths: RAR's lengths are Huffman-optimal for the block's symbol counts (same total
  bit cost as an unrestricted Huffman code in all 745 tables checked; 15 is reached but the
  limit did not bind in these tests), with ties broken in a way a textbook heap
  construction reproduces only partly, and symbols not used in the block get length 0.
  Exception: an alphabet with exactly one used symbol gets that symbol at length 1 plus a
  dummy symbol at length 1 (symbol 0, or symbol 1 if the used symbol is 0). Unused alphabets
  get all zeros. The length code's 20 lengths follow the same rules. Record the lengths, or
  the differences from a predicted construction.
- Run-length coding of the 430/446 lengths. RAR's rule (verified on all 746 tables of the
  fixtures, both versions):
  - zero runs: a run of r ≥ 3 zeros is coded as zero-runs of up to 138 (18 for 3–10, 19 for
    11–138) as long as at least 3 remain; 1 or 2 remaining zeros are coded as literal 0;
  - a nonzero value v followed by r more copies of v: code v, then repeat-runs of up to 138
    (16 for 3–10, 17 for 11–138) while at least 3 copies remain; 1 or 2 remaining copies are
    coded as literal v (which then starts its own run).
  - Runs cross alphabet boundaries and are truncated by nothing but the sequence end.
- Length-code nibbles: a run of r ≥ 3 zeros is coded as 15 followed by `min(r, 17) − 2`,
  repeatedly; 1–2 zeros literally; length 15 as 15, 0. Verified on all tables.

### 3.6 What to record

Per stream: the token sequence (literal, explicit copy n/D, repeat i with n, 257, filter
record with its fields and byte counts) or a model-predicted equivalent plus exceptions;
block boundaries; per block the tables-present flag and the code lengths of the 4 alphabets
and of the length code; any deviation from the RLE rule above. Everything else (check byte,
k, B, last flag, padding) is derived. Per archive: headers verbatim, as in 3.1.

## 4. Test vectors

### 4.1 Fixtures made for this document

In `third_party/rar-dl/fixtures5/` (gitignored; `INDEX.txt` there lists inputs and
commands). Inputs are in `in/`. "bsdtar" = `bsdtar -xf` (3.7.4), volumes concatenated; every
extracted file was compared with its input.

| fixture | exercises | bsdtar |
|---|---|---|
| `methods/{723,624}-m{1..5}-t{1,8}.rar` | methods 1–5, both versions, thread dependence (wer.dll: E8/E8E9 + delta filters; text.txt) | ok |
| `filters/auto-*.rar` | automatic filters: delta (rgb.bin, 3 channels), E8/E8E9 (wer.dll, zsh.arm64), none (text) | ok |
| `filters/mcEplus-*`, `mcEminus-*`, `mcDplus-*`, `mc5Dplus-*`, `mcoff-*` | forced E8 everywhere, E8 off (delta only), delta, 5-channel delta everywhere, no filters | ok |
| `filters/exe-{723,624}.rar` | E8E9/E8 and delta with 2, 4, 12 channels, default threads (filters declared far ahead) | ok |
| `misc/mixed-{723,624}.rar` | stored fallback (small, random, empty), packed PE, delta | ok |
| `misc/solid-723.rar` | solid stream, E8E9 in a later file (file-relative offsets), table reuse | ok |
| `misc/manysmall-solid-{723,624}.rar` | 300 files in one solid stream, blocks without tables | ok |
| `misc/geo-codelen15-723.rar`, `geo-624.rar` | code length 15 | ok |
| `misc/dict128k-wrap.rar`, `dict1m-wrap.rar` | dictionary smaller than the file (window wrap), filters across the wrap | ok |
| `misc/longruns.rar` | 4096-byte copies, symbol 257 chains, explicit copies at segment starts | ok |
| `vol/v.part1..3.rar` | solid volumes, BLAKE2sp per part, a block split across volumes | ok |
| `vol/c.part1..3.rar` | volumes with CRC32 per part | ok |
| `v1/text-md5g-si.rar`, `text-md6g-si`, `text-md64g-si`, `wer-md5g-si`, `rgb-md5g-si` | algorithm version 1 (80 distance slots), fractional dictionary fields, filters in version 1 | fails ("Declared dictionary size is not supported"; md64g: wrong data, then "Failed to decode the distance slot"); `rar t -mdx64` ok |
| `v1/text-md4g-si-v0.rar` | version 0 with a 4 GiB field | fails (size); rar ok |
| `v1/text-md5g-reduced-v0.rar` | `-md5g` reduced to 512 KiB | ok |
| `headers/cmt.rar` | CMT service header, directory entry | ok |
| `headers/links.rar` | symlink and hard link redirection records | ok |
| `headers/rr.rar` | recovery record, main locator with both offsets | ok |
| `headers/times.rar` | time record with mtime, ctime, atime | ok |
| `headers/ver.rar` | file version record | ok (second copy named `b.txt;1`) |
| `headers/htc.rar`, `noqo.rar` | `-htc`, no quick-open | ok |
| `headers/sivol.rar` | `-si` input | ok |
| `headers/enc.rar` | data encryption (password PASS) | fails (unsupported) |
| `headers/henc.rar` | header encryption | fails ("Encryption is not supported") |

### 4.2 libarchive's test archives

In `third_party/libarchive/libarchive/test/` as uuencoded `.uu` files. Valid archives that
decode correctly with bsdtar 3.7.4 and with a decoder following this document:

| archive | exercises |
|---|---|
| `test_read_format_rar5_stored.rar`, `stored_manyfiles.rar` | method 0 |
| `compressed.rar` | one compressed file |
| `multiple_files.rar`, `multiple_files_solid.rar` | several files, non-solid and solid |
| `solid.rar`, `win32.rar` | solid stream; Windows host OS attributes |
| `blake2.rar` | BLAKE2sp hash record |
| `arm.rar` | ARM filter (and delta) on an ARMv7 ELF, method 5 |
| `multiarchive.part01..08.rar` | 8 volumes, files split across them |
| `multiarchive_solid.part01..04.rar` | solid volumes |
| `sfx.exe` | SFX prefix |
| `extra_field_version.rar`, `owner.rar`, `symlink.rar`, `hardlink.rar`, `unicode.rar`, `fileattr.rar` | extra records, redirections, UTF-8 names, attributes |
| `zip_in_rar.rar` | stored ZIP inside |

The others (`bad_tables`, `truncated_huff`, `distance_overflow`, `invalid_dict_reference`,
`leftshift1/2`, `readtables_overflow`, `decode_number_out_of_bounds_read`, `window_buf_and_size_desync`,
`different_window_size`, `different_solid_window_size`, `different_winsize_on_merge`,
`arm_filter_on_window_boundary`, `block_size_is_too_small`, `bytes_remaining_underflow`,
`loop_bug`, `nonempty_dir_stream`, `unpacked_size_exceeds_declared`, `*_toolong`, `redir_*`,
`extra_field_underflow`, `main_block_extra_bytes`, `skip_block_extra_bytes`) are fuzzer
cases a decoder must reject or survive without crashing. `encrypted*.rar` and
`solid_encrypted*.rar` are encrypted.

## 5. Summary of RAR 6.24 vs 7.23

Same format, same table coding (RLE rule, Huffman lengths, one-symbol rule), same block
limit, same dictionary choice and filter kinds; different parses (and so different streams)
for methods 2–5 and for binary data at method 1. Only 7.x writes version-1 streams.

## 6. Open points

- Distance slots 64–79 of version 1 were never exercised; their meaning is extrapolated from
  the slot rule, which the 80-symbol alphabet and the 1 TiB maximum fit.
- Compression information bit 20 (version-0 data with a version-1 dictionary field) is only
  known from the technote.
- Whether the distance cache and L persist across solid files (libarchive keeps them; RAR's
  output does not depend on it in these tests).
- Compressed service data (NTFS streams) and its relation to the solid state.
- Encryption (AES-256-CBC per the technote) is not described; libarchive has no support.
- RAR's tie-breaking in Huffman construction, its block-cutting rule beyond the 16384-symbol
  limit, and its table-reuse decisions in solid streams are not derived; record them.
