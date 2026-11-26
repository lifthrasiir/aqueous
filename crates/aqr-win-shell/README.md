# aqr-win-shell

`.aqr` files browsed in Windows Explorer as folders, as Windows' ZIP folders are: a shell
namespace extension, `aqr_shell.dll` (`docs/win-shell-ext-design.md`). The file is a junction
(its ProgID's `CLSID`, a class with `ShellFolder` and `CATID_BrowsableShellExt`); the shell hands
the folder the file's ID list, and the folder lists the archive's entries as items of its own
(`src/pidl.rs`: one `SHITEMID` for each name of a path, its kind, size, time and attributes
copied in), the view being the shell's own (DefView).

The library is loaded by Explorer and by every program that opens a file dialog, so it holds
nothing but the items: what it lists and reads it asks `aqr-win --serve` (`src/client.rs`: the
server's late-bound objects, kept in the Global Interface Table as the shell calls from several
apartments, made again where the server is gone), which runs `aqr --serve`. A panic is caught at
every entry point. A folder holds a use of its archive; once the last is let go of, the archive
object is released, for the server to close it a minute later (and to end, idle).

- Folders: `IShellFolder2` (enumeration, binding to folders in it and to a file's `IStream`, as
  `BHID_Stream` asks, parsing paths, names, attributes, comparison), `IPersistFolder3`,
  `IPersistIDList`.
- Columns: name, date modified, type (the extension's, as the system tells it), size, and the
  attributes (`RHSA`, off by default), as the storage properties (`PKEY_Size`, ...).
- Icons: of the name's extension (`SHCreateFileExtractIconW`).

`cargo aqr-win --register` registers the library kept beside aqr-win (`aqr-win --register`
registers the folder where `aqr_shell.dll` is beside it; the double-click then opens the
archive as a folder, "Extract here" staying a verb). `AQR_SHELL_LOG=FILE` has the library tell
what fails, a line at a time.

`tests/shell.rs` goes through the shell in its own process: `.aqrtest` files of a class of the
test's (registered for the user while it runs), parsed by path, enumerated as shell items, a
file read through `BHID_Stream`, the view made; ignored, as it needs aqr and aqr-win built
beside the library (`cargo rt -p aqr -p aqr-win -p aqr-win-shell --test shell -- --ignored`).

## Status

Left (the design's phases 4 on): dragging and copying files out (`IDataObject`, asynchronous),
opening a file (a temporary copy, `Materialize`), context menus, encrypted and damaged archives
told, `SHChangeNotify` where an archive changed, the installer and the release carrying the
library, ARM64.
