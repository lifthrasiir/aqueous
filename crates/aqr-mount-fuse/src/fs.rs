//! The file system: an aqr-mount [`Tree`] served through FUSE (fuser, which speaks the protocol
//! itself and mounts through `fusermount3`, so no libfuse is linked).
//!
//! The tree's node IDs are the inodes as they are (the root is 1 in both). Nothing ever changes,
//! so the kernel is told to keep entries and attributes for long ([`TTL`]) and pages across
//! opens (`FOPEN_KEEP_CACHE`); what comes here is the first lookup of each name, the first read
//! of each page and listings.

use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use aqr_mount::{Attr, Case, Kind, NodeId, Tree};
use fuser::{
    Config, Errno, FileAttr, FileHandle, FileType, FopenFlags, Generation, INodeNo, LockOwner, MountOption, OpenFlags,
    ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, ReplyOpen, Request,
};

/// How long the kernel keeps what it was told: the tree is immutable.
const TTL: Duration = Duration::from_secs(365 * 24 * 60 * 60);

fn errno(e: aqr_mount::Error) -> Errno {
    match e {
        aqr_mount::Error::NotFound => Errno::ENOENT,
        aqr_mount::Error::NotADirectory => Errno::ENOTDIR,
        aqr_mount::Error::IsADirectory => Errno::EISDIR,
        aqr_mount::Error::Io(e) => e.into(),
    }
}

struct Fs {
    tree: Box<dyn Tree>,
    uid: u32,
    gid: u32,
}

impl Fs {
    fn file_attr(&self, node: NodeId, attr: &Attr) -> FileAttr {
        let time = if attr.mtime_ns >= 0 {
            UNIX_EPOCH + Duration::from_nanos(attr.mtime_ns as u64)
        } else {
            UNIX_EPOCH - Duration::from_nanos(attr.mtime_ns.unsigned_abs())
        };
        let (kind, perm, nlink) = match attr.kind {
            Kind::Dir => (FileType::Directory, 0o555, 2),
            Kind::File => (FileType::RegularFile, 0o444, 1),
        };
        FileAttr {
            ino: INodeNo(node),
            size: attr.size,
            blocks: attr.size.div_ceil(512),
            atime: time,
            mtime: time,
            ctime: time,
            crtime: time,
            kind,
            perm,
            nlink,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    fn attr_of(&self, node: NodeId) -> Result<FileAttr, Errno> {
        Ok(self.file_attr(node, &self.tree.attr(node).map_err(errno)?))
    }
}

impl fuser::Filesystem for Fs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        // (a name that is not UTF-8 is in no archive)
        let Some(name) = name.to_str() else { return reply.error(Errno::ENOENT) };
        match self.tree.lookup(parent.0, name, Case::Sensitive).map_err(errno).and_then(|n| self.attr_of(n)) {
            Ok(attr) => reply.entry(&TTL, &attr, Generation(0)),
            Err(e) => reply.error(e),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.attr_of(ino.0) {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(e) => reply.error(e),
        }
    }

    /// Opened for reading only (the mount is read-only besides); the pages are kept across
    /// opens, the contents never changing.
    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, _kill_suid_gid: bool, reply: ReplyOpen) {
        if flags.0 & libc::O_ACCMODE != libc::O_RDONLY {
            return reply.error(Errno::EROFS);
        }
        match self.tree.attr(ino.0) {
            Ok(Attr { kind: Kind::File, .. }) => reply.opened(FileHandle(0), FopenFlags::FOPEN_KEEP_CACHE),
            Ok(_) => reply.error(Errno::EISDIR),
            Err(e) => reply.error(errno(e)),
        }
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        // (FUSE wants the whole of the size but at the end: reads are repeated until it is had)
        let mut buf = vec![0; size as usize];
        let mut have = 0;
        while have < buf.len() {
            match self.tree.read(ino.0, offset + have as u64, &mut buf[have..]) {
                Ok(0) => break,
                Ok(n) => have += n,
                Err(e) => return reply.error(errno(e)),
            }
        }
        reply.data(&buf[..have]);
    }

    /// `.`, `..` and the entries; an entry's offset is the index of the next one. The parent
    /// of a directory is not known to the tree, so `..` is given as the directory itself (the
    /// kernel answers `..` lookups from its own cache).
    fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
        let entries = match self.tree.entries(ino.0) {
            Ok(entries) => entries,
            Err(e) => return reply.error(errno(e)),
        };
        let dots = [(".", ino.0), ("..", ino.0)];
        let all = dots.into_iter().chain(entries.iter().map(|e| (e.name.as_str(), e.node)));
        for (i, (name, node)) in all.enumerate().skip(offset as usize) {
            let kind = match self.tree.attr(node) {
                Ok(Attr { kind: Kind::Dir, .. }) => FileType::Directory,
                Ok(_) => FileType::RegularFile,
                Err(e) => return reply.error(errno(e)),
            };
            if reply.add(INodeNo(node), i as u64 + 1, kind, name) {
                break;
            }
        }
        reply.ok();
    }
}

/// Signals that end the program, blocked in every thread and taken by [`serve`].
fn ending_signals() -> libc::sigset_t {
    unsafe {
        let mut set = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for s in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::sigaddset(&mut set, s);
        }
        set
    }
}

/// Mounts the file system and serves it until a signal ends the program, then unmounts.
pub fn serve(tree: Box<dyn Tree>, mount_point: &Path) -> io::Result<()> {
    // Blocked before the session's threads start, which inherit the mask: the signals then
    // come to sigwait here alone.
    let signals = ending_signals();
    let rc = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &signals, std::ptr::null_mut()) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::FSName(tree.label()),
        MountOption::Subtype("aqr".into()),
        MountOption::RO,
        MountOption::NoDev,
        MountOption::NoSuid,
        // (the kernel checks the modes itself)
        MountOption::DefaultPermissions,
    ];
    config.n_threads = Some(std::thread::available_parallelism().map_or(1, |n| n.get()));
    let session = fuser::spawn_mount(Fs { tree, uid, gid }, mount_point, &config)?;
    eprintln!("aqr-mount: mounted; end the program (Ctrl+C) to unmount");
    let mut signal = 0;
    let rc = unsafe { libc::sigwait(&signals, &mut signal) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    session.umount_and_join()
}
