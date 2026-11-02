# aqr-win

Aqueous in Windows Explorer: "Compress with Aqueous" on files and folders, and beside it the
menu "Aqueous" of the rest ("Compress with passphrase…", encrypted, `aqr -P`; a separator;
"Compress into .zip", `aqr --format zip`), "Extract here" (also the double-click) and "Test" on `.aqr`
files and the other archives `aqr` extracts, and a window of how far the work is. The work is
done by `aqr --ipc` in a process of its own (the protocol: `crates/aqr/src/ipc.rs`); this
program is the window and the verbs, about 1 MB (550 KB of it the icons: `droplet.ico`, the
program's, and `icons/*.ico`, the files', from `scripts/icons.py`; in the installer they take
some 230 KB, the drop being the same bytes in each).

```sh
cargo build -r -p aqr-win
# aqr.exe beside aqr-win.exe (or on PATH, or named by AQR)
aqr-win --register      # the verbs, for this user (HKCU); --unregister takes them away
```

Installed by `aqr-win-install` (crates/aqr-win-install), it is run as `aqr-win --install`
(the verbs, the folder on the user's `PATH`, the uninstaller in Settings' Apps) and
`aqr-win --uninstall [--quiet]` (all that taken away, and the folder: `src/install.rs`).

- The menu "Aqueous" is the registry's own (`SubCommands` and a `shell` key under the verb,
  the verbs in the order of their keys' names, `CommandFlags` for the separator): no shell
  extension. Compressing into another format is a verb `compress-FORMAT` for each format of
  `OTHER_FORMATS` (`src/register.rs`), the archive of several paths named after their folder
  with that extension.
- The verbs are registered for the user alone, no administrator needed; "Compress with Aqueous"
  takes this program's icon, `.aqr` files it with a label, "AQR". On Windows 11 the
  verbs are under "Show more options": the menu above takes only `IExplorerCommand` handlers of
  packaged (MSIX) applications.
- The other formats (`.zip`, `.7z`, `.rar`, `.alz`, `.egg`, `.tar`, `.tgz`, `.tbz2`, `.txz`,
  `.gz`, `.bz2`, `.xz`, `.lzma`, `.zst`) take the drop in gray with their label, and are taken
  from no other program: Aqueous opens them where nothing did, and elsewhere is offered (Windows
  8 on asks which program to open the next such file with; Settings' default apps lists
  Aqueous). `src/register.rs` has the keys.
- Explorer runs a process for each path chosen; those of a verb in a folder are gathered into
  one (a named pipe, `src/gather.rs`). Several paths compressed make one archive named after
  their folder.
- The window is a task dialog (common controls 6, by the embedded manifest): what is being done,
  a bar of the phases overlaid as on a terminal (lighter, recompressed; darker, compressed;
  the taskbar's, their share), the file at work, the time left, and Cancel, which asks `aqr` to
  stop (its output being written removed; killed if it has not stopped in 10 s). A job done
  within 0.4 s shows no window.
- An output there already is asked about (replace, or skip; for all, where there are several).
  A passphrase is asked for in a dialog of its own (`src/passphrase.rs`: a template made in
  memory, as task dialogs have no edit controls), two fields to choose one (it answers only
  when both match), and again where it was wrong; an encrypted archive asks on its own.
  Cancelling it stops the job without an error shown. Errors are shown with the last lines
  `aqr` wrote. A test tells what it checked.

## Status

Left: the menu of Windows 11 (an `IExplorerCommand` DLL and a sparse package), a list
of what an archive holds, drag and drop, and the questions `aqr` does not ask yet (`--consume`,
damaged blocks).
