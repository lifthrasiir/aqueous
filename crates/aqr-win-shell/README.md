# aqr-win-shell

`.aqr` files browsed in Windows Explorer as folders, as Windows' ZIP folders are: a shell
namespace extension, `aqr-shell.dll` (`docs/win-shell-ext-design.md`; cargo names it
`aqr_shell.dll`, renamed where it is copied: `cargo aqr-win`, the installer). The file is a junction
(its ProgID's `CLSID`, a class with `ShellFolder` and `CATID_BrowsableShellExt`); the shell hands
the folder the file's ID list, and the folder lists the archive's entries as items of its own
(`src/pidl.rs`: one `SHITEMID` for each name of a path, its kind, size, time and attributes
copied in), the view being the shell's own (DefView).

The library is loaded by Explorer and by every program that opens a file dialog, so it holds
nothing but the items: what it lists and reads it asks `aqr-win --serve` (`src/client.rs`: the
server's late-bound objects, kept in the Global Interface Table as the shell calls from several
apartments, made again where the server is gone), which runs `aqr --ipc`. A panic is caught at
every entry point. A folder holds a use of its archive; once the last is let go of, the archive
object is released, for the server to close it a minute later (and to end, idle).

- Folders: `IShellFolder2` (enumeration, binding to folders in it and to a file's `IStream`, as
  `BHID_Stream` asks, parsing paths, names, attributes, comparison), `IPersistFolder3`,
  `IPersistIDList`.
- Columns (`src/columns.rs`): all that `aqr -t -v` tells of an entry but its formula. Name,
  date modified, type (the extension's, as the system tells it) and size by default; data,
  overhead, ratio, segment (of the archive's listing, all of its blocks read once these are
  asked for), owner, permissions, attributes (`RHSATI`), macOS' flags, the flags of `-t`, the
  dates created, accessed and of a change of metadata, the comment and a link's target in
  "More…". Those of a property of the system's are keyed by it (named and shown by the shell in
  the user's language), the others by properties of ours named by aqr-l10n (`folder.col_*`).
  A folder keeps what the server told of the entries it listed, for no call a cell.
- Icons: of the name's extension (`SHCreateFileExtractIconW`).
- Thumbnails and the preview pane, by the system's handlers of a file's type (`src/handlers.rs`):
  a file's associations (`IQueryAssociations`: its extension, `SystemFileAssociations` of it and
  of its perceived type, `*`), through which the preview pane finds its handler (reading the
  file by `BHID_Stream`); a thumbnail by the type's provider, given the file's bytes (up to 64
  MiB) as a stream in memory (a provider may read it from threads of its own), as
  `IThumbnailProvider` and as `IExtractImage2` (what the shell's image factory asks for, at
  every size of Explorer's views).
- Copying out (paste, drag): the shell's data object of the items' ID lists
  (`SHCreateDataObject`), which the copy engine reads as folders and streams (Windows 8 on;
  Windows 7's takes such items only of a folder of `ITransferSource`, and is given the
  descriptors and contents alone, which its drop targets copy themselves); for other
  programs' drops (a browser's upload, a mail's attachment), its inner object (`src/data.rs`)
  has `CFSTR_FILEDESCRIPTORW` (every file and folder under those chosen, by its path from the
  folder) and `CFSTR_FILECONTENTS` (a stream of each by its index).
- The context menu (`src/menu.rs`): the shell's (`SHCreateDefaultContextMenu`: Copy,
  Properties; of folders, the verbs of folders), with ours first of a file chosen alone: "Open"
  (the default) and "Open with the files beside it" (the files of its folder, not under it, and
  those under its folders named after it: a `.cue` and its `.bin`, `page.html` and `page_files`;
  over 256 MiB in all, once the user says so), written out by the server (`Materialize`) on a
  thread of their own, the shell's progress window shown where it takes over 0.4 s (cancelled,
  the file being written stopped and those after it not begun, nothing opened), then opened as
  the system opens a file of its type.
- Properties (the menu's, Alt+Enter; `src/props.rs`): a sheet of a page for each entry chosen,
  of all that `aqr -t -v` tells of it, its formula and instructions too, under the columns'
  names; on a thread of its own, a window of its own.
- Named versions (journal points, `-j`, `--tag`; `src/versions.rs`): the view's background menu
  "Versions", "Latest" and the points newest first, the one shown checked; one chosen is browsed
  to in the same window, the archive as of it (read only, as all of it is): the file's ID list
  with an item of the point after it (`src/pidl.rs`'s `Kind::Version`, never listed; parsed as
  `@` and the point's offset). And the "Versions" tab of the file's Properties
  (`src/history.rs`, a property sheet handler): the points, read once the tab is shown; "Open"
  (or a double-click) opens one in a window of its own, "Extract…", "Name this version…" and
  "Delete later versions…" run aqr-win (`extract-version ARCHIVE #N`, `name-version ARCHIVE`,
  `rollback-version ARCHIVE #N`), the list read again once a version is named or the later
  ones deleted. And a command "Versions" of the view, its subcommands the menu's
  (`src/commands.rs`, `IExplorerCommandProvider`), which Explorer may show in its command bar.
- The menu "Aqueous" of files and folders (`src/more.rs`, an `IExplorerCommand` of
  subcommands: the verb `Aqueous.more`'s `ExplorerCommandHandler`), beside the verbs "Compress
  with Aqueous", "Extract here" and "Extract to…": of archives (by extension, aqr-ipc's
  `shell::ARCHIVES`) "Test", "Mount as a drive" (of one; disabled unless aqr-mount.exe is beside
  the library and WinFsp is installed, a file and a key, so that it is seen to need them), "Extract a previous version…" and "Name
  this version…" (of one `.aqr` file); then of all the ways to compress. Each command tells at
  once whether it is shown of the items it is asked of (the menu itself is told nothing of
  them). Invoked, a verb of many paths drops them on aqr-win's class of it (as Explorer does on
  a verb's `DropTarget`: all at once), the others run aqr-win on the one.
- "Eject" of `Drive` (`src/mount.rs`, the verb's `ExplorerCommandHandler`): of drives of the
  file system `aqr` (the item's `System.Volume.FileSystem`, else the volume's, put off by
  `E_PENDING` where that is not to be slow); invoked, it runs `aqr-win eject`.
  `tests/mount.rs` loads the library built and asks the menu's commands and Eject (`cargo rt
  -p aqr -p aqr-win -p aqr-mount-win -p aqr-win-shell --test mount`; ejecting where WinFsp is).
- Archives of other formats (ZIP, 7z, RAR, ALZ, EGG, tar and its compressors, a single file
  compressed), browsed alike where Aqueous opens their type: read by `aqr --ipc` through
  aqr-unpack, the columns those their format has (the server's `fields`: Data, a file's compressed bytes,
  and Ratio of all but tar's; never Segment nor Overhead), nothing added nor versions.
- Adding files (`src/drop.rs`): files of the file system's dropped on a view, on a folder in it
  or on the archive file (its type's `DropHandler`), or pasted (the view's own "Paste", through
  the view's drop target), always copied, are added by aqr-win (`add ARCHIVE DIR PATH...`:
  `aqr -a`, each at its name in the folder), the user asked first where a name is there
  already; nothing of a journal point's view.
- Encrypted archives: their passphrase asked for by the server's dialog over the window of the
  shell's (the server let come to the front).
- An archive not read (damaged): told in a box over the window, once in a while, the failure
  then one the shell tells nothing of.
- An archive whose file changed (its size or time): looked for every 2 s (by the library's
  thread of the MTA) and as a folder of it is enumerated again; let go of, to be opened anew,
  and the views of its folders told to refresh (`SHCNE_UPDATEDIR`), with no F5.

`cargo aqr-win --register` registers the library kept beside aqr-win (`aqr-win --register`
registers the folder where `aqr-shell.dll` is beside it; the double-click then opens the
archive as a folder, "Extract here" staying a verb). `AQR_SHELL_LOG=FILE` has the library tell
what fails, a line at a time.

`tests/shell.rs` goes through the shell in its own process: `.aqrtest` files of a class of the
test's (registered for the user while it runs), parsed by path, enumerated as shell items, a
file read through `BHID_Stream`, the view made; ignored, as it needs aqr and aqr-win built
beside the library (`cargo rt -p aqr -p aqr-win -p aqr-win-shell --test shell -- --ignored`).

- Windows 7: the `windows` crate links `combase.dll` and `api-ms-win-core-winrt-error-l1-1-0.dll`
  (Windows 8 on), delay-loaded (`build.rs`), the hook (`src/delay.rs`) giving `ole32.dll` for the
  one and a function of no effect for its `RoOriginateErrorW`. The tests are built for the win7
  target and run in the VM too.

## Tests

- `tests/shell.rs`: the shell in its own process (see above): files of a test class parsed by
  path, enumerated as shell items, read through `BHID_Stream`, the view made.

The progress window is tested by hand only. `AQR_SHELL_TEST_OPENED=FILE` has "Open" tell the file
in FILE rather than run it (for tests).

## Left

- **Damaged archives.** Not salvaged: told in a box, nothing listed.
- **ARM64.**
