# aqr-ole

OLE compound files ([MS-CFB]: Windows Installer packages `.msi` and `.msp`, Office 97-2003
documents, HWP 5 documents, Outlook `.msg`, thumbnail databases) read into their streams: a file
system of aqr-disk ([`FS`]), so that aqr-disk takes a file apart into the contents of the streams
and the rest (the header, the FAT and DIFAT, the directory, the mini stream and free sectors), the
contents recompressed in turn by what they are. The formula is aqr-disk's, so this crate has no
module of its own and no FORMAT.md.

```sh
cargo run --profile release-test -p aqr-ole --example aqr-ole-ls -- FILE            # the streams and their runs
cargo run --profile release-test -p aqr-ole --example aqr-ole-ls -- FILE STREAM OUT # one stream written
```

## What is taken

Version 3 and 4 files with a DIFAT, in directories of any depth. A stream of the regular sectors
(4096 bytes and over) is a file named by its path from the root; its extents are the runs of its
sectors. A package's cabinet is such a stream, and the FAT's own sectors lie between its sectors,
so that only the extents put it together as the CAB file aqr-mszip and aqr-lzx take. Cut or looping
files give what reads.

Not taken: streams in the mini stream, which stay in the rest, as do storages' CLSIDs and
timestamps.

## Results

Windows Installer packages of `C:\Windows\Installer` (2026-11-11, 16 threads, `aqr -9`; LZX from
level 9, see aqr-compress's `lzx_on`):

| package | cabinet | `-1` | `-9` |
|---|---|---|---|
| 10ce6a.msi | LZX | 19.60 -> 17.29 MB | 16.57 MB (84.6%, 25 s) |
| 617785f.msi | LZX | 20.62 -> 19.41 MB | 19.15 MB (92.9%, 21 s) |
| two of 22 MB | LZX | | 99.7% (20 s) |

A 568 MB cabinet in a 569 MB package comes to 552.33 MB (97.0%, decoded in 2 min 4 s); compressing
holds a folder's data and parse whole (11.7 GB heap peak). As with `.cab` files alone, the gain is
small for one package (aqr-compress's `lzx_on`: -1.1% over 441 CAB files) and pays with the files
archived along (other versions of a product).

## Tests

`cargo rt -p aqr-ole`:

- `tests/edge.rs`: files built by the test (version 4's sectors of 4096 bytes, a DIFAT of its own,
  directories that loop, many siblings, deep storages, damaged files).

## Left

- **HWP 5.** Its streams are raw DEFLATE (no zlib header) and are not found as such;
  `\x05HwpSummaryInformation` and the rest are small.
- **Office documents.** Their streams are only separated, not recompressed.
- **Mini stream.** Small streams, CLSIDs and timestamps stay in the rest.
- **LZX memory.** LZX's encoder is not streamed, so compressing a large cabinet holds its folder
  whole (11.7 GB heap for 568 MB).
