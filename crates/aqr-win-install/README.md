# aqr-win-install

The installer of Aqueous on Windows: `aqr.exe`, `aqr-win.exe`, `aqr-shell.dll` (the archives
browsed in Explorer as folders, crates/aqr-win-shell) and `man.html` (the manual, which `aqr
--help -v` opens) carried in one program, installed for the user alone (no administrator
needed) into `%LOCALAPPDATA%\Programs\Aqueous`.

```sh
cargo build --profile dist -p aqr -p aqr-win -p aqr-win-shell   # first: the installer carries these
python scripts/man2html.py --single target/dist/man.html       # and this, where it is there
cargo build --profile dist -p aqr-win-install   # target/dist/aqr-win-install.exe
aqr-win-install [--quiet]                       # asks first, unless --quiet
aqr-win-install --tour                          # the window after installing, installing nothing
```

`build.bat` runs the two builds as the release workflow does.

- `build.rs` packs `target/dist/{aqr,aqr-win}.exe` and `aqr_shell.dll` (as `aqr-shell.dll`;
  `AQR_WIN_INSTALL_FROM` names another directory, relative to the workspace's root), checks the
  payload by unpacking it, and is run again when they change. Without them the installer is
  built anyway, saying so, and tells whoever runs it that it carries nothing. `man.html` of the
  same directory goes with them where it is there; without it the installer is built without
  the manual, saying so (`aqr --help -v` then opens the site's page).
- The files are recompressed as Aqueous would, without Aqueous (which is what is installed): the
  code sections through aqr-exe's x86 disfilter, the base relocations through its `pe-reloc`.
  The streams of a kind of all the files are one raw LZMA2 frame (solid; preset 9e, lc/lp/pb
  chosen for each kind), as are the rest of the images and that of the other files, decoded
  each on a thread of its own. The installer depends on aqr-exe's decoders and liblzma's
  alone. The layout is in `src/payload.rs`.
- `%LOCALAPPDATA%\Programs` is the user's own folder of programs (`FOLDERID_UserProgramFiles`),
  where per-user installers put them (Chrome without an administrator went to
  `%LOCALAPPDATA%\Google\Chrome`), not `%APPDATA%`, which a roaming profile copies about.
- Then `aqr-win --install` (`crates/aqr-win/src/install.rs`): the verbs of Explorer, the
  folder on the user's `PATH`, and the uninstaller in Settings' Apps, `aqr-win --uninstall`.
  The uninstaller takes all that away, then the files; `aqr-win.exe` itself and the folder are
  deleted by `cmd.exe` once it has exited.
- Installing over an installation replaces its files; one running is renamed aside first
  (`*.old`, deleted by the next install or the uninstall). `aqr-shell.dll`, which Explorer holds
  once it loaded it, is replaced so too, and Explorer runs the one it had until it is started
  again, as the installer then says.
- Installed, a window shows what was installed as Explorer shows it (`src/tour.rs`): a file and
  a `.aqr` file right-clicked, their menus open, the submenu "Aqueous" too, drawn by the theme's
  parts of menus (laid out as measured of Windows 11's menus, from the theme's metrics before
  it, as Windows 7's menus are laid out of them, in the device's pixels; without visual styles,
  as classic menus. Both looked at against real menus in the Windows 7 VM, Windows Server 2008
  R2 classic and Windows 7 Basic, at 96 and 144 DPI). Where the window would be wider than the
  screen, the two files are one under the other. The line under them tells what the command pointed at or chosen does (every command is a
  stop of Tab, the arrows move as in a menu), else what was installed, that on Windows 11 the
  menus are under "Show more options", and that Explorer must be restarted after an update.
  "Mount as a drive" is drawn disabled unless aqr-mount and WinFsp are installed, as the menu
  shows it then, its line telling what it needs. `AQR_TOUR_CLASSIC` draws the layout of
  Windows 10 and before on Windows 11, to look at it.
- The manifest asks for no elevation (`asInvoker`): Windows would otherwise ask for an
  administrator for a program named so.

## Results

Measured on 2026-10-31 on a Ryzen 7 9700X: `aqr.exe` and `aqr-win.exe` of `target/dist` at
`d8bd1d3` (29,177,856 bytes):

| | bytes | |
|---|---|---|
| zstd -19, each file | 9,138,000 | |
| xz -9e, each file | 8,474,364 | |
| the payload (zstd -19, each stream) | 7,553,736 | −17.3% of zstd's, −10.9% of xz's |
| the installer | 7,973,376 | |

Measured on 2026-11-27 on the same machine: the four files of `target/dist` at `6505eaa8`
(27,806,289 bytes), each stream (and rest) of each file a frame of its own, or those of a kind
one frame (solid):

| | each | solid |
|---|---|---|
| zstd -19 | 8,745,224 | 8,550,705 |
| zstd -22 | 8,735,854 | 8,538,670 |
| LZMA2 9e (lc 3, lp 0, pb 2) | 8,088,275 | 7,895,732 |
| LZMA2 9e, lc/lp/pb of each kind | | 7,836,872 |

One frame of all of it was no better (7,893,517 bytes of LZMA2, 8,599,556 of zstd -19). Of
the last, the payload is 7,837,451 bytes (8,745,447 before) and the installer 8,307,712
(9,243,648): −10.1%. Unpacking takes 167 ms (96 ms of zstd), the longest frame (`ST_OP`, 16
MB) on its own thread; packing 9.3 s (10.9 s), `build.rs`'s dependencies not optimised. The
installer's own code (fat LTO as `dist` builds it: liblzma's decoder, the x86 decoder, the
dialogs), the icon and the resources are 470 KB (498 KB with zstd's decoder).

## Left

- Nothing known.
