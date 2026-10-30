# aqr-asar

Electron's asar archives (`app.asar`, `electron.asar`, `node_modules.asar`), rebuilt bit for bit
from the data after their header (`FORMAT.md`). The header's JSON is kept, but the files' sizes
and offsets and their SHA-256 integrity (of the whole file and of its 4 MiB blocks, as
`@electron/asar` writes them) are derived from the data: they are holes in the header, and the
index holds the sizes and offsets once, as varints. Whatever else the header has (files left
unpacked, their integrity, links, values written otherwise) is kept as it is, and so are the
bytes of the data no file tells of (tools that take files out of the header leave them; one
archive of the corpus has 52 MB of them).

As with aqr-bundle, an archive alone gains little: what it is for is that the files become
contents of the application, to be recompressed by their formats (PNG files by aqr-png, the
APKs inside by aqr-zip) and stored once where the same files are archived along with it. Files
of the same contents in one archive (of at least 1 KiB, common in `node_modules`) are stored
once.

## How it does

Measured 2026-10-30 on Windows 11 (x86-64), on 27 archives found on one machine (1.83 GB:
VS Code, Dropbox, Discord ×4, Slack ×2, Signal, Teams, Evernote, MongoDB Compass, balenaEtcher,
Oculus, and games; written by `asar` 0.14 to `@electron/asar` 3, 127,329 files).

- Every archive is rebuilt (`aqr-asar-check`), the encoder at 950 MB/s and the module at
  4.1 GB/s natively (most of it SHA-256, of the archives with integrity); as WebAssembly 155
  MB/s on Ridibooks' archive (73 MB, 4558 files with integrity), against 484 MB/s natively.
- The headers (9.9 MB) at zstd -3: 2.13 MB as they are, 0.84 MB as the header with holes and
  the index. Archives with integrity gain most (Ridibooks 230 → 29 KB, shapez 102 → 15 KB);
  Orca's (4555 of its 4578 files unpacked, their integrity kept) gains nothing.
- Through `aqr` (all threads, level mid) against the commit before (`18fdaf4`), all 27 archives
  at once: 714.42 -> 697.11 MB (-2.4%), 23.3 -> 107 s (726.59 MB before aqr-zip's kept bodies
  as slices of their own, `share_slices` for a file alone and GIF files not taken as
  compressed; 715.68 MB and 26.0 s with them, before aqr-gif, whose model takes most of the
  time). Each alone (`cache/per-file-0129d9f.txt`, 719.78 -> 706.49 MB): Evernote 57.08 ->
  51.19 MB, shapez 151.99 -> 147.03, Oculus 215.09 -> 212.73 (its two APKs share 8.8 MB of
  deflated entries), Dropbox 33.06 -> 31.59, The Devil's Calculator 41.75 -> 40.58 (the GIF
  files by aqr-gif, in all but Oculus); but etcher 36.52 -> 38.48, VS Code's
  `node_modules.asar` 9.41 -> 10.27, Signal 32.08 -> 32.67.
- What loses is the contents of many small files by kind (aqr-compress's, as aqr-bundle's): a
  file alone, the text of its files is a stream apart from the rest, coded by aqr-wrt in pieces
  without the batch's dictionary, where the archive scanned is one stream compressed solid.
  Of no kind at all, etcher comes to 36.47 MB, but the 27 archives at once to 724.87 MB: kinds
  pay in a batch and cost alone. Left to decide.

## Tests

`cargo rt -p aqr-asar`, on archives the tests make as `@electron/asar` and the old `asar` write;
`aqr-asar-check [-v] FILE_OR_DIR...` (whether each archive is rebuilt, zstd -3 of the headers as
they are and as the module's inputs, speeds).
