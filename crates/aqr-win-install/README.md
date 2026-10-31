# aqr-win-install

The installer of Aqueous on Windows: `aqr.exe` and `aqr-win.exe` carried in one program, installed
for the user alone (no administrator needed) into `%LOCALAPPDATA%\Programs\Aqueous`.

```sh
cargo build --profile dist -p aqr -p aqr-win    # first: the installer carries these two
cargo build --profile dist -p aqr-win-install   # target/dist/aqr-win-install.exe
aqr-win-install [--quiet]                       # asks first, unless --quiet
```

- `build.rs` packs `target/dist/{aqr,aqr-win}.exe` (`AQR_WIN_INSTALL_FROM` names another
  directory, relative to the workspace's root), checks the payload by unpacking it, and is run
  again when they change. Without them the installer is built anyway, saying so, and tells
  whoever runs it that it carries nothing.
- The files are recompressed as Aqueous would, without Aqueous (which is what is installed): the
  code sections through aqr-exe's x86 disfilter, the base relocations through its `pe-reloc`,
  and each of their streams and the rest of the file as a zstd frame of its own (level 19).
  The installer depends on aqr-exe's decoders and zstd's alone. The layout is in
  `src/payload.rs`.
- `%LOCALAPPDATA%\Programs` is the user's own folder of programs (`FOLDERID_UserProgramFiles`),
  where per-user installers put them (Chrome without an administrator went to
  `%LOCALAPPDATA%\Google\Chrome`), not `%APPDATA%`, which a roaming profile copies about.
- Then `aqr-win --install` (`crates/aqr-win/src/install.rs`): the verbs of Explorer, the
  folder on the user's `PATH`, and the uninstaller in Settings' Apps, `aqr-win --uninstall`.
  The uninstaller takes all that away, then the files; `aqr-win.exe` itself and the folder are
  deleted by `cmd.exe` once it has exited.
- Installing over an installation replaces its files; one running is renamed aside first
  (`*.old`, deleted by the next install or the uninstall).
- The manifest asks for no elevation (`asInvoker`): Windows would otherwise ask for an
  administrator for a program named so.

## Results

Measured on 2026-10-31 on a Ryzen 7 9700X: `aqr.exe` and `aqr-win.exe` of `target/dist` at
`d8bd1d3` (29,177,856 bytes):

| | bytes | |
|---|---|---|
| zstd -19, each file | 9,138,000 | |
| xz -9e, each file | 8,474,364 | |
| the payload | 7,553,736 | −17.3% of zstd's, −10.9% of xz's |
| the installer | 7,973,376 | |

Packing takes 13 s (`build.rs`'s dependencies are not optimised); the installer's own code
(fat LTO as `dist` builds it: zstd's decoder, the x86 decoder, the dialogs), the icon and the
resources are some 0.4 MB.
