# aqr-ole

OLE compound files ([MS-CFB]: Windows Installer packages `.msi` and `.msp`, Office 97-2003
documents, HWP 5 documents, Outlook `.msg`, thumbnail databases) read into their streams: a file
system of aqr-disk ([`FS`]), so that aqr-disk takes a file apart into the contents of the streams
and the rest (the header, the FAT and DIFAT, the directory, the mini stream and free sectors), the
contents recompressed in turn by what they are. The formula is aqr-disk's, so this crate has no
module of its own and no FORMAT.md.

A stream of the regular sectors (4096 bytes and over) is a file named by its path from the root; its
extents are the runs of its sectors. A package's cabinet is such a stream, and the FAT's own sectors
lie between its sectors, so that only the extents put it together as the CAB file aqr-mszip and
aqr-lzx take. Streams in the mini stream are left in the rest.

```sh
cargo run --profile release-test -p aqr-ole --example aqr-ole-ls -- FILE            # the streams and their runs
cargo run --profile release-test -p aqr-ole --example aqr-ole-ls -- FILE STREAM OUT # one stream written
```

## Results

Windows Installer packages of `C:\Windows\Installer` (measured on 2026-11-11, 16 threads, `aqr -9`;
LZX from level 9, see aqr-compress's `lzx_on`):

| package | cabinet | before (`-1`) | `-9` |
|---|---|---|---|
| 10ce6a.msi | LZX | 19.60 MB -> 17.29 MB | 16.57 MB (84.6%, 25 s) |
| 617785f.msi | LZX | 20.62 MB -> 19.41 MB | 19.15 MB (92.9%, 21 s) |
| two of 22 MB | LZX | 98.0% | 99.7% (20 s) |

Before this crate no cabinet inside a package was found at all: the FAT's sectors break its bytes
apart. As with `.cab` files alone, the gain is small for one package (aqr-compress's `lzx_on`:
-1.1% over 441 CAB files) and pays with the files archived along (other versions of a product).

An LZX cabinet whose data was over some tens of MB was not used while `lzx-recons` was a whole
module (469 MiB planned for a 66 MB cabinet, over the compressing bound of 256 MiB). Since
`lzx-recons` 3, a part module, a 568 MB cabinet in a 569 MB package comes to 552.33 MB (97.0%,
decoded in 2 min 4 s; aqr-lzx's commit 090c6c36); compressing still holds a folder's data and
parse whole (11.7 GB heap peak).

## Status

Done: version 3 and 4 files with a DIFAT, the streams of the regular sectors, in files of any
directory depth; cut or looping files give what reads.

Left:

- Storages' CLSIDs, timestamps and the mini stream are kept as they are (in the rest).
- HWP 5's streams are raw DEFLATE (no zlib header): not found as such; `\x05HwpSummaryInformation` and
  the rest are small. Office documents' streams are not recompressed, only separated.
- LZX's encoder streamed, for the memory of large cabinets (above).
