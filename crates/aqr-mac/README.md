# aqr-mac

Aqueous in the macOS Finder. "Compress with Aqueous" works on files and folders, and "Compress with Aqueous
(Passphrase)…" does so with the archive encrypted (`aqr -P`). "Extract with Aqueous" and "Test
with Aqueous" work on `.aqr` files and the other archives `aqr` extracts, and opening one (a
double-click) extracts it. The other formats are taken from no app: Aqueous opens them where
nothing else does, and is an alternative elsewhere (Get Info, Open with). The progress shows on the file itself in Finder. As in aqr-win
(`crates/aqr-win`), the work is done by `aqr --ipc` in a process of its own (the protocol:
`crates/aqr/src/ipc.rs`). This program only hands Finder's requests to it.

```sh
crates/aqr-mac/bundle.sh   # target/dist/Aqueous.app and Aqueous.zip (dist profile)
crates/aqr-mac/icns.sh     # droplet.icns, from droplet.svg at the root (committed)
# icons/*.icns, the files' (`.aqr` with a blue label, others dark): scripts/icons.py (committed)
```

Aqueous is distributed as `Aqueous.zip`, holding `Aqueous.app` (`aqr-mac` and `aqr` in
`Contents/MacOS`). There is no installer.

- Move the app to Applications and open it once. A launch of its own (not for a file or a
  service) registers it with Launch Services (`LSRegisterURL`: the `.aqr` type and its icon,
  the document type) and the services (`NSUpdateDynamicServices`) and says so. It also offers
  to link `/usr/local/bin/aqr` to the `aqr` in the bundle (as an administrator, through
  `osascript`). Anything else there (a link elsewhere, a file) is named and replaced only if
  the user agrees; a folder there is left alone. An app run from where it was downloaded is
  translocated (run from a read-only copy at a random path), so it only tells the user to
  move it.
- It is signed ad hoc, not notarized. Gatekeeper refuses it downloaded until it is allowed in
  System Settings (Privacy & Security, "Open Anyway"), or `xattr -d com.apple.quarantine` is
  run on it.
- The services are `NSServices` in `Info.plist`, under Quick Actions or Services of a
  Control-click in Finder. Finder sends all the paths chosen at once, so nothing gathers them
  as aqr-win does. Several paths compressed make one archive named after their folder.
- The versions of an `.aqr` archive (its journal's points: `docs/append-design.md`,
  "Journaling"), one archive at a time (several chosen are refused), as aqr-win's verbs:
  "Name this version with Aqueous…" asks for a name (`{today}` at first) and runs
  `aqr --tag`; "Extract a previous version with Aqueous…" lists them
  (`aqr --history --json`, a table in the alert, the last selected, a double-click is OK)
  and extracts the one chosen as it was (`aqr -d --at #N`) into `NAME (LABEL)` beside the
  archive, a job like the others (progress, the overwrite question).
- The app is an agent (`LSUIElement`): no Dock icon, no menu, no window. It quits once its
  jobs are done.
- Progress is an `NSProgress` published for the file at work: the archive being written
  (`aqr`'s `NAME.PID.part`, renamed once done), the folder being extracted into, or the
  archive being tested. Finder draws a bar on its icon with a button that stops the job.
  Stopping sends `aqr` a cancel (its output being written removed), and `aqr` is killed if it
  has not stopped within 10 s.
- Alerts show errors (with the last lines `aqr` wrote) and what a test checked, and ask about
  an output that is there already. A passphrase is asked for in an alert with a secure field
  (typed twice, to choose one, until both match), again where it was wrong; an encrypted
  archive asks on its own. Cancelling it stops the job without an error alert.

- The texts are in the user's language (aqr-l10n: English, Korean; the app's own language in
  System Settings, else the system's). Those Finder shows (the services, the types' names) are
  the bundle's `LANG.lproj/*.strings`, written by `bundle.sh`.

## Status

Left:

- A window of the progress (the phases overlaid, as aqr-win's).
- Notarization (a Developer ID).
- A `.dmg` to drag the app to Applications from.
- Compressing what is dropped on the app's icon (a document type of `public.item`, which would
  put Aqueous in every file's "Open With").
- Quick Look of what an archive holds.
