# aqr-pack

Archives of other formats than Aqueous written from the tree that `aqr -c` gathers, the
counterpart of aqr-unpack: `aqr -c --format FORMAT` (or `-o NAME.EXT`) makes a ZIP, 7z, ALZ,
EGG or tar archive (alone, or under gzip, bzip2, xz, LZMA or Zstandard), or one of those
compressors' streams of a single file, instead of an Aqueous one. A single input goes in by its
name (a directory with what it holds under it, as 7-Zip, `zip -r` and tar have it).

```rust
let w = aqr_pack::write(Format::Tar(Some(Filter::Xz)), &tree, &contents, &options, &mut out, &|progress| ...)?;
// w.read, w.len: bytes of the files and of the archive; w.notes: what the format did not keep
```

Every format spreads its work over the pool's threads, with the parts cut so that each thread
has one even for a small input: the files of a batch side by side (ZIP, ALZ, EGG), or a
stream's parts side by side (gzip members, bzip2 blocks, xz blocks, 7z's LZMA2 chunks,
Zstandard jobs). Only `.lzma` (a single LZMA stream, no parts) is one thread's.

| format | `-l` (default) | how | `-P` |
|---|---|---|---|
| ZIP | zlib's 0–9 (6) | each file as zlib 1.3.1 deflates it (aqr-flate's emulator; a file of 4 MiB on in chunks, below), stored where no smaller | WinZip's AES-256 (AE-2) |
| 7z | XZ Utils' presets 0–9 (6) | one solid folder of LZMA2 (liblzma's raw encoder) in parts of `total / threads` (1 MiB to 4 dictionaries), each from a dictionary reset; Copy at 0 | AES-256, the header too |
| ALZ, EGG | zlib's 0–9 (6) | as ZIP's; ALZ's names in CP949 where they all can be, EGG's in UTF-8 | — |
| tar | — | pax, kept to ustar where that holds an entry | — |
| gzip | zlib's 0–9 (6) | members of `total / threads` (1 to 16 MiB), each as zlib 1.3.1 deflates it | — |
| bzip2 | 1–9 (9) | aqr-bz2's libbzip2: the blocks side by side, the stream bzip2 writes bit for bit | — |
| xz | presets 0–9 (6) | liblzma's encoder of threads, blocks of `total / threads` (1 MiB to 3 dictionaries) | — |
| LZMA | presets 0–9 (6) | liblzma's `.lzma`, one thread | — |
| Zstandard | 1–22 (3) | libzstd's workers, jobs of `total / threads` (512 KiB on), its checksum | — |

liblzma is XZ Utils' (0BSD), built from its source by the `liblzma` crate (not the system's),
used as a black box.

A file of 4 MiB or more is deflated in chunks of 512 KiB on the pool (pigz's way), in ZIP, ALZ
and EGG (an EGG block of up to 1 GiB), where there are two threads or more: each chunk has the
32 KiB before it as its preset dictionary (aqr-flate's `zlib::compress_part`: the emulator
started at the chunk, so the chunk is not zlib's own output) and ends on a byte boundary with an
empty stored block (`Z_SYNC_FLUSH`), the last one final, so that the chunks in order are one
DEFLATE stream; the CRC-32s of the chunks are made with them and combined. Smaller files, and
any with `-T 1`, are deflated whole, byte for byte as before. Encryption takes the stream
afterwards, as for any other. The cost is 16 bytes or so a chunk and the matches cut at the
chunk's start; 128 KiB to 2 MiB chunks gave the same speed within 10% (the dictionary's 32 KiB
is run again for each), 512 KiB the size close to the whole's.

## What is kept

- ZIP: UTF-8 names, directory entries, symbolic links as Info-ZIP's `S_IFLNK` entries, MS-DOS,
  extended timestamp and NTFS times, Unix modes and Windows attributes, the owner's ids
  (Info-ZIP's 0x7875), Zip64 where needed.
- 7z: UTF-16 names, empty streams and files, modification, access and creation times, Windows
  attributes with 7-Zip's Unix extension (the mode), symbolic links as 7-Zip on Unix writes them.
- ALZ and EGG: names, directories, the modification time (MS-DOS's; EGG's FILETIME) and
  Windows attributes; in EGG, where the mode is known, POSIX file information (the mode, the
  owner's ids). Not tested against ALZip or Bandizip (only aqr-unpack).
- tar: files, directories, symbolic and hard links (of a link group), FIFOs, devices; mode,
  owner (ids and names), the modification time (to the nanosecond in pax), extended attributes
  (`SCHILY.xattr.`).
- Left out, told: what each format has no room for (FIFOs and devices but in tar, symbolic links
  in ALZ and EGG, extended attributes but in tar, forks).

## Measurements

On a Ryzen 7 9700X, `third_party` (1,399 files, 40.8 MB) as ZIP: 0.30 s on 16 threads, 0.92 s
on one, 20.04 MB; recompressed by `aqr -c` in a directory, 9.88 MB (the files archived
themselves: 9.69 MB).

On an M4 (10 cores), 7z at level 6 of `third_party` (230.2 MB): 6.2 s and 113.7 MB on 10 threads,
48.3 s and 107.0 MB on one; `7zz a -mx6 -mmt10`: 10.7 s, 88.7 MB. ALZ and EGG of 48 files of 1
MiB (50.3 MB): 0.22 s on 10 threads, 1.44 s on one.

One file, 300,000,000 bytes (the head of enwik9) alone as ZIP at level 6, on an M4 (10 cores):

| | time | size | peak RSS | instructions |
|---|---|---|---|---|
| before, 10 threads | 7.13 s | 108,820,960 | 395 MiB | 119.4 G |
| after, 10 threads | 1.66 s | 108,832,255 (+0.010%) | 513 MiB | 125.4 G (+5%) |
| before, `-T 1` | 7.14 s | 108,820,960 | 395 MiB | 119.4 G |
| after, `-T 1` | 7.27 s | 108,820,960 (identical) | 395 MiB | 119.5 G |

`unzip -t` and `7zz t` accept it (`tests/large.rs`). `aqr -c` of that ZIP still takes its
stream apart whole: 86,586,961 bytes before, 86,592,487 of the chunked one, in the same time.

## Status

Left:

- The 7z gap to `7zz -mx6` (22% at 10 threads): its LZMA2 has a dictionary of 64 MiB (8 here,
  XZ Utils' preset 6; a larger one costs some 11 times its size in memory on every thread), and
  it puts executables under BCJ or ARM64 filters. Ordering the folder by extension, as 7-Zip
  does, was measured not to help: 107.4 -> 115.7 MB on one thread; 114.1 -> 111.7 MB but 6.3
  -> 11.9 s on 10.
- Encryption of ALZ and EGG (aqr-unpack does not decrypt them, to check against).
- RAR is not written: a proprietary format, whose compressor RARLAB keeps to itself.
