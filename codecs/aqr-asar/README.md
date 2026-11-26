# aqr-asar

Electron's asar archives (`app.asar`, `electron.asar`, `node_modules.asar`), rebuilt bit for bit
from the data after their header (`FORMAT.md` specifies the module `asar-recons`, and how it works). The header's JSON is kept, but the files' sizes
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

## What is taken

- Asar archives of `asar` 0.14 to `@electron/asar` 3, with or without integrity; files unpacked, links
  and values written otherwise are kept as they are in the header, and so are the bytes no file tells of.
- Files of the same contents in one archive (of at least 1 KiB) are stored once.

## Results

Windows 11 x86-64, 2026-10-30, 27 archives found on one machine (1.83 GB: VS Code, Dropbox, Discord x4,
Slack x2, Signal, Teams, Evernote, MongoDB Compass, balenaEtcher, Oculus, and games; 127,329 files).
Every archive is rebuilt (`aqr-asar-check`): the encoder at 950 MB/s and the module at 4.1 GB/s natively
(most of it SHA-256), 155 MB/s as WebAssembly (Ridibooks' archive, 73 MB, 4558 files with integrity).

- Headers (9.9 MB) at zstd -3: 2.13 MB as they are, 0.84 MB as the header with holes and the index.
  Archives with integrity gain most (Ridibooks 230 -> 29 KB); Orca's (4555 of its 4578 files unpacked)
  gains nothing.
- `aqr` (all threads, level mid) against the commit before (`18fdaf4`), the 27 archives at once: 714.42 ->
  697.11 MB (-2.4%), 23.3 -> 107 s (aqr-gif's model takes most of the time). Each alone (719.78 -> 706.49
  MB): Evernote 57.08 -> 51.19 MB, shapez 151.99 -> 147.03, Oculus 215.09 -> 212.73, Dropbox 33.06 ->
  31.59; but etcher 36.52 -> 38.48, VS Code's `node_modules.asar` 9.41 -> 10.27, Signal 32.08 -> 32.67.

## Tests

`cargo rt -p aqr-asar`, on archives the tests make as `@electron/asar` and the old `asar` write;
`aqr-asar-check [-v] FILE_OR_DIR...` (whether each archive is rebuilt, zstd -3 of the headers as
they are and as the module's inputs, speeds).

## Left

- **Many small files, alone.** What loses is the contents of many small files by kind (aqr-compress's, as
  aqr-bundle's): alone, the text of the files is a stream apart coded by aqr-wrt in pieces without the
  batch's dictionary, where the archive scanned is one stream compressed solid. Of no kind at all, etcher
  comes to 36.47 MB, but the 27 at once to 724.87 MB: kinds pay in a batch and cost alone. To decide.
