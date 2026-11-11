# aqr-isc

InstallShield cabinets (`ISc(`, InstallShield 5 on: the `data1.cab`, `data2.cab`, ... beside a
`data1.hdr`, or alone with the descriptor inside) taken apart: the files' data is a sequence of
chunks, each an independent raw DEFLATE stream (zlib's) of at most 32 KiB of data after the two
bytes of its length, which aqr-flate rebuilds bit for bit from the data and a record. The archive
stores the data (the files', a content of its own each, recompressed in turn: the executables
among them), which compresses far better than the chunks of 32 KiB, and which other files may
share. The formula is the cabinet's bytes in order with the chunks the outputs of one application
of aqr-flate's module (`flate-set`), split, so this crate has no module of its own;
[FORMAT.md](FORMAT.md) tells what is taken.

The format has no public specification. It was written from `docs/installshield-cab.md`, notes of
an agent that read the MIT-licensed unshield project's source (and no other implementation), and
checked against the cabinets of InstallShield Installation Information of Windows (below).

```sh
cargo run --profile release-test -p aqr-isc --example aqr-isc-check -- FILE...   # taken apart, evaluated, compared
```

```rust
// None where it is no data volume, or no chunk is rebuilt
let f = aqr_isc::formula::encode(&cab, threads).unwrap();
```

The file table is in the header file (`data1.hdr`) and the chunks are found without it: the data
starts where the volume header says and the chunks follow one another to the end, so a cabinet
is taken alone, and a file ends where a chunk holds less than a full one. The header file itself is
small and kept as it is. Left: the files' names for their kinds (they come from the header file, a
file of its own), obfuscated files (flag 2: the bytes are not chunks until undone) and stored ones
between chunks, and data volumes past the first of a set, whose descriptors the header gives.

## Results

The six `data1.cab` of `Program Files (x86)\InstallShield Installation Information` on a
Windows 11 machine (versions by the major number the header gives, measured on 2026-11-11 with
`aqr -c`, 16 threads; the cabinets are not published: the programs they install are not ours):

| cabinet | before | after | records | chunks rebuilt |
|---|---|---|---|---|
| 7.05 MB (v21) | 3.27 MB (46.4%) | 2.03 MB (28.8%) | 5.99% | 1608 of 1608 |
| 6.99 MB (v29) | 2.69 MB (38.6%) | 1.72 MB (24.7%) | 6.22% | 1608 of 1608 |
| 6.15 MB (v21) | 2.38 MB (38.7%) | 1.50 MB (24.4%) | 6.27% | 1440 of 1440 |
| 10.81 MB (v12) | 9.91 MB (91.7%) | 4.79 MB (44.3%) | 3.52% | 860 of 860 |
| 54.73 MB (v19) | 50.88 MB (93.0%) | 38.85 MB (71.0%) | 1.60% | 3339 of 3341 |
| 0.42 MB (v12) | 100.0% | 100.0% | 0.91% | 26 of 26 |

(Before: the cabinet as it was, a file of unknown content to zstd at the default level. The last
gains nothing: its chunks are rebuilt, but the archive is no smaller, so the cabinet is kept.) Splitting 7 MB takes 0.25 s, rebuilding 0.13 s.

## Tests

```sh
cargo rt -p aqr-isc
```

Cabinets made of chunks that aqr-flate's zlib emulation writes (level 6 and 9, files of a few
chunks, of one, of a few bytes): taken apart, evaluated natively, compared; a damaged chunk, the
limit and a tail kept. aqr-compress's `installshield_cabinet_taken_apart` takes one through the
whole recompression.

## Status

Done: the data volumes of cabinets with chunked DEFLATE (every `ISc(` cabinet seen).

Not yet: the file names and kinds (the header file with its cabinet: a set), obfuscated and
stored files, files split between volumes, the older framing the notes tell of (chunks cut at
sync-flush markers), InstallShield 3's `.Z` and `.LIB` archives (other formats).
