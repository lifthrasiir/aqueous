# aqr-mount-fuse

`aqr-mount-fuse MOUNTPOINT` mounts a read-only FUSE file system at MOUNTPOINT (an existing
directory) on Linux, for as long as the program runs. SIGINT, SIGTERM and SIGHUP unmount and
end it. A process killed otherwise (SIGKILL, a crash) leaves the mount point answering
"Transport endpoint is not connected" until `fusermount3 -u MOUNTPOINT`. FUSE's
`auto_unmount` would clean that up, but fusermount3 takes it only with `allow_other` or
`allow_root`, which need `user_allow_other` in `/etc/fuse.conf`.

The file system serves a tree of aqr-mount (`src/fs.rs` is the adapter): for now `OneFile`,
`/hello.txt` of fixed contents. Names are case-sensitive and must be UTF-8; owners are the
mounting user, modes 0555 and 0444, and the kernel checks them itself (`default_permissions`).
Entries and attributes are kept by the kernel for a year, and pages across opens
(`FOPEN_KEEP_CACHE`), since the tree never changes.

It links fuser (MIT), which speaks the protocol itself and mounts through the setuid
`fusermount3` (FUSE 3's package, `fuse3`), so no libfuse is linked or needed at build time.
Other systems build a stub that says it is the program of Linux.

## Left

- An archive's tree (aqr-mount's Left).
- How it is released (a binary of the Linux release archive, or a package).
- Tests: they need `/dev/fuse` and `fusermount3`. Tried by hand in WSL2 (Ubuntu, kernel 6.18):
  `ls -la`, `cat`, `stat`, a read at an offset, `df`, a name in another case not found,
  writes refused as `EROFS`, SIGINT unmounting.
