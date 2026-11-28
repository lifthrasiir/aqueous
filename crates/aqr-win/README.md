# aqr-win

Aqueous in Windows Explorer: "Compress with Aqueous" on files and folders, "Extract here"
(also the double-click) and "Extract to…" (a folder picked, the root of what each archive
holds: `aqr -d --each --into`, the user asked once where files there would be replaced) on
`.aqr` files and the other archives `aqr` extracts, and beside them the menu "Aqueous" of the
rest (of archives "Test", "Mount as a drive" and of `.aqr` files the versions'; then
"Compress with passphrase…", encrypted, `aqr -P`; "Compress into .zip" and ".7z", `aqr
--format zip`, `7z`; "Compress with options…", the dialog of the options, `aqr --ipc`),
and a window of how far the work is. The work is done by `aqr --ipc` in a process of its own (the protocol:
`crates/aqr/src/ipc.rs`); this program is the window and the verbs, about 1 MB (550 KB of it
the icons: `droplet.ico`, the program's, and `icons/*.ico`, the files', from `scripts/icons.py`;
in the installer they take some 230 KB, the drop being the same bytes in each).

```sh
cargo build -r -p aqr-win
# aqr.exe beside aqr-win.exe (or on PATH, or named by AQR)
aqr-win --register      # the verbs, for this user (HKCU); --unregister takes them away
```

Installed by `aqr-win-install` (crates/aqr-win-install), it is run as `aqr-win --install`
(the verbs, the folder on the user's `PATH`, the uninstaller in Settings' Apps) and
`aqr-win --uninstall [--quiet]` (all that taken away, and the folder: `src/install.rs`).

- The menu "Aqueous" is the shell extension's (aqr-win-shell's `more`, the verb's
  `ExplorerCommandHandler`), each command shown of what is chosen; those of many paths dropped
  on this program's classes of them, as the verbs' `DropTarget`s are. Compressing into another format is a verb `compress-FORMAT` for each format of
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

- The texts are in the user's language (aqr-l10n: English, Korean), the windows' chosen as
  the program starts. Those Explorer shows (the verbs, the types' names) are indirect strings of
  the string table in `aqr-win.rc`, so they follow a change of Windows' language without
  registering again.

- Where `aqr-shell.dll` (crates/aqr-win-shell) is beside this program, `--register` registers
  it too: a `.aqr` file (and one of the other formats Aqueous opens) double-clicked opens as a folder in Explorer (its class's `CLSID`, the
  verb `open` the shell's `ExecuteFolder`), "Extract here" staying a verb; and the "Versions"
  tab of a `.aqr` file's Properties (a property sheet handler of the library's), which runs
  `extract-version ARCHIVE #N` (point N extracted, asked for nothing), `name-version` and
  `rollback-version ARCHIVE #N` (asked first: the versions after point N deleted, `aqr
  --rollback`, or point N saved as a new archive beside it, `aqr --convert --keep-until`); and
  its drop handler: files dropped on a `.aqr` file, or in its folder, are added to it by
  `aqr-win add ARCHIVE DIR PATH...` (`aqr -a`, each `--rename DIR/NAME`); and the menu
  "Aqueous" (the library's, its commands shown of what is chosen: of archives "Test", "Mount
  as a drive", the versions'; of all the ways to compress) and "Eject" of the drives (of
  aqr-mount's), the verbs' `ExplorerCommandHandler`s; Mount and Eject run `aqr-win mount ARCHIVE` (aqr-mount, crates/aqr-mount-win,
  beside this program: `aqr-mount --install`; a dialog with Cancel while it opens the archive,
  then the drive opened in Explorer, or the one it is mounted as already) and `aqr-win eject
  DRIVE` (asked first where files of it are open, then forced).
- `aqr-win --serve` is the COM local server of the archives a shell extension browses
  (`docs/win-shell-ext-design.md`, `src/serve.rs`): late-bound objects (`IDispatch`, marshaled
  by the system's proxies, no type library) over one `aqr --ipc`, an archive's handle closed a
  minute after its object is let go of, the program ending after ten minutes of none, at once
  where its class is registered as another program's or not at all, or as the session's event
  `Local\Aqueous.Serve.Quit` is set (the uninstall: then `aqr-shell.dll`, held by Explorer, is
  moved into the temporary directory and deleted as the user next signs in).
  `tests/serve.rs` drives it from another process (ignored: it needs `aqr.exe` built beside,
  `cargo rt -p aqr -p aqr-win --test serve -- --ignored`).

## Left

- **Windows 11's own menu.** An `IExplorerCommand` DLL and a sparse package (the verbs are under
  "Show more options" until then).
- **Questions `aqr` does not ask yet.** `--consume`, damaged blocks.
