# aqr-mount-fuse

`aqr-mount ARCHIVE MOUNTPOINT` mounts the archive as a read-only FUSE file system at
MOUNTPOINT (an existing directory) on Linux, for as long as the program runs. SIGINT, SIGTERM
and SIGHUP unmount and end it. A process killed otherwise (SIGKILL, a crash) leaves the mount point answering
"Transport endpoint is not connected" until `fusermount3 -u MOUNTPOINT`. FUSE's
`auto_unmount` would clean that up, but fusermount3 takes it only with `allow_other` or
`allow_root`, which need `user_allow_other` in `/etc/fuse.conf`.

The file system serves aqr-mount's tree of the archive (`ServedTree`, over an `aqr --serve`:
`AQR`, else the `aqr` beside the program, else the one on `PATH`; `src/fs.rs` is the adapter),
named after the archive in the mount table. Names are case-sensitive and must be UTF-8; owners
are the mounting user, modes 0555 and 0444, and the kernel checks them itself (`default_permissions`).
Entries and attributes are kept by the kernel for a year, and pages across opens
(`FOPEN_KEEP_CACHE`), since the tree never changes.

It links fuser (MIT), which speaks the protocol itself and mounts through the setuid
`fusermount3` (FUSE 3's package, `fuse3`), so no libfuse is linked or needed at build time.
Other systems build a stub that says it is the program of Linux.

The package's binary is `aqr-mount-fuse`, released as `aqr-mount` (the Linux release archives,
`.github/workflows/release.yml`), the name it tells itself by: built here, aqr-mount-win's stub
would take the name `aqr-mount` too, and the two would overwrite each other in one target
directory.

## Left

- Tests: they need `/dev/fuse` and `fusermount3`. Tried by hand in WSL2 (Ubuntu, kernel 6.18):
  `ls -la`, `cat`, `stat`, a read at an offset, `df`, a name in another case not found,
  writes refused as `EROFS`, SIGINT unmounting.
