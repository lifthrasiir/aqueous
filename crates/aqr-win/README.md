# aqr-win

Aqueous in Windows Explorer: "Compress with Aqueous" on files and folders, "Extract here" (also
the double-click) and "Test" on `.aqr` files, and a window of how far the work is. The work is
done by `aqr --ipc` in a process of its own (the protocol: `crates/aqr/src/ipc.rs`); this program
is the window and the verbs, about 470 KB.

```sh
cargo build -r -p aqr-win
# aqr.exe beside aqr-win.exe (or on PATH, or named by AQR)
aqr-win --register      # the verbs, for this user (HKCU); --unregister takes them away
```

- The verbs are registered for the user alone, no administrator needed. On Windows 11 they are
  under "Show more options": the menu above takes only `IExplorerCommand` handlers of packaged
  (MSIX) applications.
- Explorer runs a process for each path chosen; those of a verb in a folder are gathered into
  one (a named pipe, `src/gather.rs`). Several paths compressed make one archive named after
  their folder.
- The window is a task dialog (common controls 6, by the embedded manifest): what is being done,
  a bar of the phases overlaid as on a terminal (lighter, recompressed; darker, compressed;
  the taskbar's, their share), the file at work, the time left, and Cancel, which asks `aqr` to
  stop (its output being written removed; killed if it has not stopped in 10 s). A job done
  within 0.4 s shows no window.
- An output there already is asked about (replace, or skip; for all, where there are several).
  Errors are shown with the last lines `aqr` wrote. A test tells what it checked.

## Status

Left: the menu of Windows 11 (an `IExplorerCommand` DLL and a sparse package), an icon, a list
of what an archive holds, drag and drop, and the questions `aqr` does not ask yet (`--consume`,
damaged blocks).
