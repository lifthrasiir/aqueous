//! The metadata of files on the system, as the optional metadata of an archive has it
//! ([`Extra`]): read of the files archived ([`Reader`]), and applied to those extracted
//! ([`Applier`]).
//!
//! What is not commonly kept is read and applied only where asked (`-p`, `privileged`): the
//! status change and backup times are read then, and the ownership, the superuser's flags and
//! the set-user-ID and set-group-ID bits applied then.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use aqr_formula::aqr::{EntryNode, Extra, MAC_FLAGS_ALLOWED, Time};

/// The name of the resource fork of macOS among forks (spec, "forks").
#[cfg(target_os = "macos")]
const RESOURCE_FORK: &[u8] = b"AFP_Resource";
/// The name of the resource fork of macOS among its extended attributes.
#[cfg(target_os = "macos")]
const RESOURCE_FORK_XATTR: &[u8] = b"com.apple.ResourceFork";
/// `UF_*` of `mac-flags`, which the owner can change; the rest (`SF_*`) the superuser alone.
#[cfg(target_os = "macos")]
const USER_FLAGS: u32 = 0xffff;

/// Reads the metadata of the files archived; the names of users and groups found once each.
#[derive(Default)]
pub struct Reader {
    pub privileged: bool,
    users: HashMap<u32, Option<Vec<u8>>>,
    groups: HashMap<u32, Option<Vec<u8>>>,
}

/// A fork to archive: its name, and the path its content is read by.
pub struct Fork {
    pub name: Vec<u8>,
    pub path: PathBuf,
    pub size: u64,
}

impl Reader {
    pub fn new(privileged: bool) -> Reader {
        Reader { privileged, ..Reader::default() }
    }

    /// The extra of `path` (of metadata `m`, not followed where it is a link), and its forks
    /// (whose contents the caller gives `Extra::forks`). What cannot be read of it is told by
    /// `warn`.
    pub fn extra(&mut self, path: &Path, m: &fs::Metadata, warn: &mut dyn FnMut(String)) -> (Extra, Vec<Fork>) {
        let mut x =
            Extra { mtime: m.modified().ok().map(time_of), crtime: m.created().ok().map(time_of), ..Extra::default() };
        let mut forks = Vec::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let t = |secs: i64, nanos: i64| Some(Time { secs, nanos: nanos as u32 });
            x.atime = t(m.atime(), m.atime_nsec());
            x.mtime = t(m.mtime(), m.mtime_nsec());
            if self.privileged {
                x.ctime = t(m.ctime(), m.ctime_nsec());
            }
            x.mode = Some(m.mode() & 0o7777);
            x.uid = Some(m.uid() as u64);
            x.gid = Some(m.gid() as u64);
            x.uname = self.users.entry(m.uid()).or_insert_with(|| os::user_name(m.uid())).clone();
            x.gname = self.groups.entry(m.gid()).or_insert_with(|| os::group_name(m.gid())).clone();
            match os::xattrs(path) {
                Ok(a) => x.xattrs = a,
                Err(e) => warn(format!("{}: extended attributes not read: {e}", path.display())),
            }
        }
        #[cfg(target_os = "macos")]
        {
            use std::os::macos::fs::MetadataExt;
            if m.st_flags() & MAC_FLAGS_ALLOWED != 0 {
                x.mac_flags = Some(m.st_flags() & MAC_FLAGS_ALLOWED);
            }
            if self.privileged {
                x.arctime = os::backup_time(path);
            }
            // (the resource fork a fork, not an attribute)
            if let Some(i) = x.xattrs.iter().position(|a| a.0 == RESOURCE_FORK_XATTR) {
                let (_, rsrc) = x.xattrs.remove(i);
                if m.is_file() && !rsrc.is_empty() {
                    let path = fork_path(path);
                    forks.push(Fork { name: RESOURCE_FORK.to_vec(), path, size: rsrc.len() as u64 });
                }
            }
        }
        let _ = (path, &mut forks, MAC_FLAGS_ALLOWED);
        (x, forks)
    }
}

/// The path the resource fork of file `path` is read and written by.
#[cfg(target_os = "macos")]
fn fork_path(path: &Path) -> PathBuf {
    let mut p = std::ffi::OsString::from(path);
    p.push("/..namedfork/rsrc");
    p.into()
}

/// The node of `path` of metadata `m`, where it is a FIFO or a device.
pub fn special(m: &fs::Metadata) -> Option<EntryNode> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let t = m.file_type();
        if t.is_fifo() {
            return Some(EntryNode::Fifo);
        }
        if t.is_char_device() || t.is_block_device() {
            let (major, minor) = os::device(m.rdev());
            return Some(EntryNode::Device { block: t.is_block_device(), major, minor });
        }
    }
    let _ = m;
    None
}

fn time_of(t: std::time::SystemTime) -> Time {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => Time { secs: d.as_secs() as i64, nanos: d.subsec_nanos() },
        Err(e) => {
            let d = e.duration();
            let (s, n) = (d.as_secs() as i64, d.subsec_nanos());
            if n == 0 { Time { secs: -s, nanos: 0 } } else { Time { secs: -s - 1, nanos: 1_000_000_000 - n } }
        }
    }
}

/// What an entry being extracted is, as far as its metadata is applied.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    /// A symbolic link, its metadata applied to itself.
    Link,
    /// A FIFO or a device.
    Special,
}

/// Applies the metadata of the entries extracted; what cannot be applied but needs not fail the
/// extraction is told (but with `quiet`).
pub struct Applier {
    pub privileged: bool,
    pub quiet: bool,
}

impl Applier {
    pub fn tell(&self, path: &Path, what: &str) {
        if !self.quiet {
            eprintln!("{}: {what}", path.display());
        }
    }

    /// Writes fork `name` of `data` to `path`, a `kind` (told where it cannot be written).
    pub fn fork(&self, path: &Path, kind: Kind, name: &[u8], data: &[u8]) {
        let shown = String::from_utf8_lossy(name);
        if let Err(e) = os::fork(path, kind, name, data) {
            self.tell(path, &format!("fork {shown} not written: {e}"));
        }
    }

    /// Creates FIFO or device `node` at `path` (where asked).
    pub fn special(&self, path: &Path, node: &EntryNode) -> Result<bool, String> {
        if !self.privileged {
            self.tell(path, "a FIFO or a device, not created (-p to create it)");
            return Ok(false);
        }
        os::make_special(path, node).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(true)
    }

    /// Applies `x` to the `kind` at `path` (its forks written already): its extended
    /// attributes, its owner (where asked), its permissions, its times and its flags, in this
    /// order (spec, "Extraction"). Fails where its permissions or times cannot be set, or its
    /// owner where asked.
    pub fn apply(&self, path: &Path, kind: Kind, x: &Extra) -> Result<(), String> {
        let err = |e: std::io::Error| format!("{}: {e}", path.display());
        // (the attributes the forks are written as)
        let forks: Vec<&[u8]> = x.forks.iter().filter_map(|f| os::fork_xattr(kind, &f.0)).collect();
        for (name, value) in &x.xattrs {
            let shown = String::from_utf8_lossy(name);
            if forks.contains(&&name[..]) {
                self.tell(path, &format!("extended attribute {shown} not written: a fork of the same name"));
            } else if let Err(e) = os::set_xattr(path, name, value) {
                self.tell(path, &format!("extended attribute {shown} not written: {e}"));
            }
        }
        #[cfg(unix)]
        if self.privileged && (x.uid.is_some() || x.uname.is_some() || x.gid.is_some() || x.gname.is_some()) {
            let uid = x.uname.as_deref().and_then(os::user_id).or(x.uid.and_then(|u| u32::try_from(u).ok()));
            let gid = x.gname.as_deref().and_then(os::group_id).or(x.gid.and_then(|g| u32::try_from(g).ok()));
            std::os::unix::fs::lchown(path, uid, gid).map_err(err)?;
        }
        if let Some(mode) = x.mode {
            let mode = if self.privileged { mode } else { mode & 0o1777 };
            os::set_mode(path, kind, mode).map_err(err)?;
        }
        os::set_times(path, x).map_err(err)?;
        #[cfg(target_os = "macos")]
        if let Some(t) = x.crtime.filter(|_| kind != Kind::Link)
            && let Err(e) = os::set_attr_time(path, libc::ATTR_CMN_CRTIME, t)
        {
            self.tell(path, &format!("creation time not set: {e}"));
        }
        #[cfg(target_os = "macos")]
        if let Some(t) = x.arctime.filter(|_| self.privileged && kind != Kind::Link)
            && let Err(e) = os::set_attr_time(path, libc::ATTR_CMN_BKUPTIME, t)
        {
            self.tell(path, &format!("backup time not set: {e}"));
        }
        #[cfg(target_os = "macos")]
        if let Some(f) = x.mac_flags {
            let f = if self.privileged { f } else { f & USER_FLAGS };
            if f != 0
                && let Err(e) = os::set_flags(path, f)
            {
                self.tell(path, &format!("flags not set: {e}"));
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
mod os {
    use std::ffi::{CStr, CString};
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use aqr_formula::aqr::{EntryNode, Extra, Time};

    use super::Kind;

    fn c_path(p: &Path) -> io::Result<CString> {
        CString::new(p.as_os_str().as_bytes()).map_err(|_| io::Error::other("a path of a zero byte"))
    }

    fn c_name(n: &[u8]) -> io::Result<CString> {
        CString::new(n).map_err(|_| io::Error::other("a name of a zero byte"))
    }

    fn check(r: libc::c_int) -> io::Result<()> {
        if r < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }

    /// A buffer of `n` bytes filled by `f` (its size, or below zero for an error), grown where
    /// the bytes became more since they were counted (`ERANGE`).
    fn sized(mut f: impl FnMut(*mut u8, usize) -> isize) -> io::Result<Vec<u8>> {
        loop {
            let n = f(std::ptr::null_mut(), 0);
            if n < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut b = vec![0u8; n as usize];
            let got = f(b.as_mut_ptr(), b.len());
            if got >= 0 {
                b.truncate(got as usize);
                return Ok(b);
            }
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::ERANGE) {
                return Err(e);
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn list(p: &CStr, b: *mut u8, n: usize) -> isize {
        // SAFETY: `b` is of `n` bytes, or null with `n` zero
        unsafe { libc::listxattr(p.as_ptr(), b.cast(), n, libc::XATTR_NOFOLLOW) }
    }

    #[cfg(target_os = "macos")]
    fn get(p: &CStr, name: &CStr, b: *mut u8, n: usize) -> isize {
        // SAFETY: as `list`
        unsafe { libc::getxattr(p.as_ptr(), name.as_ptr(), b.cast(), n, 0, libc::XATTR_NOFOLLOW) }
    }

    #[cfg(target_os = "linux")]
    fn list(p: &CStr, b: *mut u8, n: usize) -> isize {
        // SAFETY: as above
        unsafe { libc::llistxattr(p.as_ptr(), b.cast(), n) }
    }

    #[cfg(target_os = "linux")]
    fn get(p: &CStr, name: &CStr, b: *mut u8, n: usize) -> isize {
        // SAFETY: as above
        unsafe { libc::lgetxattr(p.as_ptr(), name.as_ptr(), b.cast(), n) }
    }

    /// The extended attributes of `path` (not followed), by name.
    pub fn xattrs(path: &Path) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let p = c_path(path)?;
        let names = match sized(|b, n| list(&p, b, n)) {
            Err(e) if e.raw_os_error() == Some(libc::ENOTSUP) => return Ok(Vec::new()),
            r => r?,
        };
        let mut out = Vec::new();
        for name in names.split(|&b| b == 0).filter(|n| !n.is_empty()) {
            let c = c_name(name)?;
            match sized(|b, n| get(&p, &c, b, n)) {
                Ok(v) => out.push((name.to_vec(), v)),
                // (gone since it was listed)
                Err(e) if e.raw_os_error() == Some(NO_ATTR) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    /// The error of an attribute not there.
    #[cfg(target_os = "macos")]
    const NO_ATTR: libc::c_int = libc::ENOATTR;
    #[cfg(target_os = "linux")]
    const NO_ATTR: libc::c_int = libc::ENODATA;

    pub fn set_xattr(path: &Path, name: &[u8], value: &[u8]) -> io::Result<()> {
        let (p, n) = (c_path(path)?, c_name(name)?);
        let (v, len) = (value.as_ptr().cast(), value.len());
        // SAFETY: `value` is of `len` bytes
        #[cfg(target_os = "macos")]
        let r = unsafe { libc::setxattr(p.as_ptr(), n.as_ptr(), v, len, 0, libc::XATTR_NOFOLLOW) };
        // SAFETY: as above
        #[cfg(target_os = "linux")]
        let r = unsafe { libc::lsetxattr(p.as_ptr(), n.as_ptr(), v, len, 0) };
        check(r)
    }

    /// The extended attribute that fork `name` of a `kind` is written as (the resource fork
    /// too), where it can be written (`None`: a name reserved here).
    pub fn fork_xattr(kind: Kind, name: &[u8]) -> Option<&[u8]> {
        #[cfg(target_os = "macos")]
        match name {
            super::RESOURCE_FORK if kind == Kind::File => return Some(super::RESOURCE_FORK_XATTR),
            super::RESOURCE_FORK | super::RESOURCE_FORK_XATTR => return None,
            _ => {}
        }
        let _ = kind;
        Some(name)
    }

    pub fn fork(path: &Path, kind: Kind, name: &[u8], data: &[u8]) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        if name == super::RESOURCE_FORK && kind == Kind::File {
            return std::fs::write(super::fork_path(path), data);
        }
        match fork_xattr(kind, name) {
            Some(n) => set_xattr(path, n, data),
            None => Err(io::Error::other("a name reserved here")),
        }
    }

    pub fn set_mode(path: &Path, kind: Kind, mode: u32) -> io::Result<()> {
        let p = c_path(path)?;
        let flags = if kind == Kind::Link { libc::AT_SYMLINK_NOFOLLOW } else { 0 };
        // (Linux has no modes of links)
        if cfg!(target_os = "linux") && kind == Kind::Link {
            return Ok(());
        }
        // SAFETY: a path of a C string
        check(unsafe { libc::fchmodat(libc::AT_FDCWD, p.as_ptr(), mode as libc::mode_t, flags) })
    }

    /// Sets the access and modification times of `path` (not followed) where `x` has them.
    pub fn set_times(path: &Path, x: &Extra) -> io::Result<()> {
        if x.atime.is_none() && x.mtime.is_none() {
            return Ok(());
        }
        let spec = |t: Option<Time>| match t {
            Some(t) => libc::timespec { tv_sec: t.secs as libc::time_t, tv_nsec: t.nanos as _ },
            None => libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_OMIT as _ },
        };
        let times = [spec(x.atime), spec(x.mtime)];
        let p = c_path(path)?;
        // SAFETY: two times, as it takes
        check(unsafe { libc::utimensat(libc::AT_FDCWD, p.as_ptr(), times.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) })
    }

    pub fn make_special(path: &Path, node: &EntryNode) -> io::Result<()> {
        let p = c_path(path)?;
        let r = match *node {
            // SAFETY: a path of a C string
            EntryNode::Fifo => unsafe { libc::mkfifo(p.as_ptr(), 0o600) },
            EntryNode::Device { block, major, minor } => {
                let (Ok(major), Ok(minor)) = (major.try_into(), minor.try_into()) else {
                    return Err(io::Error::other("a device number beyond this system's"));
                };
                let kind = if block { libc::S_IFBLK } else { libc::S_IFCHR };
                let dev = libc::makedev(major, minor);
                // SAFETY: as above
                unsafe { libc::mknod(p.as_ptr(), kind | 0o600, dev) }
            }
            _ => unreachable!("not a FIFO nor a device"),
        };
        check(r)
    }

    /// The major and minor numbers of device `dev`.
    pub fn device(dev: u64) -> (u64, u64) {
        let dev = dev as libc::dev_t;
        (libc::major(dev) as u64, libc::minor(dev) as u64)
    }

    fn name_of(id: u32, group: bool) -> Option<Vec<u8>> {
        let mut buf = vec![0 as libc::c_char; 16 << 10];
        let mut out = std::ptr::null_mut();
        // SAFETY: the entries and the buffer are ours, and the name read while they live
        unsafe {
            if group {
                let mut g: libc::group = std::mem::zeroed();
                libc::getgrgid_r(id, &mut g, buf.as_mut_ptr(), buf.len(), &mut out);
                (!out.is_null()).then(|| CStr::from_ptr(g.gr_name).to_bytes().to_vec())
            } else {
                let mut pw: libc::passwd = std::mem::zeroed();
                let mut out_pw = std::ptr::null_mut();
                libc::getpwuid_r(id, &mut pw, buf.as_mut_ptr(), buf.len(), &mut out_pw);
                (!out_pw.is_null()).then(|| CStr::from_ptr(pw.pw_name).to_bytes().to_vec())
            }
        }
    }

    pub fn user_name(uid: u32) -> Option<Vec<u8>> {
        name_of(uid, false)
    }

    pub fn group_name(gid: u32) -> Option<Vec<u8>> {
        name_of(gid, true)
    }

    fn id_of(name: &[u8], group: bool) -> Option<u32> {
        let name = CString::new(name).ok()?;
        let mut buf = vec![0 as libc::c_char; 16 << 10];
        // SAFETY: as `name_of`
        unsafe {
            if group {
                let mut g: libc::group = std::mem::zeroed();
                let mut out = std::ptr::null_mut();
                libc::getgrnam_r(name.as_ptr(), &mut g, buf.as_mut_ptr(), buf.len(), &mut out);
                (!out.is_null()).then_some(g.gr_gid)
            } else {
                let mut pw: libc::passwd = std::mem::zeroed();
                let mut out = std::ptr::null_mut();
                libc::getpwnam_r(name.as_ptr(), &mut pw, buf.as_mut_ptr(), buf.len(), &mut out);
                (!out.is_null()).then_some(pw.pw_uid)
            }
        }
    }

    pub fn user_id(name: &[u8]) -> Option<u32> {
        id_of(name, false)
    }

    pub fn group_id(name: &[u8]) -> Option<u32> {
        id_of(name, true)
    }

    #[cfg(target_os = "macos")]
    fn attrs(common: libc::attrgroup_t) -> libc::attrlist {
        // SAFETY: plain data, zero where not set
        let mut a: libc::attrlist = unsafe { std::mem::zeroed() };
        a.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        a.commonattr = common;
        a
    }

    /// The backup time of `path` (not followed), where it has one (not zero).
    #[cfg(target_os = "macos")]
    pub fn backup_time(path: &Path) -> Option<Time> {
        let p = c_path(path).ok()?;
        let mut a = attrs(libc::ATTR_CMN_BKUPTIME);
        // (the length of the attributes, then the time)
        #[repr(C)]
        struct Buf {
            len: u32,
            t: libc::timespec,
        }
        // SAFETY: as `attrs`
        let mut b: Buf = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<Buf>();
        // SAFETY: the buffer is of `size` bytes
        let r = unsafe {
            libc::getattrlist(p.as_ptr(), (&raw mut a).cast(), (&raw mut b).cast(), size, libc::FSOPT_NOFOLLOW)
        };
        (r == 0 && (b.t.tv_sec, b.t.tv_nsec) != (0, 0)).then_some(Time { secs: b.t.tv_sec, nanos: b.t.tv_nsec as u32 })
    }

    /// Sets common attribute `attr` (a time) of `path` (not followed) to `t`.
    #[cfg(target_os = "macos")]
    pub fn set_attr_time(path: &Path, attr: libc::attrgroup_t, t: Time) -> io::Result<()> {
        let p = c_path(path)?;
        let mut a = attrs(attr);
        let mut b = libc::timespec { tv_sec: t.secs, tv_nsec: t.nanos as _ };
        let size = std::mem::size_of::<libc::timespec>();
        // SAFETY: the buffer is of `size` bytes
        check(unsafe {
            libc::setattrlist(p.as_ptr(), (&raw mut a).cast(), (&raw mut b).cast(), size, libc::FSOPT_NOFOLLOW)
        })
    }

    #[cfg(target_os = "macos")]
    pub fn set_flags(path: &Path, flags: u32) -> io::Result<()> {
        let p = c_path(path)?;
        // SAFETY: a path of a C string
        check(unsafe { libc::lchflags(p.as_ptr(), flags) })
    }
}

#[cfg(not(unix))]
mod os {
    use std::io;
    use std::path::Path;

    use aqr_formula::aqr::{EntryNode, Extra};

    use super::Kind;

    pub fn set_xattr(_: &Path, _: &[u8], _: &[u8]) -> io::Result<()> {
        Err(io::Error::other("not written on this system"))
    }

    pub fn fork_xattr(_: Kind, _: &[u8]) -> Option<&[u8]> {
        None
    }

    pub fn fork(_: &Path, _: Kind, _: &[u8], _: &[u8]) -> io::Result<()> {
        Err(io::Error::other("not written on this system"))
    }

    pub fn set_mode(_: &Path, _: Kind, _: u32) -> io::Result<()> {
        Ok(())
    }

    pub fn set_times(path: &Path, x: &Extra) -> io::Result<()> {
        let Some(t) = x.mtime else { return Ok(()) };
        let time = match t.secs {
            s if s >= 0 => std::time::UNIX_EPOCH + std::time::Duration::new(s as u64, t.nanos),
            s => {
                std::time::UNIX_EPOCH - std::time::Duration::from_secs(s.unsigned_abs())
                    + std::time::Duration::from_nanos(t.nanos as u64)
            }
        };
        let f = std::fs::File::options().write(!path.is_dir()).read(path.is_dir()).open(path)?;
        f.set_modified(time)
    }

    pub fn make_special(_: &Path, _: &EntryNode) -> io::Result<()> {
        Err(io::Error::other("not made on this system"))
    }
}
